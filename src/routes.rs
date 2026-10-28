//! 路由装配：公开探针/文档与受保护的业务接口分层。
//!
//! Swagger UI 与 OpenAPI 契约仅在 `docs.enabled`（`ROF_DOCS_ENABLED`）开启时注册，
//! 生产环境默认关闭，不会暴露任何文档路径。

use std::time::Duration;

use axum::{
    Router,
    extract::DefaultBodyLimit,
    http::{HeaderName, HeaderValue, Method, header},
    middleware,
    routing::{get, post},
};
use tower_http::cors::{AllowOrigin, Any, CorsLayer};

use crate::{api, config::Config, middleware as app_middleware, state::AppState};

/// Swagger UI 挂载路径（相对 `/api/v1`）。
pub const SWAGGER_UI_PATH: &str = "/docs";
/// OpenAPI 契约路径（相对 `/api/v1`）。
pub const OPENAPI_SPEC_PATH: &str = "/openapi.json";

pub fn build_router(state: AppState) -> Router {
    let config = state.config.clone();
    let max_body_bytes = config
        .max_upload_bytes
        .saturating_add(api::MAX_MULTIPART_OVERHEAD);

    let mut public_api = Router::new();
    if config.docs_enabled {
        public_api = public_api
            .route(OPENAPI_SPEC_PATH, get(api::openapi_spec))
            .merge(swagger_ui());
    }
    let protected_api = Router::new()
        .route("/formats", get(api::list_formats))
        .route("/conversions", post(api::create_conversion))
        .route("/conversions/sync", post(api::create_conversion_sync))
        .route(
            "/conversions/{job_id}",
            get(api::get_conversion).delete(api::delete_conversion),
        )
        .route(
            "/conversions/{job_id}/download",
            get(api::download_conversion),
        )
        .method_not_allowed_fallback(api::method_not_allowed)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            app_middleware::require_api_key,
        ))
        .layer(DefaultBodyLimit::max(max_body_bytes));

    let api = public_api.merge(protected_api);

    let mut router = Router::new()
        .route("/healthz", get(api::healthz))
        .route("/readyz", get(api::readyz))
        .nest("/api/v1", api)
        .fallback(api::not_found)
        .method_not_allowed_fallback(api::method_not_allowed)
        .with_state(state.clone())
        .layer(middleware::from_fn_with_state(
            state.clone(),
            app_middleware::enforce_timeout,
        ))
        .layer(middleware::from_fn(app_middleware::request_context));

    if let Some(cors) = cors_layer(&config) {
        router = router.layer(cors);
    }
    router
}

/// Swagger UI 页面：只渲染界面，契约由 `api::openapi_spec` 统一提供。
fn swagger_ui() -> Router<AppState> {
    // 浏览器以页面地址为基准解析该地址，因此必须写全 `/api/v1` 前缀。
    let ui_config = utoipa_swagger_ui::Config::new([utoipa_swagger_ui::Url::new(
        "rof-convert API",
        "/api/v1/openapi.json",
    )]);

    utoipa_swagger_ui::SwaggerUi::new(SWAGGER_UI_PATH)
        .config(ui_config)
        .into()
}

fn cors_layer(config: &Config) -> Option<CorsLayer> {
    if config.cors_origins.is_empty() {
        return None;
    }

    let cors = CorsLayer::new()
        .allow_methods([Method::GET, Method::POST, Method::DELETE, Method::OPTIONS])
        .allow_headers([
            header::ACCEPT,
            header::AUTHORIZATION,
            header::CONTENT_TYPE,
            HeaderName::from_static("x-api-key"),
            HeaderName::from_static("x-request-id"),
        ])
        .expose_headers([
            header::CONTENT_DISPOSITION,
            HeaderName::from_static("x-checksum-sha256"),
            HeaderName::from_static("x-request-id"),
        ])
        .max_age(Duration::from_secs(600));

    if config.cors_origins.iter().any(|origin| origin == "*") {
        Some(cors.allow_origin(Any))
    } else {
        let origins: Vec<HeaderValue> = config
            .cors_origins
            .iter()
            .filter_map(|origin| HeaderValue::from_str(origin).ok())
            .collect();
        Some(cors.allow_origin(AllowOrigin::list(origins)))
    }
}
