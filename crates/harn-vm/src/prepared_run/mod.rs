//! Prepared-run authority reconciliation.
//!
//! [`PreparedRun`] is the external seam: callers provide a value-free
//! [`RunIntent`] and observed [`HostFacts`], then receive either a ready
//! [`AuthorityLease`], one batched approval request, or actionable blocking
//! diagnostics. Execution consumes the lease through the same canonical
//! evaluators used during preparation and persists terminal authority evidence.

mod contracts;
mod engine;
mod receipt;

pub use contracts::*;
pub use engine::*;
pub use receipt::*;

#[cfg(test)]
mod tests;
