pub const LISTEN_ADDR: &str = "0.0.0.0:8080";

// (name, route prefix, replicas)
pub const BACKENDS: &[(&str, &str, &[&str])] = &[
    (
        "api-service",
        "/api",
        &["http://127.0.0.1:3001", "http://127.0.0.1:3002"],
    ),
    ("auth-service", "/auth", &["http://127.0.0.1:4001"]),
    (
        "admin-service",
        "/admin",
        &["http://127.0.0.1:5001", "http://127.0.0.1:5002"],
    ),
];
