use opentelemetry::KeyValue;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::runtime::Tokio;
use opentelemetry_sdk::trace::TracerProvider;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer, fmt};

use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};

use crate::config::{Blueprint, LogFormat};
use crate::telemetry::metrics;

/// Returned by `init` so `main` can hand the metrics handle to `AppState` and
/// call `shutdown()` on the tracer provider before the process exits.
pub struct TelemetryHandles {
    pub metrics: PrometheusHandle,
    /// Present only when `otlp_endpoint` is configured. Must be shut down on
    /// exit to flush in-flight span batches.
    pub tracer_provider: Option<TracerProvider>,
}

impl TelemetryHandles {
    /// Flush and shut down the OTLP exporter. Call this after the axum server
    /// returns so the last spans are not silently dropped.
    pub fn shutdown(self) {
        if let Some(provider) = self.tracer_provider {
            if let Err(e) = provider.shutdown() {
                eprintln!("OTLP shutdown error: {e}");
            }
        }
    }
}

pub fn init(bp: &Blueprint) -> TelemetryHandles {
    let filter =
        EnvFilter::try_new(&bp.observability.log_level).unwrap_or_else(|_| EnvFilter::new("info"));

    let fmt_layer = match bp.observability.log_format {
        LogFormat::Json => fmt::layer().json().boxed(),
        LogFormat::Pretty => fmt::layer().pretty().boxed(),
    };

    // Build the OTLP layer only when an endpoint is configured. `Option<Layer>`
    // implements `Layer` in tracing-subscriber, so `.with(None)` is a no-op.
    let (otlp_layer, tracer_provider) = match &bp.observability.otlp_endpoint {
        None => (None, None),
        Some(endpoint) => {
            let exporter = opentelemetry_otlp::SpanExporter::builder()
                .with_tonic()
                .with_endpoint(endpoint)
                .build()
                .expect("build OTLP span exporter");

            let resource = Resource::new([KeyValue::new("service.name", "edgeproxy")]);
            let provider = TracerProvider::builder()
                .with_batch_exporter(exporter, Tokio)
                .with_resource(resource)
                .build();

            use opentelemetry::trace::TracerProvider as _;
            let tracer = provider.tracer("edgeproxy");
            let layer = tracing_opentelemetry::layer().with_tracer(tracer);
            (Some(layer), Some(provider))
        }
    };

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt_layer)
        .with(otlp_layer) // no-op when None
        .init();

    let metrics = PrometheusBuilder::new()
        .set_buckets(&[
            0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0,
        ])
        .expect("valid buckets")
        .install_recorder()
        .expect("install metrics recorder");

    metrics::describe();

    TelemetryHandles {
        metrics,
        tracer_provider,
    }
}
