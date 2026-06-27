// Several Blueprint fields are populated and validated here but only consumed by
// later foundation phases (F2–F6: TLS, cache store, rate limits, timeouts).
// They are part of the runtime model now so the config pipeline is complete.
#![allow(dead_code)]

use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use super::model::{BackendKindConfig, CacheConfig, Config, RateLimitConfig};
use crate::proxy::lb::LbStrategy;

// ---- defaults (applied here, nowhere else) --------------------------------
const DEFAULT_LISTEN: &str = "0.0.0.0:8080";
const DEFAULT_REQUEST_TIMEOUT_S: u64 = 30;
const DEFAULT_CONNECT_TIMEOUT_S: u64 = 5;
const DEFAULT_RETRIES: u32 = 2;
const DEFAULT_RPS: u32 = 100;
const DEFAULT_BURST: u32 = 50;
const DEFAULT_CACHE_ENABLED: bool = false;
const DEFAULT_CACHE_TTL_S: u64 = 30;
const DEFAULT_CACHE_MAX_BODY_KB: u64 = 256;
const DEFAULT_L1_MAX_ENTRIES: u64 = 100_000;
const DEFAULT_LOG_FORMAT: LogFormat = LogFormat::Json;
const DEFAULT_LOG_LEVEL: &str = "info";

// ---- the runtime model ----------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    Json,
    Pretty,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateLimitKey {
    Ip,
}

#[derive(Debug, Clone)]
pub struct Blueprint {
    pub listen: SocketAddr,
    pub tls: Option<TlsBlueprint>,
    pub cache_store: CacheStoreBlueprint,
    pub observability: ObservabilityBlueprint,
    pub backends: Vec<BackendBlueprint>,
}

#[derive(Debug, Clone)]
pub struct TlsBlueprint {
    pub cert: PathBuf,
    pub key: PathBuf,
}

#[derive(Debug, Clone)]
pub struct CacheStoreBlueprint {
    pub l1_max_entries: u64,
    pub l2_path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct ObservabilityBlueprint {
    pub log_format: LogFormat,
    pub log_level: String,
    pub otlp_endpoint: Option<String>,
}

#[derive(Debug, Clone)]
pub struct BackendBlueprint {
    pub name: String,
    pub prefix: String,
    pub kind: BackendKindBlueprint,
    pub request_timeout: Duration,
    pub connect_timeout: Duration,
    pub retries: u32,
    pub rate_limit: RateLimitBlueprint,
    pub cache: CacheBlueprint,
}

#[derive(Debug, Clone)]
pub enum BackendKindBlueprint {
    HttpPassthrough {
        upstreams: Vec<String>,
        lb: LbStrategy,
    },
}

#[derive(Debug, Clone)]
pub struct RateLimitBlueprint {
    pub rps: u32,
    pub burst: u32,
    pub key: RateLimitKey,
}

#[derive(Debug, Clone)]
pub struct CacheBlueprint {
    pub enabled: bool,
    pub ttl: Duration,
    pub max_body_bytes: u64,
}

/// One config problem, with the YAML path that caused it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    pub path: String,
    pub message: String,
}

// ---- conversion (validation by conversion, D31) ---------------------------

/// Error accumulator — collects every problem instead of failing on the first.
struct Acc {
    errors: Vec<ValidationError>,
}

impl Acc {
    fn new() -> Self {
        Self { errors: Vec::new() }
    }

    fn push(&mut self, path: impl Into<String>, message: impl Into<String>) {
        self.errors.push(ValidationError {
            path: path.into(),
            message: message.into(),
        });
    }
}

impl TryFrom<Config> for Blueprint {
    type Error = Vec<ValidationError>;

    fn try_from(c: Config) -> Result<Self, Self::Error> {
        let mut acc = Acc::new();

        let listen = parse_listen(c.listen.as_deref(), &mut acc);

        let tls = c.tls.map(|t| TlsBlueprint {
            cert: PathBuf::from(t.cert),
            key: PathBuf::from(t.key),
        });

        let cs = c.cache_store.unwrap_or_default();
        let cache_store = CacheStoreBlueprint {
            l1_max_entries: cs.l1_max_entries.unwrap_or(DEFAULT_L1_MAX_ENTRIES),
            l2_path: cs.l2_path.map(PathBuf::from),
        };

        let obs = c.observability.unwrap_or_default();
        let observability = ObservabilityBlueprint {
            log_format: parse_log_format(obs.log_format.as_deref(), &mut acc),
            log_level: obs
                .log_level
                .unwrap_or_else(|| DEFAULT_LOG_LEVEL.to_string()),
            otlp_endpoint: obs.otlp_endpoint,
        };

        let defaults = c.defaults.unwrap_or_default();
        let mut backends = Vec::with_capacity(c.backends.len());
        let mut seen_names = HashSet::new();
        let mut seen_prefixes = HashSet::new();

        for (i, b) in c.backends.into_iter().enumerate() {
            let path = format!("backends[{i}]");

            if b.name.trim().is_empty() {
                acc.push(format!("{path}.name"), "must not be empty");
            } else if !seen_names.insert(b.name.clone()) {
                acc.push(
                    format!("{path}.name"),
                    format!("duplicate backend name '{}'", b.name),
                );
            }

            if !b.prefix.starts_with('/') {
                acc.push(
                    format!("{path}.prefix"),
                    format!("must start with '/', got '{}'", b.prefix),
                );
            }
            if !seen_prefixes.insert(b.prefix.clone()) {
                acc.push(
                    format!("{path}.prefix"),
                    format!("duplicate prefix '{}'", b.prefix),
                );
            }

            let kind = match b.kind {
                BackendKindConfig::HttpPassthrough { upstreams, lb } => {
                    if upstreams.is_empty() {
                        acc.push(
                            format!("{path}.kind.upstreams"),
                            "must list at least one upstream",
                        );
                    }
                    for (j, u) in upstreams.iter().enumerate() {
                        if !valid_upstream(u) {
                            acc.push(
                                format!("{path}.kind.upstreams[{j}]"),
                                format!("'{u}' is not a valid http(s) URL"),
                            );
                        }
                    }
                    BackendKindBlueprint::HttpPassthrough { upstreams, lb }
                }
            };

            let request_timeout = Duration::from_secs(pick(
                b.request_timeout_s,
                defaults.request_timeout_s,
                DEFAULT_REQUEST_TIMEOUT_S,
            ));
            let connect_timeout = Duration::from_secs(pick(
                b.connect_timeout_s,
                defaults.connect_timeout_s,
                DEFAULT_CONNECT_TIMEOUT_S,
            ));
            let retries = pick(b.retries, defaults.retries, DEFAULT_RETRIES);
            let rate_limit = resolve_rate_limit(
                defaults.rate_limit.as_ref(),
                b.rate_limit.as_ref(),
                &path,
                &mut acc,
            );
            let cache = resolve_cache(defaults.cache.as_ref(), b.cache.as_ref());

            backends.push(BackendBlueprint {
                name: b.name,
                prefix: b.prefix,
                kind,
                request_timeout,
                connect_timeout,
                retries,
                rate_limit,
                cache,
            });
        }

        if acc.errors.is_empty() {
            Ok(Blueprint {
                listen,
                tls,
                cache_store,
                observability,
                backends,
            })
        } else {
            Err(acc.errors)
        }
    }
}

fn pick<T>(route: Option<T>, default: Option<T>, fallback: T) -> T {
    route.or(default).unwrap_or(fallback)
}

fn parse_listen(s: Option<&str>, acc: &mut Acc) -> SocketAddr {
    let raw = s.unwrap_or(DEFAULT_LISTEN);
    match raw.parse::<SocketAddr>() {
        Ok(addr) => addr,
        Err(_) => {
            acc.push(
                "listen",
                format!("'{raw}' is not a valid socket address (expected ip:port)"),
            );
            DEFAULT_LISTEN.parse().expect("default listen is valid")
        }
    }
}

fn parse_log_format(s: Option<&str>, acc: &mut Acc) -> LogFormat {
    match s {
        None => DEFAULT_LOG_FORMAT,
        Some("json") => LogFormat::Json,
        Some("pretty") => LogFormat::Pretty,
        Some(other) => {
            acc.push(
                "observability.log_format",
                format!("unknown log_format '{other}' (supported: json, pretty)"),
            );
            DEFAULT_LOG_FORMAT
        }
    }
}

fn valid_upstream(s: &str) -> bool {
    match s.parse::<http::Uri>() {
        Ok(uri) => {
            matches!(uri.scheme_str(), Some("http") | Some("https")) && uri.authority().is_some()
        }
        Err(_) => false,
    }
}

fn resolve_rate_limit(
    defaults: Option<&RateLimitConfig>,
    route: Option<&RateLimitConfig>,
    path: &str,
    acc: &mut Acc,
) -> RateLimitBlueprint {
    let rps = pick(
        route.and_then(|r| r.rps),
        defaults.and_then(|d| d.rps),
        DEFAULT_RPS,
    );
    let burst = pick(
        route.and_then(|r| r.burst),
        defaults.and_then(|d| d.burst),
        DEFAULT_BURST,
    );
    let key_str = route
        .and_then(|r| r.key.clone())
        .or_else(|| defaults.and_then(|d| d.key.clone()));
    let key = match key_str.as_deref() {
        None | Some("ip") => RateLimitKey::Ip,
        Some(other) => {
            acc.push(
                format!("{path}.rate_limit.key"),
                format!("unknown rate-limit key '{other}' (supported: ip)"),
            );
            RateLimitKey::Ip
        }
    };
    RateLimitBlueprint { rps, burst, key }
}

fn resolve_cache(defaults: Option<&CacheConfig>, route: Option<&CacheConfig>) -> CacheBlueprint {
    let enabled = pick(
        route.and_then(|r| r.enabled),
        defaults.and_then(|d| d.enabled),
        DEFAULT_CACHE_ENABLED,
    );
    let ttl_s = pick(
        route.and_then(|r| r.ttl_s),
        defaults.and_then(|d| d.ttl_s),
        DEFAULT_CACHE_TTL_S,
    );
    let max_body_kb = pick(
        route.and_then(|r| r.max_body_kb),
        defaults.and_then(|d| d.max_body_kb),
        DEFAULT_CACHE_MAX_BODY_KB,
    );
    CacheBlueprint {
        enabled,
        ttl: Duration::from_secs(ttl_s),
        max_body_bytes: max_body_kb * 1024,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(yaml: &str) -> Config {
        serde_yaml_ng::from_str(yaml).unwrap()
    }

    #[test]
    fn applies_defaults() {
        let cfg = config(
            "backends:\n  - name: api\n    prefix: /api\n    kind: { type: http_passthrough, upstreams: [http://127.0.0.1:3001] }\n",
        );
        let bp = Blueprint::try_from(cfg).unwrap();

        assert_eq!(bp.listen.to_string(), "0.0.0.0:8080");
        assert_eq!(bp.observability.log_format, LogFormat::Json);
        assert_eq!(bp.observability.log_level, "info");
        assert_eq!(bp.cache_store.l1_max_entries, 100_000);

        let b = &bp.backends[0];
        assert_eq!(b.retries, 2);
        assert_eq!(b.request_timeout, Duration::from_secs(30));
        assert_eq!(b.rate_limit.rps, 100);
        assert_eq!(b.rate_limit.key, RateLimitKey::Ip);
        assert!(!b.cache.enabled);
        match &b.kind {
            BackendKindBlueprint::HttpPassthrough { lb, .. } => {
                assert!(matches!(lb, LbStrategy::RoundRobin))
            }
        }
    }

    #[test]
    fn log_level_passthrough() {
        let cfg = config("observability:\n  log_level: \"debug,hyper=warn\"\nbackends: []\n");
        let bp = Blueprint::try_from(cfg).unwrap();
        assert_eq!(bp.observability.log_level, "debug,hyper=warn");
    }

    #[test]
    fn route_overrides_defaults() {
        let cfg = config(
            "defaults:\n  rate_limit: { rps: 100 }\n  cache: { enabled: false }\nbackends:\n  - name: api\n    prefix: /api\n    rate_limit: { rps: 500 }\n    cache: { enabled: true, ttl_s: 90 }\n    kind: { type: http_passthrough, upstreams: [http://127.0.0.1:3001] }\n",
        );
        let bp = Blueprint::try_from(cfg).unwrap();
        let b = &bp.backends[0];
        assert_eq!(b.rate_limit.rps, 500); // route override
        assert!(b.cache.enabled);
        assert_eq!(b.cache.ttl, Duration::from_secs(90));
    }

    #[test]
    fn accumulates_all_errors() {
        let cfg = config(
            "listen: not-an-addr\nbackends:\n  - name: dup\n    prefix: noslash\n    kind: { type: http_passthrough, upstreams: [] }\n  - name: dup\n    prefix: /ok\n    kind: { type: http_passthrough, upstreams: [ftp://bad] }\n",
        );
        let errs = Blueprint::try_from(cfg).unwrap_err();
        let paths: Vec<&str> = errs.iter().map(|e| e.path.as_str()).collect();

        assert!(paths.contains(&"listen"), "{paths:?}");
        assert!(
            paths.iter().any(|p| *p == "backends[0].prefix"),
            "{paths:?}"
        );
        assert!(
            paths.iter().any(|p| *p == "backends[0].kind.upstreams"),
            "{paths:?}"
        );
        assert!(paths.iter().any(|p| *p == "backends[1].name"), "{paths:?}");
        assert!(
            paths.iter().any(|p| *p == "backends[1].kind.upstreams[0]"),
            "{paths:?}"
        );
        assert!(errs.len() >= 5, "expected >=5 errors, got {}", errs.len());
    }
}
