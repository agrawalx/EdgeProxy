pub mod routing;
use axum::response::IntoResponse;
use axum::{Router, routing::get};
use http::StatusCode;
pub fn entrypoint() -> Router {
    Router::new()
        .route("/health", get(|| async { StatusCode::OK }))
        .merge(routing::router())
        .fallback(not_found)
}

async fn not_found() -> impl IntoResponse {
    StatusCode::NOT_FOUND
}
