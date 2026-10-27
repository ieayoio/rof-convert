//! 服务配置：全部通过环境变量注入，便于容器化部署与多环境管理。
//!
//! 可用变量：
//! - `ROF_HOST`（默认 `0.0.0.0`）、`ROF_PORT`（默认 `8080`）
//! - `ROF_MAX_UPLOAD_MB`（默认 `50`）
//! - `ROF_CONVERSION_TIMEOUT_SECS`（默认 `120`）
//! - `ROF_REQUEST_TIMEOUT_SECS`（默认 `300`）
//! - `ROF_MAX_CONCURRENCY`（默认 `2`）、`ROF_MAX_ACTIVE_JOBS`（默认 `100`）
//! - `ROF_JOB_TTL_SECS`（默认 `3600`）
//! - `ROF_TEMP_DIR`（默认系统临时目录下的 `rof-convert`）
//! - `ROF_SOFFICE_BIN`（默认 `soffice`，可传绝对路径）
//! - `ROF_API_KEYS`（逗号分隔；为空表示关闭鉴权）
//! - `ROF_CORS_ORIGINS`（逗号分隔；为空表示不启用 CORS）
//! - `ROF_LOG_FORMAT`（`text` 或 `json`）
//! - `RUST_LOG` 控制日志级别过滤

use std::{
    env,
    fmt::Display,
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    str::FromStr,
    time::Duration,
};

use axum::http::HeaderValue;
use thiserror::Error;

const MIB: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    Text,
    Json,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub bind_addr: SocketAddr,
    pub max_upload_bytes: usize,
    pub conversion_timeout: Duration,
    pub request_timeout: Duration,
    pub max_concurrency: usize,
    pub max_active_jobs: usize,
    pub job_ttl: Duration,
    pub temp_dir: PathBuf,
    pub soffice_bin: String,
    pub api_keys: Vec<String>,
    pub cors_origins: Vec<String>,
    pub log_format: LogFormat,
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        let host: IpAddr = parse_var("ROF_HOST", IpAddr::from([0, 0, 0, 0]))?;
        let port: u16 = parse_var("ROF_PORT", 8080)?;
        let max_upload_mb: usize = parse_var("ROF_MAX_UPLOAD_MB", 50)?;
        let conversion_timeout_secs: u64 = parse_var("ROF_CONVERSION_TIMEOUT_SECS", 120)?;
        let request_timeout_secs: u64 = parse_var("ROF_REQUEST_TIMEOUT_SECS", 300)?;
        let max_concurrency: usize = parse_var("ROF_MAX_CONCURRENCY", 2)?;
        let max_active_jobs: usize = parse_var("ROF_MAX_ACTIVE_JOBS", 100)?;
        let job_ttl_secs: u64 = parse_var("ROF_JOB_TTL_SECS", 3600)?;
        let temp_dir = env_opt("ROF_TEMP_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| env::temp_dir().join("rof-convert"));
        let soffice_bin = env_opt("ROF_SOFFICE_BIN").unwrap_or_else(|| "soffice".to_string());
        let api_keys = parse_api_keys()?;
        let cors_origins = parse_cors_origins()?;
        let log_format = match env_opt("ROF_LOG_FORMAT")
            .unwrap_or_else(|| "text".to_string())
            .to_ascii_lowercase()
            .as_str()
        {
            "text" => LogFormat::Text,
            "json" => LogFormat::Json,
            other => {
                return Err(ConfigError::invalid(
                    "ROF_LOG_FORMAT",
                    other,
                    "expected `text` or `json`",
                ));
            }
        };

        let max_upload_bytes = max_upload_mb
            .checked_mul(MIB)
            .filter(|bytes| *bytes > 0)
            .ok_or_else(|| {
                ConfigError::invalid(
                    "ROF_MAX_UPLOAD_MB",
                    max_upload_mb.to_string(),
                    "must be between 1 and 1024",
                )
            })?;
        if !(1..=1024).contains(&max_upload_mb) {
            return Err(ConfigError::invalid(
                "ROF_MAX_UPLOAD_MB",
                max_upload_mb.to_string(),
                "must be between 1 and 1024",
            ));
        }
        if !(5..=3600).contains(&conversion_timeout_secs) {
            return Err(ConfigError::invalid(
                "ROF_CONVERSION_TIMEOUT_SECS",
                conversion_timeout_secs.to_string(),
                "must be between 5 and 3600",
            ));
        }
        if !(10..=3600).contains(&request_timeout_secs) {
            return Err(ConfigError::invalid(
                "ROF_REQUEST_TIMEOUT_SECS",
                request_timeout_secs.to_string(),
                "must be between 10 and 3600",
            ));
        }
        if !(1..=64).contains(&max_concurrency) {
            return Err(ConfigError::invalid(
                "ROF_MAX_CONCURRENCY",
                max_concurrency.to_string(),
                "must be between 1 and 64",
            ));
        }
        if max_active_jobs < max_concurrency || max_active_jobs > 10_000 {
            return Err(ConfigError::invalid(
                "ROF_MAX_ACTIVE_JOBS",
                max_active_jobs.to_string(),
                "must be between ROF_MAX_CONCURRENCY and 10000",
            ));
        }
        if !(60..=604_800).contains(&job_ttl_secs) {
            return Err(ConfigError::invalid(
                "ROF_JOB_TTL_SECS",
                job_ttl_secs.to_string(),
                "must be between 60 and 604800",
            ));
        }

        Ok(Self {
            bind_addr: SocketAddr::new(host, port),
            max_upload_bytes,
            conversion_timeout: Duration::from_secs(conversion_timeout_secs),
            request_timeout: Duration::from_secs(request_timeout_secs),
            max_concurrency,
            max_active_jobs,
            job_ttl: Duration::from_secs(job_ttl_secs),
            temp_dir,
            soffice_bin,
            api_keys,
            cors_origins,
            log_format,
        })
    }

    pub fn auth_enabled(&self) -> bool {
        !self.api_keys.is_empty()
    }

    pub fn max_upload_mb(&self) -> usize {
        self.max_upload_bytes / MIB
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("invalid configuration `{name}`={value:?}: {reason}")]
    Invalid {
        name: &'static str,
        value: String,
        reason: String,
    },
}

impl ConfigError {
    fn invalid(name: &'static str, value: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::Invalid {
            name,
            value: value.into(),
            reason: reason.into(),
        }
    }
}

fn env_opt(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn parse_var<T>(name: &'static str, default: T) -> Result<T, ConfigError>
where
    T: FromStr,
    T::Err: Display,
{
    match env_opt(name) {
        Some(raw) => raw.parse().map_err(|error| {
            ConfigError::invalid(name, raw, format!("failed to parse value: {error}"))
        }),
        None => Ok(default),
    }
}

fn parse_api_keys() -> Result<Vec<String>, ConfigError> {
    let keys: Vec<String> = env_opt("ROF_API_KEYS")
        .map(|raw| {
            raw.split(',')
                .map(str::trim)
                .filter(|key| !key.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    if keys.len() > 32 {
        return Err(ConfigError::invalid(
            "ROF_API_KEYS",
            format!("{} keys", keys.len()),
            "at most 32 API keys are supported",
        ));
    }
    for key in &keys {
        if HeaderValue::from_str(key).is_err() {
            return Err(ConfigError::invalid(
                "ROF_API_KEYS",
                "<redacted>",
                "API keys must be valid ASCII header values",
            ));
        }
    }
    Ok(keys)
}

fn parse_cors_origins() -> Result<Vec<String>, ConfigError> {
    let origins: Vec<String> = env_opt("ROF_CORS_ORIGINS")
        .map(|raw| {
            raw.split(',')
                .map(str::trim)
                .filter(|origin| !origin.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    if origins.iter().any(|origin| origin == "*") && origins.len() > 1 {
        return Err(ConfigError::invalid(
            "ROF_CORS_ORIGINS",
            origins.join(","),
            "`*` cannot be combined with explicit origins",
        ));
    }
    for origin in origins.iter().filter(|origin| origin.as_str() != "*") {
        HeaderValue::from_str(origin).map_err(|error| {
            ConfigError::invalid(
                "ROF_CORS_ORIGINS",
                origin,
                format!("invalid origin header value: {error}"),
            )
        })?;
    }
    Ok(origins)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid() {
        // 不依赖进程环境变量，仅验证默认值组合可通过校验。
        let config = Config {
            bind_addr: SocketAddr::from(([0, 0, 0, 0], 8080)),
            max_upload_bytes: 50 * MIB,
            conversion_timeout: Duration::from_secs(120),
            request_timeout: Duration::from_secs(300),
            max_concurrency: 2,
            max_active_jobs: 100,
            job_ttl: Duration::from_secs(3600),
            temp_dir: env::temp_dir().join("rof-convert"),
            soffice_bin: "soffice".to_string(),
            api_keys: Vec::new(),
            cors_origins: Vec::new(),
            log_format: LogFormat::Text,
        };
        assert_eq!(config.max_upload_mb(), 50);
        assert!(!config.auth_enabled());
    }
}
