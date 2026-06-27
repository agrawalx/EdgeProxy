mod config;
mod proxy;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;

use crate::config::{Blueprint, LogFormat, load};
use crate::proxy::{AppState, Backend, build_proxy_client, entrypoint};

#[derive(Parser)]
#[command(name = "edgeproxy", about = "Generic reverse proxy (MCP-aware layer to come)")]
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

    init_tracing(blueprint.observability.log_format);

    if cli.check_config {
        println!(
            "config OK: {} backend(s), listen {}",
            blueprint.backends.len(),
            blueprint.listen
        );
        return;
    }

    serve(blueprint).await;
}

async fn serve(blueprint: Blueprint) {
    let registry: Vec<Arc<Backend>> =
        blueprint.backends.iter().map(Backend::from_blueprint).collect();
    let state = AppState {
        client: build_proxy_client(),
        registry,
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
}

fn init_tracing(format: LogFormat) {
    match format {
        LogFormat::Json => tracing_subscriber::fmt().json().init(),
        LogFormat::Pretty => tracing_subscriber::fmt().pretty().init(),
    }
}
