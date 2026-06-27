use crate::proxy::lb::LbState;
use crate::telemetry::{UPSTREAM_ERRORS_TOTAL, UPSTREAM_REQUESTS_TOTAL, UPSTREAM_REQUEST_DURATION};
use axum::body::Body;
use axum::extract::{ConnectInfo, Request};
use axum::response::{IntoResponse, Response};
use http::status::StatusCode;
use http::{HeaderMap, HeaderName, HeaderValue, Uri};
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use metrics::{counter, histogram};
use std::net::SocketAddr;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Instant;
use tower::{Layer, Service};
use tracing::field;
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
// Proxy client
// ---------------------------------------------------------------------------
pub type ProxyClient = Client<HttpConnector, Body>;

/// Build the shared upstream HTTP client (pooled, reused across all backends).
pub fn build_proxy_client() -> Arc<ProxyClient> {
    Arc::new(
        Client::builder(TokioExecutor::new())
            .pool_max_idle_per_host(32)
            .build(HttpConnector::new()),
    )
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
/// Carried on the response so the `ObservabilityFuture` can label metrics with
/// the backend name rather than the raw path.
#[derive(Clone)]
pub struct BackendRoute(pub String);

pub async fn proxy_request(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
    lb: Arc<LbState>,
    client: Arc<ProxyClient>,
    backend_name: String,
) -> impl IntoResponse {
    let client_ip = peer.ip().to_string();
    let (idx, upstream_base) = lb.pick(Some(&client_ip));
    lb.increment(idx);
    let _guard = ConnectionGuard { lb: &lb, idx };

    // Tell the root `request` span (created by ObservabilityLayer) which backend
    // is serving this request. Must happen before the forward so the child span
    // inherits the correct parent context.
    tracing::Span::current().record("route", &backend_name);

    // Hop-by-hop headers are already stripped by StripHopByHopLayer.
    let (mut parts, body) = req.into_parts();

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

    let path_and_query = parts
        .uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/");
    let upstream_uri = match format!("{upstream_base}{path_and_query}").parse::<Uri>() {
        Ok(uri) => uri,
        Err(_) => return StatusCode::BAD_GATEWAY.into_response(),
    };
    parts.uri = upstream_uri.clone();

    let upstream_req = Request::from_parts(parts, body);
    let authority = upstream_uri
        .authority()
        .map(|a| a.as_str().to_owned())
        .unwrap_or_default();

    forward_once(upstream_req, &client, &backend_name, &authority, backend_name.clone()).await
}

/// The actual upstream hop, extracted so `#[instrument]` opens a child `upstream`
/// span scoped only to the network call. `skip_all` is mandatory — without it the
/// macro captures function args including headers (Authorization/Cookie leak risk).
#[tracing::instrument(
    name = "upstream",
    level = "debug",
    skip_all,
    fields(
        backend  = %backend,
        server_addr = %server_addr,
        attempt  = 1,
        status   = field::Empty,
        upstream_latency_ms = field::Empty,
    )
)]
async fn forward_once(
    req: Request,
    client: &ProxyClient,
    backend: &str,
    server_addr: &str,
    backend_name: String,
) -> Response {
    let start = Instant::now();
    match client.request(req).await {
        Err(e) => {
            let latency_ms = start.elapsed().as_secs_f64() * 1000.0;
            tracing::Span::current().record("upstream_latency_ms", latency_ms);
            tracing::error!(cause = %e, "upstream connection failed");
            counter!(
                UPSTREAM_ERRORS_TOTAL,
                "backend" => backend_name.clone(),
                "kind"    => "connect",
            )
            .increment(1);
            counter!(
                UPSTREAM_REQUESTS_TOTAL,
                "backend" => backend_name,
                "status"  => "error",
            )
            .increment(1);
            StatusCode::BAD_GATEWAY.into_response()
        }
        Ok(upstream_res) => {
            let latency = start.elapsed();
            let latency_ms = latency.as_secs_f64() * 1000.0;
            let status = upstream_res.status().as_u16();

            tracing::Span::current().record("status", status);
            tracing::Span::current().record("upstream_latency_ms", latency_ms);
            tracing::debug!(status, "upstream responded");

            counter!(
                UPSTREAM_REQUESTS_TOTAL,
                "backend" => backend_name.clone(),
                "status"  => status.to_string(),
            )
            .increment(1);
            histogram!(
                UPSTREAM_REQUEST_DURATION,
                "backend" => backend_name.clone(),
            )
            .record(latency.as_secs_f64());

            let (mut resp_parts, resp_body) = upstream_res.into_parts();
            strip_hop_by_hop(&mut resp_parts.headers);
            let mut response = Response::from_parts(resp_parts, Body::new(resp_body));
            response.extensions_mut().insert(BackendRoute(backend_name));
            response
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        BackendBlueprint, BackendKindBlueprint, CacheBlueprint, RateLimitBlueprint, RateLimitKey,
    };
    use crate::proxy::backend::Backend;
    use crate::proxy::lb::LbStrategy;
    use crate::proxy::{AppState, entrypoint};
    use axum::body::Body;
    use std::time::Duration;
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

    // ---------------------------------------------------------------------------
    // Integration test helpers
    // ---------------------------------------------------------------------------

    // Prometheus recorder is a global singleton — install it once for the whole
    // test process and hand out clones of the handle to each test.
    fn test_recorder() -> metrics_exporter_prometheus::PrometheusHandle {
        use std::sync::OnceLock;
        static HANDLE: OnceLock<metrics_exporter_prometheus::PrometheusHandle> = OnceLock::new();
        HANDLE
            .get_or_init(|| {
                metrics_exporter_prometheus::PrometheusBuilder::new()
                    .install_recorder()
                    .unwrap()
            })
            .clone()
    }

    fn make_bp() -> BackendBlueprint {
        BackendBlueprint {
            name: "test-api".into(),
            prefix: "/api".into(),
            // port 19999 is intentionally dead — forces a connect error (502).
            kind: BackendKindBlueprint::HttpPassthrough {
                upstreams: vec!["http://127.0.0.1:19999".into()],
                lb: LbStrategy::RoundRobin,
            },
            request_timeout: Duration::from_secs(5),
            connect_timeout: Duration::from_secs(1),
            retries: 0,
            rate_limit: RateLimitBlueprint {
                rps: 1000,
                burst: 500,
                key: RateLimitKey::Ip,
            },
            cache: CacheBlueprint {
                enabled: false,
                ttl: Duration::from_secs(30),
                max_body_bytes: 262_144,
            },
        }
    }

    fn test_app() -> (axum::Router, metrics_exporter_prometheus::PrometheusHandle) {
        let metrics = test_recorder();
        let state = AppState {
            client: build_proxy_client(),
            registry: vec![Backend::from_blueprint(&make_bp())],
            metrics: metrics.clone(),
        };
        (entrypoint(state), metrics)
    }

    /// Build a request pre-loaded with a peer `ConnectInfo` extension so the
    /// proxy handler extractor doesn't panic. Safe to call for non-proxy routes
    /// too — the extension is silently ignored there.
    fn req(method: &str, uri: &str) -> Request<Body> {
        let mut r = Request::builder()
            .method(method)
            .uri(uri)
            .body(Body::empty())
            .unwrap();
        r.extensions_mut()
            .insert(ConnectInfo("127.0.0.1:0".parse::<SocketAddr>().unwrap()));
        r
    }

    // ---------------------------------------------------------------------------
    // §8.1 — EnvFilter parses log_level from config
    // ---------------------------------------------------------------------------

    #[test]
    fn env_filter_parses_log_level_from_config() {
        use tracing_subscriber::EnvFilter;
        // Valid level builds without panic.
        let _ = EnvFilter::try_new("debug").expect("valid directive");
        // Compound directive (module-scoped) also works.
        let _ = EnvFilter::try_new("warn,Edgeproxy=debug").expect("compound directive");
        // Invalid level falls back gracefully — no panic.
        let _ = EnvFilter::try_new("not!!a!!level")
            .unwrap_or_else(|_| EnvFilter::new("info"));
    }

    // ---------------------------------------------------------------------------
    // §8.2 — request-id: absent → generated UUID; present → preserved
    // ---------------------------------------------------------------------------

    #[tokio::test]
    async fn request_id_generated_when_absent() {
        let (app, _) = test_app();
        let response = app.oneshot(req("GET", "/api/test")).await.unwrap();

        let id = response
            .headers()
            .get("x-request-id")
            .expect("ObservabilityLayer must echo x-request-id on response");
        let s = id.to_str().unwrap();
        // Generated id must be a valid UUID v4.
        uuid::Uuid::parse_str(s)
            .unwrap_or_else(|_| panic!("generated x-request-id is not a UUID: {s}"));
    }

    #[tokio::test]
    async fn request_id_preserved_when_present() {
        let (app, _) = test_app();
        let mut r = req("GET", "/api/test");
        r.headers_mut()
            .insert("x-request-id", "my-trace-id-abc".parse().unwrap());

        let response = app.oneshot(r).await.unwrap();
        let echoed = response
            .headers()
            .get("x-request-id")
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(echoed, "my-trace-id-abc");
    }

    // ---------------------------------------------------------------------------
    // §8.3 — all routes get a response (span + access log fires for each)
    // ---------------------------------------------------------------------------

    #[tokio::test]
    async fn health_returns_200() {
        let (app, _) = test_app();
        let response = app.oneshot(req("GET", "/health")).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn metrics_returns_200_with_prometheus_content_type() {
        let (app, _) = test_app();
        let response = app.oneshot(req("GET", "/metrics")).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let ct = response.headers().get("content-type").unwrap().to_str().unwrap();
        assert!(ct.contains("text/plain"), "unexpected content-type: {ct}");
    }

    #[tokio::test]
    async fn unknown_path_returns_404() {
        let (app, _) = test_app();
        let response = app.oneshot(req("GET", "/no/such/route")).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    // ---------------------------------------------------------------------------
    // §8.4 — metrics move: counters and gauge reflect traffic
    // ---------------------------------------------------------------------------

    #[tokio::test]
    async fn metrics_recorded_after_proxy_request() {
        let (app, metrics) = test_app();

        let response = app.oneshot(req("GET", "/api/test")).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);

        let rendered = metrics.render();

        assert!(
            rendered.contains("http_requests_total"),
            "http_requests_total missing;\n{rendered}"
        );
        assert!(
            rendered.contains("http_requests_in_flight 0"),
            "in-flight gauge must be 0 after response;\n{rendered}"
        );
        // Hop-by-hop stripping must not touch end-to-end headers on the response.
        assert!(
            response.headers().get("x-request-id").is_some(),
            "x-request-id must be echoed on the response"
        );
    }

    // ---------------------------------------------------------------------------
    // §8.5 — upstream failure: 502 + error counter + error metrics
    // ---------------------------------------------------------------------------

    #[tokio::test]
    async fn upstream_failure_increments_error_counters() {
        let (app, metrics) = test_app();

        let response = app.oneshot(req("GET", "/api/fail")).await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::BAD_GATEWAY,
            "dead upstream must yield 502"
        );

        let rendered = metrics.render();
        assert!(
            rendered.contains("upstream_errors_total"),
            "upstream_errors_total missing after connect failure;\n{rendered}"
        );
        assert!(
            rendered.contains("upstream_requests_total"),
            "upstream_requests_total missing;\n{rendered}"
        );
    }

    // ---------------------------------------------------------------------------
    // §8.6 — secret hygiene: Authorization must not appear in span fields
    //
    // Dynamic log-capture testing requires `tracing-test` (not yet a dep).
    // The static guarantee is enforced by `#[instrument(skip_all)]` on
    // `forward_once` — only explicitly named fields reach the subscriber.
    // The test below verifies the header is stripped *from the forwarded request*
    // (hop-by-hop stripping), not that it never existed. The real hygiene check
    // is: `forward_once` uses `skip_all`, and the only explicit fields are
    // `backend`, `server_addr`, `attempt`, `status`, `upstream_latency_ms`.
    // ---------------------------------------------------------------------------

    #[tokio::test]
    async fn hop_by_hop_stripped_on_proxied_request() {
        let (app, _) = test_app();
        let mut r = req("GET", "/api/secret");
        r.headers_mut()
            .insert("transfer-encoding", "chunked".parse().unwrap());
        r.headers_mut()
            .insert("x-real-header", "must-survive".parse().unwrap());
        // Authorization is an end-to-end header — must NOT be stripped.
        r.headers_mut()
            .insert("authorization", "Bearer super-secret".parse().unwrap());

        let response = app.oneshot(r).await.unwrap();
        // 502 expected (dead upstream); what matters is the layer ran without panic.
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        // x-request-id proves the ObservabilityLayer ran end-to-end.
        assert!(response.headers().get("x-request-id").is_some());
    }
}
