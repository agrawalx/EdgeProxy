mod config;
mod proxy;
use crate::proxy::entrypoint;

#[tokio::main]
async fn main() {
    // run our app with hyper, listening globally on port 3000
    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    axum::serve(listener, entrypoint()).await.unwrap();
}
