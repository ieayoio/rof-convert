//! HTTP 接口层：格式目录、异步转换任务、结果下载与健康检查。

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use axum::{
    Extension,
    body::Body,
    extract::{
        Multipart, Path as AxumPath, Query, State, multipart::MultipartRejection,
        rejection::QueryRejection,
    },
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use chrono::Utc;
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use serde::Deserialize;
use serde_json::json;
use tokio::io::AsyncWriteExt;
use tokio_util::io::ReaderStream;
use uuid::Uuid;

use crate::{
    engine,
    error::AppError,
    formats::{self, SourceFormat},
    jobs::{Job, JobSpec, JobStatus, JobView},
    middleware::RequestId,
    response::ApiCtx,
    state::AppState,
    worker,
};

pub const MAX_MULTIPART_OVERHEAD: usize = 2 * 1024 * 1024;
const MAX_TEXT_FIELD_BYTES: usize = 64;

const FILENAME_ATTRIBUTE_ENCODE_SET: &percent_encoding::AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateConversionQuery {
    target_format: Option<String>,
}

pub async fn healthz(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
) -> Response {
    ApiCtx::new(request_id).ok(json!({
        "status": "UP",
        "version": env!("CARGO_PKG_VERSION"),
        "uptimeSeconds": (Utc::now() - state.started_at).num_seconds().max(0),
    }))
}

pub async fn readyz(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
) -> Response {
    let ctx = ApiCtx::new(request_id);
    let soffice = engine::find_executable(&state.config.soffice_bin);
    let temp_dir_ready = probe_temp_dir(&state.config.temp_dir).await;
    let checks = json!({
        "soffice": {
            "status": if soffice.is_some() { "UP" } else { "DOWN" },
            "binary": state.config.soffice_bin,
        },
        "tempDirectory": {
            "status": if temp_dir_ready { "UP" } else { "DOWN" },
            "path": state.config.temp_dir.display().to_string(),
        },
    });

    if soffice.is_some() && temp_dir_ready {
        ctx.ok(json!({ "status": "UP", "checks": checks }))
    } else {
        ctx.fail(AppError::service_unavailable("service is not ready").with_details(checks))
    }
}

pub async fn openapi_spec() -> Response {
    (
        [(header::CONTENT_TYPE, "application/json")],
        include_str!("../docs/openapi.json"),
    )
        .into_response()
}

pub async fn list_formats(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
) -> Response {
    ApiCtx::new(request_id).ok(json!({
        "sources": formats::catalog(),
        "limits": {
            "maxUploadBytes": state.config.max_upload_bytes,
            "maxActiveJobs": state.config.max_active_jobs,
            "maxConcurrency": state.config.max_concurrency,
            "conversionTimeoutSeconds": state.config.conversion_timeout.as_secs(),
            "retentionSeconds": state.config.job_ttl.as_secs(),
        },
    }))
}

pub async fn create_conversion(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    query: Result<Query<CreateConversionQuery>, QueryRejection>,
    headers: HeaderMap,
    multipart: Result<Multipart, MultipartRejection>,
) -> Response {
    let ctx = ApiCtx::new(request_id);
    let query = match query {
        Ok(Query(query)) => query,
        Err(rejection) => {
            return ctx.fail(AppError::invalid_argument(format!(
                "invalid query parameters: {rejection}"
            )));
        }
    };

    match create_conversion_inner(&state, ctx.request_id(), query, &headers, multipart).await {
        Ok(view) => {
            let mut response_headers = HeaderMap::new();
            if let Ok(location) = format!("/api/v1/conversions/{}", view.job_id).parse() {
                response_headers.insert(header::LOCATION, location);
            }
            ctx.accepted(view, response_headers)
        }
        Err(error) => {
            if error.code.status().is_server_error() {
                tracing::error!(
                    request_id = ctx.request_id(),
                    code = error.code.as_str(),
                    error = %error.message,
                    "conversion request failed"
                );
            } else {
                tracing::warn!(
                    request_id = ctx.request_id(),
                    code = error.code.as_str(),
                    error = %error.message,
                    "conversion request rejected"
                );
            }
            ctx.fail(error)
        }
    }
}

async fn create_conversion_inner(
    state: &AppState,
    request_id: &str,
    query: CreateConversionQuery,
    headers: &HeaderMap,
    multipart: Result<Multipart, MultipartRejection>,
) -> Result<JobView, AppError> {
    if state.jobs.active_count() >= state.config.max_active_jobs {
        return Err(capacity_error());
    }

    let max_body_bytes = state
        .config
        .max_upload_bytes
        .saturating_add(MAX_MULTIPART_OVERHEAD);
    if let Some(length) = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        && length > max_body_bytes as u64
    {
        return Err(AppError::payload_too_large(state.config.max_upload_bytes));
    }

    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if !content_type
        .to_ascii_lowercase()
        .starts_with("multipart/form-data")
    {
        return Err(AppError::unsupported_media_type(
            "Content-Type must be multipart/form-data",
        ));
    }

    let mut multipart = multipart.map_err(|rejection| {
        if rejection.status() == StatusCode::UNSUPPORTED_MEDIA_TYPE {
            AppError::unsupported_media_type("Content-Type must be multipart/form-data")
        } else {
            AppError::invalid_argument(format!("invalid multipart request: {rejection}"))
        }
    })?;

    let mut target_format = query
        .target_format
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let mut uploaded: Option<UploadedFile> = None;

    while let Some(mut field) = multipart.next_field().await.map_err(|error| {
        AppError::invalid_argument(format!("failed to read multipart body: {error}"))
    })? {
        let field_name = field.name().unwrap_or_default().to_string();
        match field_name.as_str() {
            "file" => {
                if uploaded.is_some() {
                    return Err(AppError::invalid_argument(
                        "only one `file` field is allowed",
                    ));
                }
                let raw_name = field.file_name().map(str::to_string).unwrap_or_default();
                if raw_name.trim().is_empty() {
                    return Err(AppError::invalid_argument(
                        "`file` field must include a filename",
                    ));
                }
                let source_extension =
                    formats::file_extension(&raw_name).ok_or_else(unsupported_source_format)?;
                let source = formats::source_format(&source_extension)
                    .ok_or_else(unsupported_source_format)?;
                let file_name = formats::sanitize_file_name(&raw_name);

                // 目标格式已知时先校验组合，避免为非法请求写入整个文件。
                if let Some(target) = target_format.as_deref() {
                    resolve_conversion(source, target)?;
                }

                let job_id = Uuid::new_v4();
                let work_dir = state.config.temp_dir.join(job_id.to_string());
                let output_dir = work_dir.join("out");
                let input_path = work_dir.join(format!("input.{}", source.extension));
                create_private_dir(&work_dir).await?;
                let guard = WorkDirGuard::new(work_dir.clone());

                let mut output = tokio::fs::File::create(&input_path)
                    .await
                    .map_err(|error| {
                        AppError::internal(format!("failed to persist uploaded file: {error}"))
                    })?;
                let mut size_bytes = 0_u64;
                while let Some(chunk) = field.chunk().await.map_err(|error| {
                    AppError::invalid_argument(format!("failed to read uploaded file: {error}"))
                })? {
                    size_bytes = size_bytes.saturating_add(chunk.len() as u64);
                    if size_bytes > state.config.max_upload_bytes as u64 {
                        return Err(AppError::payload_too_large(state.config.max_upload_bytes));
                    }
                    output.write_all(&chunk).await.map_err(|error| {
                        AppError::internal(format!("failed to persist uploaded file: {error}"))
                    })?;
                }
                output.flush().await.map_err(|error| {
                    AppError::internal(format!("failed to persist uploaded file: {error}"))
                })?;
                drop(output);

                if size_bytes == 0 {
                    return Err(AppError::invalid_argument("uploaded file is empty"));
                }

                uploaded = Some(UploadedFile {
                    job_id,
                    file_name,
                    source,
                    size_bytes,
                    input_path,
                    output_dir,
                    work_dir,
                    guard,
                });
            }
            "targetFormat" => {
                let value = field.text().await.map_err(|error| {
                    AppError::invalid_argument(format!("failed to read `targetFormat`: {error}"))
                })?;
                let value = value.trim().to_string();
                if value.is_empty() {
                    return Err(AppError::invalid_argument(
                        "`targetFormat` must not be empty",
                    ));
                }
                if value.len() > MAX_TEXT_FIELD_BYTES {
                    return Err(AppError::invalid_argument("`targetFormat` is too long"));
                }
                if let Some(existing) = &target_format {
                    if !existing.eq_ignore_ascii_case(&value) {
                        return Err(AppError::invalid_argument(
                            "`targetFormat` is inconsistent between query and form data",
                        ));
                    }
                } else {
                    target_format = Some(value);
                }
            }
            other => {
                return Err(AppError::invalid_argument(format!(
                    "unknown form field `{other}`"
                )));
            }
        }
    }

    let mut uploaded =
        uploaded.ok_or_else(|| AppError::invalid_argument("`file` field is required"))?;
    let target =
        target_format.ok_or_else(|| AppError::invalid_argument("`targetFormat` is required"))?;
    let source = uploaded.source;
    let (target, filter) = resolve_conversion(source, &target)?;

    if state.jobs.active_count() >= state.config.max_active_jobs {
        return Err(capacity_error());
    }

    let job = Arc::new(Job::new(JobSpec {
        id: uploaded.job_id,
        request_id: request_id.to_string(),
        source_file_name: uploaded.file_name.clone(),
        source_format: source.extension,
        source_size_bytes: uploaded.size_bytes,
        target_format: target,
        filter,
        input_path: uploaded.input_path.clone(),
        output_dir: uploaded.output_dir.clone(),
        work_dir: uploaded.work_dir.clone(),
    }));
    let view = job.view(state.config.job_ttl);

    uploaded.guard.disarm();
    state.jobs.insert(job.clone());
    worker::spawn(state.clone(), job);

    Ok(view)
}

pub async fn get_conversion(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    AxumPath(job_id): AxumPath<String>,
) -> Response {
    let ctx = ApiCtx::new(request_id);
    let id = match parse_job_id(&job_id) {
        Ok(id) => id,
        Err(error) => return ctx.fail(error),
    };
    match state.jobs.get(id) {
        Some(job) => ctx.ok(job.view(state.config.job_ttl)),
        None => ctx.fail(job_not_found(&job_id)),
    }
}

pub async fn delete_conversion(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    AxumPath(job_id): AxumPath<String>,
) -> Response {
    let ctx = ApiCtx::new(request_id);
    let id = match parse_job_id(&job_id) {
        Ok(id) => id,
        Err(error) => return ctx.fail(error),
    };
    let Some(job) = state.jobs.get(id) else {
        return ctx.fail(job_not_found(&job_id));
    };
    // 先取消执行，再移除记录，缩小任务在删除过程中完成的竞态窗口。
    job.cancel.cancel();
    let _ = state.jobs.remove(id);
    let work_dir = job.work_dir.clone();
    tokio::spawn(async move {
        worker::cleanup_work_dir(&work_dir).await;
    });
    ctx.ok(json!({ "jobId": id.to_string(), "deleted": true }))
}

pub async fn download_conversion(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    AxumPath(job_id): AxumPath<String>,
) -> Response {
    let ctx = ApiCtx::new(request_id);
    let id = match parse_job_id(&job_id) {
        Ok(id) => id,
        Err(error) => return ctx.fail(error),
    };
    let Some(job) = state.jobs.get(id) else {
        return ctx.fail(job_not_found(&job_id));
    };

    let snapshot = job.state();
    let result = match snapshot.status {
        JobStatus::Succeeded => match snapshot.result {
            Some(result) => result,
            None => {
                tracing::error!(job_id = %job.id, "succeeded job has no result metadata");
                return ctx.fail(AppError::internal("converted document is not available"));
            }
        },
        JobStatus::Failed => {
            let failure = snapshot.failure;
            let mut error = AppError::conflict("conversion job has failed");
            if let Some(failure) = failure {
                error = error.with_details(json!({
                    "code": failure.code,
                    "message": failure.message,
                }));
            }
            return ctx.fail(error);
        }
        JobStatus::Queued | JobStatus::Running => {
            return ctx
                .fail(AppError::conflict("conversion is not completed yet").with_retry_after(1));
        }
    };

    let file = match tokio::fs::File::open(&result.file_path).await {
        Ok(file) => file,
        Err(error) => {
            tracing::error!(
                job_id = %job.id,
                path = %result.file_path.display(),
                error = %error,
                "failed to open converted document"
            );
            return ctx.fail(AppError::internal(
                "converted document is no longer available",
            ));
        }
    };

    let disposition = format!(
        "attachment; filename=\"{}\"; filename*=UTF-8''{}",
        formats::ascii_file_name(&result.file_name),
        utf8_percent_encode(&result.file_name, FILENAME_ATTRIBUTE_ENCODE_SET),
    );
    let mut response = Response::new(Body::from_stream(ReaderStream::new(file)));
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&result.content_type)
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    if let Ok(value) = result.size_bytes.to_string().parse() {
        headers.insert(header::CONTENT_LENGTH, value);
    }
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&disposition)
            .unwrap_or_else(|_| HeaderValue::from_static("attachment")),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    if let Ok(value) = result.sha256.parse() {
        headers.insert("x-checksum-sha256", value);
    }
    response
}

pub async fn not_found(Extension(request_id): Extension<RequestId>) -> Response {
    ApiCtx::new(request_id).fail(AppError::not_found("the requested resource was not found"))
}

pub async fn method_not_allowed(Extension(request_id): Extension<RequestId>) -> Response {
    ApiCtx::new(request_id).fail(AppError::method_not_allowed(
        "the HTTP method is not allowed for this resource",
    ))
}

async fn probe_temp_dir(path: &Path) -> bool {
    let probe = path.join(format!(".probe-{}", Uuid::new_v4()));
    match tokio::fs::write(&probe, b"ok").await {
        Ok(()) => {
            let _ = tokio::fs::remove_file(&probe).await;
            true
        }
        Err(error) => {
            tracing::warn!(
                path = %path.display(),
                error = %error,
                "temporary directory is not writable"
            );
            false
        }
    }
}

fn resolve_conversion(
    source: &SourceFormat,
    raw_target: &str,
) -> Result<(String, &'static str), AppError> {
    let target = normalize_target(raw_target)?;
    if target == source.extension {
        return Err(AppError::invalid_argument(
            "`targetFormat` must differ from the source format",
        ));
    }
    let supported = formats::supported_targets(source);
    if !formats::supports(source.extension, &target) {
        return Err(AppError::unsupported_conversion(
            source.extension,
            &target,
            &supported,
        ));
    }
    let filter = formats::output_filter(source.family, &target)
        .ok_or_else(|| AppError::unsupported_conversion(source.extension, &target, &supported))?;
    Ok((target, filter))
}

fn normalize_target(raw: &str) -> Result<String, AppError> {
    let target = raw.trim().trim_start_matches('.').to_ascii_lowercase();
    if target.is_empty()
        || target.len() > 16
        || !target
            .chars()
            .all(|character| character.is_ascii_alphanumeric())
    {
        return Err(AppError::invalid_argument(
            "`targetFormat` must be a format extension such as `pdf`",
        ));
    }
    Ok(target)
}

fn unsupported_source_format() -> AppError {
    let supported: Vec<&str> = formats::SOURCE_FORMATS
        .iter()
        .map(|format| format.extension)
        .collect();
    AppError::unsupported_media_type("uploaded file has an unsupported extension").with_details(
        json!({
            "supportedSourceFormats": supported,
        }),
    )
}

fn capacity_error() -> AppError {
    AppError::service_unavailable("conversion capacity is exhausted, please retry later")
        .with_retry_after(5)
}

fn parse_job_id(raw: &str) -> Result<Uuid, AppError> {
    Uuid::parse_str(raw).map_err(|_| AppError::invalid_argument("jobId must be a valid UUID"))
}

fn job_not_found(raw: &str) -> AppError {
    AppError::not_found(format!("conversion job `{raw}` was not found"))
}

async fn create_private_dir(path: &Path) -> Result<(), AppError> {
    tokio::fs::create_dir_all(path)
        .await
        .map_err(|error| AppError::internal(format!("failed to prepare job workspace: {error}")))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .await
            .map_err(|error| {
                AppError::internal(format!("failed to secure job workspace: {error}"))
            })?;
    }
    Ok(())
}

struct UploadedFile {
    job_id: Uuid,
    file_name: String,
    source: &'static SourceFormat,
    size_bytes: u64,
    input_path: PathBuf,
    output_dir: PathBuf,
    work_dir: PathBuf,
    guard: WorkDirGuard,
}

struct WorkDirGuard {
    path: PathBuf,
    armed: bool,
}

impl WorkDirGuard {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for WorkDirGuard {
    fn drop(&mut self) {
        if self.armed
            && let Err(error) = std::fs::remove_dir_all(&self.path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(
                path = %self.path.display(),
                error = %error,
                "failed to clean up aborted upload"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_target_format() {
        assert_eq!(normalize_target(" PDF ").unwrap(), "pdf");
        assert_eq!(normalize_target(".docx").unwrap(), "docx");
        assert!(normalize_target("").is_err());
        assert!(normalize_target("pdf/../../etc").is_err());
        assert!(normalize_target("a".repeat(17).as_str()).is_err());
    }

    #[test]
    fn validates_job_ids() {
        let id = Uuid::new_v4();
        assert_eq!(parse_job_id(&id.to_string()).unwrap(), id);
        assert!(parse_job_id("not-a-uuid").is_err());
    }
}
