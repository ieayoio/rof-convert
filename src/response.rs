//! 统一响应契约：成功与失败均返回相同的外层结构，便于网关与客户端统一处理。

use axum::{
    Json,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;

use crate::{error::AppError, middleware::RequestId};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiResponse<T> {
    pub success: bool,
    pub code: String,
    pub message: String,
    pub request_id: String,
    pub timestamp: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

impl<T> ApiResponse<T> {
    pub fn success(request_id: &str, data: T) -> Self {
        Self {
            success: true,
            code: "OK".to_string(),
            message: "success".to_string(),
            request_id: request_id.to_string(),
            timestamp: Utc::now(),
            data: Some(data),
            details: None,
        }
    }
}

impl ApiResponse<()> {
    pub fn failure(request_id: &str, error: &AppError) -> Self {
        Self {
            success: false,
            code: error.code.as_str().to_string(),
            message: error.message.clone(),
            request_id: request_id.to_string(),
            timestamp: Utc::now(),
            data: None,
            details: error.details.clone(),
        }
    }
}

/// 请求级响应上下文，保证响应体内的 `requestId` 与响应头一致。
#[derive(Debug, Clone)]
pub struct ApiCtx {
    request_id: String,
}

impl ApiCtx {
    pub fn new(request_id: RequestId) -> Self {
        Self {
            request_id: request_id.0,
        }
    }

    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    pub fn ok<T: Serialize>(&self, data: T) -> Response {
        self.build(StatusCode::OK, data, HeaderMap::new())
    }

    pub fn accepted<T: Serialize>(&self, data: T, headers: HeaderMap) -> Response {
        self.build(StatusCode::ACCEPTED, data, headers)
    }

    pub fn build<T: Serialize>(&self, status: StatusCode, data: T, headers: HeaderMap) -> Response {
        let mut response =
            (status, Json(ApiResponse::success(&self.request_id, data))).into_response();
        let response_headers = response.headers_mut();
        response_headers.extend(headers);
        response_headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        response_headers.insert(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        );
        response
    }

    pub fn fail(&self, error: AppError) -> Response {
        error.into_http_response(&self.request_id)
    }
}
