//! HTTP 横切能力：请求追踪、鉴权与超时保护。

use std::{panic::AssertUnwindSafe, time::Instant};

use axum::{
    extract::{Request, State},
    http::{HeaderMap, HeaderValue, header},
    middleware::Next,
    response::Response,
};
use futures_util::FutureExt;
use subtle::ConstantTimeEq;
use tracing::Instrument;
use uuid::Uuid;

use crate::{error::AppError, state::AppState};

#[derive(Debug, Clone)]
pub struct RequestId(pub String);

/// 为每个请求分配/透传 `X-Request-Id`，并输出结构化访问日志。
/// 同时兜底 panic，保证客户端始终收到统一错误结构。
pub async fn request_context(mut request: Request, next: Next) -> Response {
    let request_id = request
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .filter(|value| is_valid_request_id(value))
        .map(str::to_string)
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    request
        .extensions_mut()
        .insert(RequestId(request_id.clone()));

    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let started = Instant::now();
    let span = tracing::info_span!(
        "http_request",
        request_id = %request_id,
        method = %method,
        path = %path,
    );

    let response_request_id = request_id.clone();
    async move {
        let outcome = AssertUnwindSafe(async move { next.run(request).await })
            .catch_unwind()
            .await;

        let mut response = match outcome {
            Ok(response) => response,
            Err(_) => {
                tracing::error!("request handler panicked");
                AppError::internal("internal server error").into_http_response(&response_request_id)
            }
        };

        if let Ok(value) = HeaderValue::from_str(&response_request_id) {
            response.headers_mut().insert("x-request-id", value);
        }
        let status = response.status();
        let latency_ms = started.elapsed().as_millis() as u64;
        if status.is_server_error() {
            tracing::error!(
                status = status.as_u16(),
                latency_ms,
                "request completed with server error"
            );
        } else if status.is_client_error() {
            tracing::warn!(
                status = status.as_u16(),
                latency_ms,
                "request completed with client error"
            );
        } else {
            tracing::info!(status = status.as_u16(), latency_ms, "request completed");
        }
        response
    }
    .instrument(span)
    .await
}

fn is_valid_request_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.' | ':')
        })
}

/// API Key 鉴权：支持 `X-API-Key` 与 `Authorization: Bearer <token>`。
/// 未配置 `ROF_API_KEYS` 时显式关闭。
pub async fn require_api_key(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    if !state.config.auth_enabled() {
        return next.run(request).await;
    }

    let authorized = provided_api_key(request.headers()).is_some_and(|provided| {
        state
            .config
            .api_keys
            .iter()
            .any(|expected| constant_time_eq(&provided, expected))
    });

    if authorized {
        next.run(request).await
    } else {
        let request_id = request
            .extensions()
            .get::<RequestId>()
            .map(|id| id.0.clone())
            .unwrap_or_default();
        tracing::warn!(
            path = request.uri().path(),
            "request rejected due to missing or invalid API key"
        );
        AppError::unauthorized("a valid API key is required").into_http_response(&request_id)
    }
}

/// 请求级超时：与转换任务超时相互独立，避免慢上传长期占用连接。
pub async fn enforce_timeout(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let request_id = request
        .extensions()
        .get::<RequestId>()
        .map(|id| id.0.clone())
        .unwrap_or_default();
    let timeout = state.config.request_timeout;
    match tokio::time::timeout(timeout, next.run(request)).await {
        Ok(response) => response,
        Err(_) => {
            tracing::warn!(
                request_id = request_id,
                timeout_secs = timeout.as_secs(),
                "request timed out"
            );
            AppError::request_timeout().into_http_response(&request_id)
        }
    }
}

fn provided_api_key(headers: &HeaderMap) -> Option<String> {
    if let Some(value) = headers
        .get("x-api-key")
        .and_then(|value| value.to_str().ok())
    {
        let value = value.trim();
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }

    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let token = token.trim();
    (!token.is_empty()).then(|| token.to_string())
}

fn constant_time_eq(provided: &str, expected: &str) -> bool {
    let provided = provided.as_bytes();
    let expected = expected.as_bytes();
    if provided.len() != expected.len() {
        return false;
    }
    provided.ct_eq(expected).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_request_ids() {
        assert!(is_valid_request_id("req-1"));
        assert!(is_valid_request_id("9f8e7d6c-1234"));
        assert!(!is_valid_request_id(""));
        assert!(!is_valid_request_id("bad id"));
        assert!(!is_valid_request_id("bad/id"));
        assert!(!is_valid_request_id(&"a".repeat(129)));
    }

    #[test]
    fn compares_api_keys_in_constant_time() {
        assert!(constant_time_eq("secret", "secret"));
        assert!(!constant_time_eq("secret", "secreT"));
        assert!(!constant_time_eq("secret", "secret2"));
    }

    #[test]
    fn extracts_bearer_and_api_key_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("x-api-key", HeaderValue::from_static("key-a"));
        assert_eq!(provided_api_key(&headers).as_deref(), Some("key-a"));

        headers.remove("x-api-key");
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer key-b"),
        );
        assert_eq!(provided_api_key(&headers).as_deref(), Some("key-b"));

        headers.insert(header::AUTHORIZATION, HeaderValue::from_static("Basic abc"));
        assert_eq!(provided_api_key(&headers), None);
    }
}
