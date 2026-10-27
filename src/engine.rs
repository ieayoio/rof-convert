//! 调用 LibreOffice 无头模式执行实际文档转换。

use std::{
    env,
    fmt::Write as _,
    io::Read,
    path::{Path, PathBuf},
    process::Stdio,
};

use sha2::{Digest, Sha256};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use crate::{config::Config, error::AppError};

#[derive(Debug)]
pub struct ConversionOutcome {
    pub file_path: PathBuf,
    pub size_bytes: u64,
    pub sha256: String,
}

pub async fn convert(
    config: &Config,
    input_path: &Path,
    output_dir: &Path,
    profile_dir: &Path,
    filter: &str,
    target_extension: &str,
    cancel: &CancellationToken,
) -> Result<ConversionOutcome, AppError> {
    ensure_private_dir(output_dir).await.map_err(|error| {
        AppError::internal(format!("failed to prepare output directory: {error}"))
    })?;
    ensure_private_dir(profile_dir).await.map_err(|error| {
        AppError::internal(format!("failed to prepare LibreOffice profile: {error}"))
    })?;

    let profile_url = file_url(profile_dir);
    let mut command = Command::new(&config.soffice_bin);
    command
        .arg("--headless")
        .arg("--invisible")
        .arg("--norestore")
        .arg("--nodefault")
        .arg("--nolockcheck")
        .arg("--nofirststartwizard")
        .arg(format!("-env:UserInstallation={profile_url}"))
        .arg("--convert-to")
        .arg(filter)
        .arg("--outdir")
        .arg(output_dir)
        .arg(input_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    // 每个任务使用独立 UserInstallation，避免 LibreOffice 单实例锁导致并发互斥。
    let output_future = command.output();
    tokio::pin!(output_future);
    let output = tokio::select! {
        result = &mut output_future => result.map_err(|error| {
            AppError::conversion_failed(format!("failed to start LibreOffice: {error}"))
        })?,
        _ = cancel.cancelled() => {
            return Err(AppError::conflict("conversion was cancelled"));
        }
        _ = tokio::time::sleep(config.conversion_timeout) => {
            return Err(AppError::conversion_timeout(config.conversion_timeout.as_secs()));
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::warn!(
            exit_code = output.status.code(),
            input = %input_path.display(),
            stderr = %truncate(stderr.trim(), 2000),
            "LibreOffice conversion failed"
        );
        return Err(AppError::conversion_failed("document conversion failed"));
    }

    let expected_path = expected_output_path(input_path, output_dir, target_extension);
    let file_path = if tokio::fs::metadata(&expected_path)
        .await
        .map(|metadata| metadata.is_file())
        .unwrap_or(false)
    {
        expected_path
    } else {
        find_output(output_dir, target_extension)
            .await
            .ok_or_else(|| {
                tracing::warn!(
                    output_dir = %output_dir.display(),
                    "LibreOffice reported success but no output file was found"
                );
                AppError::conversion_failed("document conversion produced no output")
            })?
    };

    let (size_bytes, sha256) = hash_file(&file_path).await.map_err(|error| {
        AppError::internal(format!("failed to inspect converted document: {error}"))
    })?;
    if size_bytes == 0 {
        return Err(AppError::conversion_failed(
            "document conversion produced an empty file",
        ));
    }

    Ok(ConversionOutcome {
        file_path,
        size_bytes,
        sha256,
    })
}

pub fn file_url(path: &Path) -> String {
    let mut url = String::from("file://");
    for byte in path.to_string_lossy().bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.' | b'~') {
            url.push(byte as char);
        } else {
            let _ = write!(url, "%{byte:02X}");
        }
    }
    url
}

pub fn find_executable(binary: &str) -> Option<PathBuf> {
    let candidate = Path::new(binary);
    if candidate.components().count() > 1 {
        return is_executable(candidate).then(|| candidate.to_path_buf());
    }

    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .map(|directory| directory.join(binary))
        .find(|candidate| is_executable(candidate))
}

async fn ensure_private_dir(path: &Path) -> std::io::Result<()> {
    tokio::fs::create_dir_all(path).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).await?;
    }
    Ok(())
}

fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn expected_output_path(input: &Path, output_dir: &Path, target_extension: &str) -> PathBuf {
    let stem = input
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("output");
    output_dir.join(format!("{stem}.{target_extension}"))
}

async fn find_output(output_dir: &Path, target_extension: &str) -> Option<PathBuf> {
    let mut entries = tokio::fs::read_dir(output_dir).await.ok()?;
    let mut candidates = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        let extension_matches = path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case(target_extension));
        let is_file = entry
            .file_type()
            .await
            .map(|file_type| file_type.is_file())
            .unwrap_or(false);
        if extension_matches && is_file {
            candidates.push(path);
        }
    }
    candidates.sort();
    candidates.into_iter().next()
}

async fn hash_file(path: &Path) -> std::io::Result<(u64, String)> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut file = std::fs::File::open(&path)?;
        let mut hasher = Sha256::new();
        let mut buffer = [0_u8; 64 * 1024];
        let mut total = 0_u64;
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            total += read as u64;
            hasher.update(&buffer[..read]);
        }
        Ok((total, hex::encode(hasher.finalize())))
    })
    .await
    .map_err(|error| std::io::Error::other(format!("hashing task failed: {error}")))?
}

fn truncate(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_valid_file_url() {
        assert_eq!(
            file_url(Path::new("/tmp/rof convert/profile")),
            "file:///tmp/rof%20convert/profile"
        );
        assert_eq!(file_url(Path::new("/tmp/a&b")), "file:///tmp/a%26b");
    }

    #[test]
    fn derives_expected_output_path() {
        assert_eq!(
            expected_output_path(
                Path::new("/tmp/job/input.docx"),
                Path::new("/tmp/job/out"),
                "pdf"
            ),
            PathBuf::from("/tmp/job/out/input.pdf")
        );
    }

    #[test]
    fn resolves_absolute_executable_path() {
        let executable = env::current_exe().expect("test binary path");
        assert_eq!(
            find_executable(executable.to_str().expect("utf-8 path")),
            Some(executable)
        );
        assert!(find_executable("definitely-not-a-real-binary").is_none());
    }
}
