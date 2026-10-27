//! 路由装配：公开探针/文档与受保护的业务接口分层。

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

pub fn build_router(state: AppState) -> Router {
    let config = state.config.clone();
    let max_body_bytes = config
        .max_upload_bytes
        .saturating_add(api::MAX_MULTIPART_OVERHEAD);

    let public_api = Router::new().route("/openapi.json", get(api::openapi_spec));
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
