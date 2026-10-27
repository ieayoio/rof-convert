//! 领域错误模型：稳定的业务错误码 + 恰当的 HTTP 状态码。

use axum::{
    Json,
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};

use crate::response::ApiResponse;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    InvalidArgument,
    Unauthorized,
    NotFound,
    PayloadTooLarge,
    UnsupportedMediaType,
    UnsupportedConversion,
    Conflict,
    MethodNotAllowed,
    RequestTimeout,
    ConversionFailed,
    ConversionTimeout,
    ServiceUnavailable,
    Internal,
}

impl ErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidArgument => "INVALID_ARGUMENT",
            Self::Unauthorized => "UNAUTHORIZED",
            Self::NotFound => "NOT_FOUND",
            Self::PayloadTooLarge => "PAYLOAD_TOO_LARGE",
            Self::UnsupportedMediaType => "UNSUPPORTED_MEDIA_TYPE",
            Self::UnsupportedConversion => "UNSUPPORTED_CONVERSION",
            Self::Conflict => "CONFLICT",
            Self::MethodNotAllowed => "METHOD_NOT_ALLOWED",
            Self::RequestTimeout => "REQUEST_TIMEOUT",
            Self::ConversionFailed => "CONVERSION_FAILED",
            Self::ConversionTimeout => "CONVERSION_TIMEOUT",
            Self::ServiceUnavailable => "SERVICE_UNAVAILABLE",
            Self::Internal => "INTERNAL_ERROR",
        }
    }

    pub const fn status(self) -> StatusCode {
        match self {
            Self::InvalidArgument => StatusCode::BAD_REQUEST,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::UnsupportedMediaType => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Self::UnsupportedConversion => StatusCode::UNPROCESSABLE_ENTITY,
            Self::Conflict => StatusCode::CONFLICT,
            Self::MethodNotAllowed => StatusCode::METHOD_NOT_ALLOWED,
            Self::RequestTimeout => StatusCode::REQUEST_TIMEOUT,
            Self::ConversionFailed | Self::Internal => StatusCode::INTERNAL_SERVER_ERROR,
            Self::ConversionTimeout => StatusCode::GATEWAY_TIMEOUT,
            Self::ServiceUnavailable => StatusCode::SERVICE_UNAVAILABLE,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AppError {
    pub code: ErrorCode,
    pub message: String,
    pub details: Option<Value>,
    pub retry_after_secs: Option<u64>,
}

impl AppError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: None,
            retry_after_secs: None,
        }
    }

    pub fn invalid_argument(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidArgument, message)
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Unauthorized, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::NotFound, message)
    }

    pub fn payload_too_large(limit_bytes: usize) -> Self {
        let limit_mb = limit_bytes / (1024 * 1024);
        Self::new(
            ErrorCode::PayloadTooLarge,
            format!("uploaded file exceeds the {limit_mb} MB limit"),
        )
        .with_details(json!({ "maxUploadBytes": limit_bytes }))
    }

    pub fn unsupported_media_type(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::UnsupportedMediaType, message)
    }

    pub fn unsupported_conversion(source: &str, target: &str, supported_targets: &[&str]) -> Self {
        Self::new(
            ErrorCode::UnsupportedConversion,
            format!("conversion from `{source}` to `{target}` is not supported"),
        )
        .with_details(json!({
            "sourceFormat": source,
            "targetFormat": target,
            "supportedTargets": supported_targets,
        }))
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Conflict, message)
    }

    pub fn method_not_allowed(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::MethodNotAllowed, message)
    }

    pub fn request_timeout() -> Self {
        Self::new(ErrorCode::RequestTimeout, "request processing timed out")
    }

    pub fn conversion_failed(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::ConversionFailed, message)
    }

    pub fn conversion_timeout(timeout_secs: u64) -> Self {
        Self::new(
            ErrorCode::ConversionTimeout,
            format!("document conversion timed out after {timeout_secs} seconds"),
        )
    }

    pub fn service_unavailable(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::ServiceUnavailable, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Internal, message)
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }

    pub fn with_retry_after(mut self, seconds: u64) -> Self {
        self.retry_after_secs = Some(seconds);
        self
    }

    pub fn into_http_response(self, request_id: &str) -> Response {
        let status = self.code.status();
        let mut response = (status, Json(ApiResponse::failure(request_id, &self))).into_response();
        let headers = response.headers_mut();
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        headers.insert(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        );
        if let Some(seconds) = self.retry_after_secs
            && let Ok(value) = seconds.to_string().parse()
        {
            headers.insert(header::RETRY_AFTER, value);
        }
        if self.code == ErrorCode::Unauthorized {
            headers.insert(
                header::WWW_AUTHENTICATE,
                HeaderValue::from_static("Bearer realm=\"rof-convert\""),
            );
        }
        response
    }
}
