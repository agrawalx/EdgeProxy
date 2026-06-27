use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use serde::Deserialize;

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LbStrategy {
    #[default]
    RoundRobin,
    LeastConnections,
    IpHash,
}

/// Runtime state for a single backend's load balancer.
/// Created once at startup, shared across all requests via `Arc`. Replica URLs
/// are runtime-owned (`Arc<[String]>`) — not `&'static` — so they can come
/// from a config file and so future control-plane state can sit alongside.
pub enum LbState {
    RoundRobin {
        replicas: Arc<[String]>,
        counter: AtomicUsize,
    },
    LeastConnections {
        replicas: Arc<[String]>,
        active: Vec<AtomicUsize>,
    },
    IpHash {
        replicas: Arc<[String]>,
    },
}

impl LbState {
    pub fn new(strategy: LbStrategy, replicas: Arc<[String]>) -> Arc<Self> {
        Arc::new(match strategy {
            LbStrategy::RoundRobin => Self::RoundRobin {
                replicas,
                counter: AtomicUsize::new(0),
            },
            LbStrategy::LeastConnections => {
                let active = (0..replicas.len()).map(|_| AtomicUsize::new(0)).collect();
                Self::LeastConnections { replicas, active }
            }
            LbStrategy::IpHash => Self::IpHash { replicas },
        })
    }

    /// Pick an upstream replica. Returns `(index, url)` — store the index
    /// and pass it back to `increment`/`decrement` for O(1) connection tracking.
    pub fn pick(&self, client_ip: Option<&str>) -> (usize, &str) {
        match self {
            Self::RoundRobin { replicas, counter } => {
                let idx = counter.fetch_add(1, Ordering::Relaxed) % replicas.len();
                (idx, &replicas[idx])
            }

            Self::LeastConnections { replicas, active } => {
                let idx = active
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, c)| c.load(Ordering::Relaxed))
                    .map(|(i, _)| i)
                    .unwrap_or(0);
                (idx, &replicas[idx])
            }

            Self::IpHash { replicas } => {
                let ip = client_ip.unwrap_or("0.0.0.0");
                let mut hasher = DefaultHasher::new();
                ip.hash(&mut hasher);
                let idx = hasher.finish() as usize % replicas.len();
                (idx, &replicas[idx])
            }
        }
    }

    pub fn increment(&self, idx: usize) {
        if let Self::LeastConnections { active, .. } = self {
            active[idx].fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn decrement(&self, idx: usize) {
        if let Self::LeastConnections { active, .. } = self {
            active[idx].fetch_sub(1, Ordering::Relaxed);
        }
    }
}
