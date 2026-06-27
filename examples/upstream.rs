/// Fake upstream API server for local tracing demos.
/// Listens on :4000. Run with: cargo run --example upstream
use axum::Router;
use axum::response::IntoResponse;
use axum::routing::get;
use http::StatusCode;
use std::time::Duration;
use tokio::time::sleep;

#[tokio::main]
async fn main() {
    let app = Router::new()
        .route("/api/users", get(users))
        .route("/api/products", get(products))
        .route("/api/orders", get(orders))
        .route("/api/slow", get(slow))
        .route("/api/error", get(error));

    let listener = tokio::net::TcpListener::bind("0.0.0.0:4000").await.unwrap();
    println!("upstream listening on http://localhost:4000");
    axum::serve(listener, app).await.unwrap();
}

async fn users() -> impl IntoResponse {
    (
        StatusCode::OK,
        r#"{"users":[{"id":1,"name":"Alice"},{"id":2,"name":"Bob"},{"id":3,"name":"Carol"}]}"#,
    )
}

async fn products() -> impl IntoResponse {
    (
        StatusCode::OK,
        r#"{"products":[{"id":1,"name":"Widget","price":9.99},{"id":2,"name":"Gadget","price":29.99}]}"#,
    )
}

async fn orders() -> impl IntoResponse {
    (
        StatusCode::OK,
        r#"{"orders":[{"id":101,"user_id":1,"product_id":2,"status":"shipped"},{"id":102,"user_id":2,"product_id":1,"status":"pending"}]}"#,
    )
}

async fn slow() -> impl IntoResponse {
    // Simulates a slow DB query — shows up clearly in the upstream span latency.
    sleep(Duration::from_millis(600)).await;
    (StatusCode::OK, r#"{"result":"slow response done"}"#)
}

async fn error() -> impl IntoResponse {
    // Always 500 — drives upstream_errors_total and the WARN access-log event.
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        r#"{"error":"something went wrong upstream"}"#,
    )
}
