use crate::proxy::lb::LbStrategy;
pub const LISTEN_ADDR: &str = "0.0.0.0:8080";

// (name, route prefix, replicas)
pub const BACKENDS: &[(&str, &str, LbStrategy, &[&str])] = &[
    (
        "api-service",
        "/api",
        LbStrategy::RoundRobin,
        &["http://127.0.0.1:3001", "http://127.0.0.1:3002"],
    ),
    (
        "auth-service",
        "/auth",
        LbStrategy::IpHash,
        &["http://127.0.0.1:4001"],
    ),
    (
        "admin-service",
        "/admin",
        LbStrategy::LeastConnections,
        &["http://127.0.0.1:5001", "http://127.0.0.1:5002"],
    ),
];
