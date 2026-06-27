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
use tower::ServiceBuilder;

use crate::telemetry::ObservabilityLayer;

pub use backend::{Backend, BackendKind};
pub use routing::{BackendRoute, ProxyClient, build_proxy_client};

use metrics_exporter_prometheus::PrometheusHandle;

use crate::proxy::routing::{StripHopByHopLayer, proxy_request};

/// Shared, process-lifetime state handed to every request. This is the seam
/// where control-plane stores (health, sessions, rate limiter) will attach as
/// MCP awareness is layered on top of the reverse proxy.
pub struct AppState {
    pub client: Arc<ProxyClient>,
    pub registry: Vec<Arc<Backend>>,
    pub metrics: PrometheusHandle,
}

/// Build the router from the live registry. Routes are derived at runtime from
/// `state.registry`; each backend dispatches by `BackendKind`.
pub fn entrypoint(state: AppState) -> Router {
    let metrics_handle = state.metrics.clone();
    let mut router = Router::new()
        .route("/health", get(|| async { (StatusCode::OK, "Router is healthy!") }))
        .route("/metrics", get(move || async move {
            (
                [(http::header::CONTENT_TYPE, "text/plain; version=0.0.4")],
                metrics_handle.render(),
            )
        }));

    for backend in &state.registry {
        match &backend.kind {
            BackendKind::HttpPassthrough { lb } => {
                let lb = Arc::clone(lb);
                let client = Arc::clone(&state.client);
                let name = backend.name.clone();
                let route = format!("{}/*path", backend.prefix);
                router = router.route(
                    &route,
                    any(
                        move |peer: ConnectInfo<SocketAddr>, req: Request| async move {
                            proxy_request(peer, req, lb, client, name).await
                        },
                    ),
                );
            }
        }
    }

    // Observability is composed *outermost* so it times the full handling
    // (including hop-by-hop stripping). Both run via `Router::layer`, i.e. after
    // routing, so `MatchedPath` is populated for the `route` label/field.
    router.fallback(not_found).layer(
        ServiceBuilder::new()
            .layer(ObservabilityLayer::new())
            .layer(StripHopByHopLayer),
    )
}

async fn not_found() -> impl IntoResponse {
    StatusCode::NOT_FOUND
}
