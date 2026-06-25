use crate::config::BACKENDS;
use axum::Router;
use axum::response::IntoResponse;
use axum::routing::any;
use http::status::StatusCode;
pub fn router() -> Router {
    BACKENDS
        .iter()
        .fold(Router::new(), |r, (_name, prefix, _backend_replicas)| {
            r.route(&format!("{prefix}/*path"), any(proxy_request))
        })
}

pub async fn proxy_request() -> impl IntoResponse {
    (StatusCode::NOT_IMPLEMENTED, "proxy received")
}
