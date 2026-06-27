pub mod backend;
pub mod lb;
pub mod routing;

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::extract::{ConnectInfo, Request};
use axum::response::IntoResponse;
use axum::routing::{any, get};
use http::StatusCode;

pub use backend::{Backend, BackendKind};
pub use routing::{ProxyClient, build_proxy_client};

use crate::proxy::routing::{StripHopByHopLayer, proxy_request};

/// Shared, process-lifetime state handed to every request. This is the seam
/// where control-plane stores (health, sessions, rate limiter) will attach as
/// MCP awareness is layered on top of the reverse proxy.
pub struct AppState {
    pub client: Arc<ProxyClient>,
    pub registry: Vec<Arc<Backend>>,
}

/// Build the router from the live registry. Routes are derived at runtime from
/// `state.registry`; each backend dispatches by `BackendKind`.
pub fn entrypoint(state: AppState) -> Router {
    let mut router = Router::new().route(
        "/health",
        get(|| async { (StatusCode::OK, "Router is healthy!") }),
    );

    for backend in &state.registry {
        match &backend.kind {
            BackendKind::HttpPassthrough { lb } => {
                let lb = Arc::clone(lb);
                let client = Arc::clone(&state.client);
                let route = format!("{}/*path", backend.prefix);
                router = router.route(
                    &route,
                    any(
                        move |peer: ConnectInfo<SocketAddr>, req: Request| async move {
                            proxy_request(peer, req, lb, client).await
                        },
                    ),
                );
            }
        }
    }

    router.fallback(not_found).layer(StripHopByHopLayer)
}

async fn not_found() -> impl IntoResponse {
    StatusCode::NOT_FOUND
}
