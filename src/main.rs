mod config;
mod proxy;

use std::net::SocketAddr;
use crate::config::LISTEN_ADDR;
use crate::proxy::entrypoint;
#[tokio::main]
async fn main() {
    // run our app with hyper, listening globally on port 3000
    let listener = tokio::net::TcpListener::bind(LISTEN_ADDR).await.unwrap();
    println!("server listening on port {}", LISTEN_ADDR);
    axum::serve(listener, entrypoint().into_make_service_with_connect_info::<SocketAddr>()).await.unwrap();
}
