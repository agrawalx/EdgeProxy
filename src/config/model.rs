use serde::Deserialize;

use crate::proxy::lb::LbStrategy;

/// The on-disk YAML mirror. Faithful to the file; defaults are *not* applied here
/// (that happens in the `Config → Blueprint` conversion). Scalars are typed
/// because interpolation runs over raw text *before* parsing, so a `${VAR}`
/// placeholder is already substituted by the time serde sees it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub listen: Option<String>,
    pub tls: Option<TlsConfig>,
    pub defaults: Option<Defaults>,
    pub cache_store: Option<CacheStoreConfig>,
    pub observability: Option<ObservabilityConfig>,
    #[serde(default)]
    pub backends: Vec<BackendConfig>,
    /// Reserved for a future auth phase; parsed but ignored today.
    #[serde(default)]
    #[allow(dead_code)]
    pub auth: Option<serde_yaml_ng::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TlsConfig {
    pub cert: String,
    pub key: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Defaults {
    pub request_timeout_s: Option<u64>,
    pub connect_timeout_s: Option<u64>,
    pub retries: Option<u32>,
    pub rate_limit: Option<RateLimitConfig>,
    pub cache: Option<CacheConfig>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RateLimitConfig {
    pub rps: Option<u32>,
    pub burst: Option<u32>,
    pub key: Option<String>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CacheConfig {
    pub enabled: Option<bool>,
    pub ttl_s: Option<u64>,
    pub max_body_kb: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CacheStoreConfig {
    pub l1_max_entries: Option<u64>,
    pub l2_path: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservabilityConfig {
    pub log_format: Option<String>,
    pub otlp_endpoint: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackendConfig {
    pub name: String,
    pub prefix: String,
    pub kind: BackendKindConfig,
    pub request_timeout_s: Option<u64>,
    pub connect_timeout_s: Option<u64>,
    pub retries: Option<u32>,
    pub rate_limit: Option<RateLimitConfig>,
    pub cache: Option<CacheConfig>,
}

/// Dispatch seam in config form. Only `http_passthrough` today; an `mcp` variant
/// slots in here later without disturbing the existing path.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BackendKindConfig {
    HttpPassthrough {
        upstreams: Vec<String>,
        #[serde(default)]
        lb: LbStrategy,
    },
}
