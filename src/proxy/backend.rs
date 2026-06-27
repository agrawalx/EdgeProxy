use std::sync::Arc;

use crate::config::{BackendBlueprint, BackendKindBlueprint};
use crate::proxy::lb::LbState;

/// A backend in the live registry, built once at startup from a `BackendBlueprint`.
/// Owns its runtime state — never `&'static` — so health, sessions, and other
/// per-backend control-plane state can hang off it as more layers are added.
pub struct Backend {
    pub name: String,
    pub prefix: String,
    pub kind: BackendKind,
}

/// Dispatch seam: each variant gets its own handler in the router.
/// `HttpPassthrough` is the reverse-proxy path today; MCP slots in as a second
/// variant later (with its own handler) without touching this path.
pub enum BackendKind {
    HttpPassthrough { lb: Arc<LbState> },
    // Mcp { .. }  ← added in the MCP phase
}

impl Backend {
    /// Build a live backend from its validated blueprint, materializing runtime
    /// state (the load balancer) from the resolved values.
    pub fn from_blueprint(bp: &BackendBlueprint) -> Arc<Self> {
        let kind = match &bp.kind {
            BackendKindBlueprint::HttpPassthrough { upstreams, lb } => {
                let replicas: Arc<[String]> = Arc::from(upstreams.clone());
                BackendKind::HttpPassthrough {
                    lb: LbState::new(*lb, replicas),
                }
            }
        };
        Arc::new(Self {
            name: bp.name.clone(),
            prefix: bp.prefix.clone(),
            kind,
        })
    }
}
