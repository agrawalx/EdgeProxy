mod config;
mod proxy;
mod telemetry;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;

use crate::config::{Blueprint, load};
use crate::proxy::{AppState, Backend, build_proxy_client, entrypoint};
use crate::telemetry::{TelemetryHandles, init};

#[derive(Parser)]
#[command(
    name = "edgeproxy",
    about = "Generic reverse proxy (MCP-aware layer to come)"
)]
struct Cli {
    /// Config file(s). Repeatable; merged left-to-right (later files win).
    #[arg(short, long)]
    config: Vec<PathBuf>,
    /// Validate the config and exit without serving.
    #[arg(long)]
    check_config: bool,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let paths = if cli.config.is_empty() {
        vec![PathBuf::from("config.yaml")]
    } else {
        cli.config
    };

    let blueprint = match load(&paths) {
        Ok(bp) => bp,
        Err(e) => {
            eprintln!("config error: {e}");
            std::process::exit(1);
        }
    };

    let handles = init(&blueprint);

    if cli.check_config {
        println!(
            "config OK: {} backend(s), listen {}",
            blueprint.backends.len(),
            blueprint.listen
        );
        return;
    }

    serve(blueprint, handles).await;
}

async fn serve(blueprint: Blueprint, handles: TelemetryHandles) {
    let registry: Vec<Arc<Backend>> = blueprint
        .backends
        .iter()
        .map(Backend::from_blueprint)
        .collect();
    let state = AppState {
        client: build_proxy_client(),
        registry,
        metrics: handles.metrics,
    };

    let listener = tokio::net::TcpListener::bind(blueprint.listen)
        .await
        .expect("bind listen address");
    tracing::info!(listen = %blueprint.listen, "server listening");
    axum::serve(
        listener,
        entrypoint(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .expect("server error");

    // Flush in-flight OTLP span batches before the process exits.
    handles.tracer_provider.map(|p| {
        if let Err(e) = p.shutdown() {
            eprintln!("OTLP shutdown error: {e}");
        }
    });
}
