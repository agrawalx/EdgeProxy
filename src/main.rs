mod config;
mod proxy;
use crate::config::LISTEN_ADDR;
use crate::proxy::entrypoint;
#[tokio::main]
async fn main() {
    // run our app with hyper, listening globally on port 3000
    let listener = tokio::net::TcpListener::bind(LISTEN_ADDR).await.unwrap();
    println!("server listening on port {}", LISTEN_ADDR);
    axum::serve(listener, entrypoint()).await.unwrap();
}
