//! Harn IR: the handler graph the invariant checks run over.
//!
//! Wiring only. `types` holds the data model, `builder` (with
//! `builder_expr`) turns an AST handler into a graph, `classify` reads
//! meaning out of call sites, `spec_parse` turns `@invariant(...)`
//! attributes into checks, `invariants` is those checks, and `analysis` is
//! the entry point that runs them.
//!
//! Each module is re-exported at the crate root, so the public surface is
//! unchanged: callers keep writing `harn_ir::Capability`.

mod analysis;
mod builder;
mod builder_expr;
mod classify;
mod invariants;
mod spec_parse;
mod types;

pub use analysis::*;
pub(crate) use builder::*;
pub use classify::literal_value;
pub use invariants::*;
pub use types::*;

#[cfg(test)]
mod tests;
