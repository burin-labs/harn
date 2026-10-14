//! Prepared-run authority reconciliation.
//!
//! [`PreparedRun`] is the external seam: callers provide a value-free
//! [`RunIntent`] and observed [`HostFacts`], then receive either a ready
//! [`AuthorityLease`], one batched approval request, or actionable blocking
//! diagnostics. Execution consumes the lease through the same canonical
//! evaluators used during preparation and persists terminal authority evidence.

mod contracts;
mod discovery;
mod engine;
mod evidence;
mod identity;
mod receipt;
mod session;
mod session_bridge;

pub use contracts::*;
pub use discovery::*;
pub use engine::*;
pub use evidence::requirement_fingerprint;
pub use identity::*;
pub use receipt::*;
pub use session::*;
pub use session_bridge::request_session_approval;

#[cfg(test)]
mod tests;
