/// Request bot for local tracing demos. Sends randomised traffic to the proxy.
/// Run with: cargo run --example bot
use axum::body::Body;
use http::Request;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;

const PROXY: &str = "http://127.0.0.1:8080";

const ENDPOINTS: &[&str] = &[
    "/api/users",
    "/api/products",
    "/api/orders",
    "/api/slow",
    "/api/error",
];

// Known request-ids to test the preservation path alongside generated UUIDs.
const KNOWN_IDS: &[&str] = &["bot-req-aaa", "bot-req-bbb", "bot-req-ccc"];

#[tokio::main]
async fn main() {
    let client: Client<HttpConnector, Body> =
        Client::builder(TokioExecutor::new()).build(HttpConnector::new());

    println!("bot started → sending traffic to {PROXY}");
    println!("  endpoints: {ENDPOINTS:?}");
    println!();

    let mut i: usize = 0;
    loop {
        let endpoint = ENDPOINTS[i % ENDPOINTS.len()];
        let url = format!("{PROXY}{endpoint}");

        // Every third request carries a known x-request-id to test preservation.
        let mut builder = Request::get(&url);
        if i % 3 == 0 {
            let id = KNOWN_IDS[(i / 3) % KNOWN_IDS.len()];
            builder = builder.header("x-request-id", id);
        }
        let req = builder.body(Body::empty()).unwrap();

        match client.request(req).await {
            Ok(res) => {
                let status = res.status();
                let req_id = res
                    .headers()
                    .get("x-request-id")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("-");
                println!("  {status}  {endpoint:<20}  request-id: {req_id}");
            }
            Err(e) => eprintln!("  ERROR  {endpoint}  {e}"),
        }

        i += 1;
        // Pseudo-random delay: 150–550 ms using a simple LCG-like scatter.
        let ms = 150 + (i.wrapping_mul(137).wrapping_add(31)) % 400;
        tokio::time::sleep(std::time::Duration::from_millis(ms as u64)).await;
    }
}
