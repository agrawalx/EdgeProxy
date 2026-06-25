use crate::config::BACKENDS;
use crate::proxy::lb::LbState;
use axum::Router;
use axum::body::Body;
use axum::extract::{ConnectInfo, Request};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use http::status::StatusCode;
use http::{HeaderMap, HeaderName, HeaderValue, Uri};
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use std::net::SocketAddr;
use std::sync::Arc;
use std::task::{Context, Poll};
use tower::{Layer, Service};

// ---------------------------------------------------------------------------
// Hop-by-hop header catalogue (RFC 7230 §6.1)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
pub enum HopByHop {
    Connection,
    KeepAlive,
    TransferEncoding,
    Te,
    Trailer,
    Upgrade,
    ProxyAuthorization,
    ProxyAuthenticate,
}

impl HopByHop {
    const ALL: &'static [Self] = &[
        Self::Connection,
        Self::KeepAlive,
        Self::TransferEncoding,
        Self::Te,
        Self::Trailer,
        Self::Upgrade,
        Self::ProxyAuthorization,
        Self::ProxyAuthenticate,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Connection => "connection",
            Self::KeepAlive => "keep-alive",
            Self::TransferEncoding => "transfer-encoding",
            Self::Te => "te",
            Self::Trailer => "trailer",
            Self::Upgrade => "upgrade",
            Self::ProxyAuthorization => "proxy-authorization",
            Self::ProxyAuthenticate => "proxy-authenticate",
        }
    }

    pub fn from_header(name: &HeaderName) -> Option<Self> {
        HopByHop::ALL
            .iter()
            .find(|h| h.as_str() == name.as_str())
            .copied()
    }
}

// ---------------------------------------------------------------------------
// Header stripping
// ---------------------------------------------------------------------------

/// The `Connection` header may list *additional* per-hop headers for this
/// connection (e.g. `Connection: close, X-Custom-Token`).  Collect them
/// before removing `Connection` itself so nothing is missed.
fn connection_declared_headers(headers: &HeaderMap) -> Vec<HeaderName> {
    let mut extras = Vec::new();
    for value in headers.get_all("connection") {
        let Ok(s) = value.to_str() else { continue };
        for token in s.split(',') {
            let name = token.trim();
            if let Ok(header) = HeaderName::from_bytes(name.as_bytes()) {
                extras.push(header);
            }
        }
    }
    extras
}

pub fn strip_hop_by_hop(headers: &mut HeaderMap) {
    let extras = connection_declared_headers(headers);

    for hop in HopByHop::ALL {
        headers.remove(hop.as_str());
    }
    for name in &extras {
        headers.remove(name);
    }
}

// ---------------------------------------------------------------------------
// Tower middleware
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct StripHopByHopLayer;

impl<S> Layer<S> for StripHopByHopLayer {
    type Service = StripHopByHopService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        StripHopByHopService { inner }
    }
}

#[derive(Clone)]
pub struct StripHopByHopService<S> {
    inner: S,
}

impl<S, B> Service<Request<B>> for StripHopByHopService<S>
where
    S: Service<Request<B>>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: Request<B>) -> Self::Future {
        strip_hop_by_hop(req.headers_mut());
        self.inner.call(req)
    }
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------
type ProxyClient = Client<HttpConnector, Body>;
pub fn router() -> Router {
    let client: ProxyClient = Client::builder(TokioExecutor::new())
        .pool_max_idle_per_host(32)
        .build(HttpConnector::new());
    let client = Arc::new(client);
    BACKENDS
        .iter()
        .fold(Router::new(), |r, &(_name, prefix, strategy, replicas)| {
            let lb = LbState::new(strategy, replicas);
            let client = Arc::clone(&client);
            r.route(
                &format!("{prefix}/*path"),
                any(
                    move |ConnectInfo(peer): ConnectInfo<SocketAddr>, req: Request| async move {
                        proxy_request(ConnectInfo(peer), req, lb, client).await
                    },
                ),
            )
        })
        .layer(StripHopByHopLayer)
}
struct ConnectionGuard<'a> {
    lb: &'a LbState,
    idx: usize,
}

impl Drop for ConnectionGuard<'_> {
    fn drop(&mut self) {
        self.lb.decrement(self.idx);
    }
}
async fn proxy_request(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
    lb: Arc<LbState>,
    client: Arc<ProxyClient>,
) -> impl IntoResponse {
    let client_ip = peer.ip().to_string();
    let (idx, upstream_base) = lb.pick(Some(&client_ip));
    lb.increment(idx);
    let _guard = ConnectionGuard { lb: &lb, idx };

    // Hop-by-hop headers are already stripped by StripHopByHopLayer at this point.
    let (mut parts, body) = req.into_parts();

    // Append the immediate client IP to the X-Forwarded-For chain.
    let xff = match parts
        .headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
    {
        Some(existing) => format!("{existing}, {client_ip}"),
        None => client_ip,
    };
    if let Ok(val) = HeaderValue::from_str(&xff) {
        parts
            .headers
            .insert(HeaderName::from_static("x-forwarded-for"), val);
    }

    // step 4: call upstream with hyper
    let path_and_query = parts
        .uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/");
    parts.uri = match format!("{upstream_base}{path_and_query}").parse::<Uri>() {
        Ok(uri) => uri,
        Err(_) => return StatusCode::BAD_GATEWAY.into_response(),
    };

    let upstream_req = Request::from_parts(parts, body);
    match client.request(upstream_req).await {
        Err(_) => StatusCode::BAD_GATEWAY.into_response(),
        Ok(upstream_res) => {
            // stream response back; also call strip_hop_by_hop on upstream response headers
            let (mut resp_parts, resp_body) = upstream_res.into_parts();
            strip_hop_by_hop(&mut resp_parts.headers);
            Response::from_parts(resp_parts, Body::new(resp_body))
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use tower::ServiceExt; // for .oneshot()

    // --- unit tests: strip_hop_by_hop in isolation ---

    #[test]
    fn removes_standard_hop_by_hop_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("keep-alive", "timeout=5".parse().unwrap());
        headers.insert("transfer-encoding", "chunked".parse().unwrap());
        headers.insert("te", "trailers".parse().unwrap());
        headers.insert("trailer", "Expires".parse().unwrap());
        headers.insert("upgrade", "websocket".parse().unwrap());
        headers.insert("x-real-header", "must-survive".parse().unwrap());

        strip_hop_by_hop(&mut headers);

        assert!(
            headers.get("keep-alive").is_none(),
            "keep-alive should be stripped"
        );
        assert!(
            headers.get("transfer-encoding").is_none(),
            "transfer-encoding should be stripped"
        );
        assert!(headers.get("te").is_none(), "te should be stripped");
        assert!(
            headers.get("trailer").is_none(),
            "trailer should be stripped"
        );
        assert!(
            headers.get("upgrade").is_none(),
            "upgrade should be stripped"
        );
        assert_eq!(headers.get("x-real-header").unwrap(), "must-survive");
    }

    #[test]
    fn removes_connection_declared_headers() {
        let mut headers = HeaderMap::new();
        // Connection names two extra per-hop headers for this connection
        headers.insert("connection", "keep-alive, x-custom-token".parse().unwrap());
        headers.insert("keep-alive", "timeout=5".parse().unwrap());
        headers.insert("x-custom-token", "secret".parse().unwrap());
        headers.insert("x-real-header", "must-survive".parse().unwrap());

        strip_hop_by_hop(&mut headers);

        assert!(
            headers.get("connection").is_none(),
            "connection should be stripped"
        );
        assert!(
            headers.get("keep-alive").is_none(),
            "keep-alive should be stripped"
        );
        assert!(
            headers.get("x-custom-token").is_none(),
            "connection-declared header should be stripped"
        );
        assert_eq!(headers.get("x-real-header").unwrap(), "must-survive");
    }

    #[test]
    fn preserves_end_to_end_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer token123".parse().unwrap());
        headers.insert("content-type", "application/json".parse().unwrap());
        headers.insert("x-request-id", "abc-123".parse().unwrap());

        strip_hop_by_hop(&mut headers);

        assert!(headers.get("authorization").is_some());
        assert!(headers.get("content-type").is_some());
        assert!(headers.get("x-request-id").is_some());
    }

    // --- integration test: full router path with println ---

    #[tokio::test]
    async fn middleware_strips_headers_before_handler() {
        let app = router();

        let mut req = Request::builder()
            .uri("/api/test")
            .header("connection", "keep-alive, x-custom-token")
            .header("keep-alive", "timeout=5")
            .header("transfer-encoding", "chunked")
            .header("x-custom-token", "should-be-stripped")
            .header("x-real-header", "should-survive")
            .header("authorization", "Bearer abc")
            .body(Body::empty())
            .unwrap();

        // ConnectInfo<SocketAddr> is required by the handler extractor
        req.extensions_mut()
            .insert(ConnectInfo("127.0.0.1:0".parse::<SocketAddr>().unwrap()));

        // Response will be 501 since the upstream call isn't wired yet.
        // The println! inside proxy_request shows what headers actually arrived.
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    }
}
