//! Configuration pipeline (foundation phase F1).
//!
//! Flow: `load(paths)` →
//!   1. load `.env` next to the first file + process env (`.env` wins, D29)
//!   2. interpolate `${VAR}` / `${VAR:-default}` / `${VAR:?msg}` per file (pre-parse)
//!   3. parse each file to a YAML value and merge left-to-right (later wins; lists append)
//!   4. deserialize the merged value into `Config` (a faithful YAML mirror)
//!   5. convert `Config` → `Blueprint` (defaults applied, types parsed, all errors
//!      accumulated with paths) — validation-by-conversion (D31)
//!
//! Downstream code consumes the `Blueprint`; it never sees `Option`s for required
//! values or re-parses strings.

mod blueprint;
mod error;
mod load;
mod merge;
mod model;
mod resolve;

// Flat public surface of the config module (D28). Some items are part of the
// API but not yet referenced by name from the binary — hence allow(unused).
#[allow(unused_imports)]
pub use blueprint::*;
#[allow(unused_imports)]
pub use error::*;
#[allow(unused_imports)]
pub use load::*;
#[allow(unused_imports)]
pub use model::*;
#[allow(unused_imports)]
pub use resolve::InterpolationError;
