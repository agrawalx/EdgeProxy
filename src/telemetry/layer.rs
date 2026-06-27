use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Instant;

use axum::extract::{ConnectInfo, Request};
use axum::response::Response;
use http::{HeaderName, HeaderValue, Method};
use metrics::{counter, gauge, histogram};
use pin_project_lite::pin_project;
use tower::{Layer, Service};
use tracing::instrument::Instrumented;
use tracing::{Instrument, Span, field, info_span};
use uuid::Uuid;

use crate::proxy::BackendRoute;
use super::metrics::{HTTP_IN_FLIGHT, HTTP_REQUESTS_TOTAL, HTTP_REQUEST_DURATION};

const REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

// ---------------------------------------------------------------------------
// Layer
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
pub struct ObservabilityLayer;

impl ObservabilityLayer {
    pub fn new() -> Self {
        Self
    }
}

impl<S> Layer<S> for ObservabilityLayer {
    type Service = ObservabilityService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        ObservabilityService { inner }
    }
}

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct ObservabilityService<S> {
    inner: S,
}

impl<S, B> Service<Request<B>> for ObservabilityService<S>
where
    // We require the inner response to be the concrete `Response` so we can read
    // `.status()` and insert the response header after the inner future resolves.
    S: Service<Request<B>, Response = Response>,
{
    type Response = Response;
    type Error = S::Error;
    type Future = ObservabilityFuture<S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<B>) -> Self::Future {
        // 1. request-id: preserve a valid inbound id, else mint a fresh uuid v4.
        let request_id = req
            .headers()
            .get(&REQUEST_ID)
            .and_then(|v| v.to_str().ok())
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| Uuid::new_v4().to_string());

        let method: Method = req.method().clone();
        let path = req.uri().path().to_owned();

        let client_ip = req
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|ci| ci.0.ip().to_string())
            .unwrap_or_else(|| "-".to_owned());

        // 2. root span. Fields filled later are declared `Empty` up front,
        let span = info_span!(
            "request",
            %method,
            %path,
            %request_id,
            %client_ip,
            route = field::Empty,
            status = field::Empty,
            latency_ms = field::Empty,
        );

        // 3. in-flight gauge (RAII: decrements on drop, so it can't leak even if
        //    the client disconnects and the future is dropped mid-flight) + timer.
        gauge!(HTTP_IN_FLIGHT).increment(1.0);
        let guard = InFlightGuard;
        let start = Instant::now();

        let inner = self.inner.call(req).instrument(span.clone());

        ObservabilityFuture {
            inner,
            start,
            span,
            request_id,
            method: method.as_str().to_owned(),
            _guard: guard,
        }
    }
}

// ---------------------------------------------------------------------------
// Future
// ---------------------------------------------------------------------------

pin_project! {
    pub struct ObservabilityFuture<F> {
        // The only structurally-pinned field; the macro generates the same
        // projection your hand-written `unsafe` did, but proves its soundness.
        #[pin]
        inner: Instrumented<F>,
        start: Instant,
        span: Span,
        request_id: String,
        method: String,
        // RAII gauge guard. Unpin, not structurally pinned. Decrements on drop.
        _guard: InFlightGuard,
    }
}

impl<F, E> Future for ObservabilityFuture<F>
where
    F: Future<Output = Result<Response, E>>,
{
    type Output = Result<Response, E>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut this = self.project();

        let result = match this.inner.as_mut().poll(cx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(result) => result,
        };

        let latency = this.start.elapsed();
        let latency_ms = latency.as_secs_f64() * 1000.0;

        match result {
            Ok(mut response) => {
                let status = response.status();
                let code = status.as_u16();

                // 5a. backfill the span fields.
                this.span.record("status", code);
                this.span.record("latency_ms", latency_ms);

                // 5b. one access-log event, level chosen by status class (5xx
                //     loud, everything else quiet). `parent` attaches it to the
                //     request span without entering across any await.
                let span = &*this.span;
                if status.is_server_error() {
                    tracing::error!(parent: span, status = code, latency_ms, "request completed");
                } else if status.is_client_error() {
                    tracing::warn!(parent: span, status = code, latency_ms, "request completed");
                } else {
                    tracing::info!(parent: span, status = code, latency_ms, "request completed");
                }

                // 5c. http metrics. `route` comes from the response extension
                //     set by proxy_request after lb selection — bounded cardinality,
                //     never the raw path.
                let route = response
                    .extensions()
                    .get::<BackendRoute>()
                    .map(|r| r.0.as_str())
                    .unwrap_or("unmatched");
                counter!(
                    HTTP_REQUESTS_TOTAL,
                    "route" => route.to_owned(),
                    "method" => this.method.clone(),
                    "status" => code.to_string(),
                )
                .increment(1);
                histogram!(
                    HTTP_REQUEST_DURATION,
                    "route" => route.to_owned(),
                    "method" => this.method.clone(),
                )
                .record(latency.as_secs_f64());

                // 5d. echo the request-id back to the caller.
                if let Ok(val) = HeaderValue::from_str(this.request_id) {
                    response.headers_mut().insert(REQUEST_ID, val);
                }

                // 6. (the in-flight gauge is decremented by `_guard`'s Drop.)
                Poll::Ready(Ok(response))
            }
            // Inner service error. For an axum router `E = Infallible` so this is
            // unreachable in practice, but we still close out the span honestly.
            Err(e) => {
                this.span.record("latency_ms", latency_ms);
                tracing::error!(parent: &*this.span, latency_ms, "request failed before response");
                Poll::Ready(Err(e))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// In-flight RAII guard
// ---------------------------------------------------------------------------

/// Decrements the in-flight gauge on drop, so the count is correct whether the
/// future completes normally or is dropped early (client disconnect, timeout).
struct InFlightGuard;

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        gauge!(HTTP_IN_FLIGHT).decrement(1.0);
    }
}
