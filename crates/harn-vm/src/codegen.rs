//! Closed generator input and one-shot plugin data contracts.

mod compiler;
mod fixtures;
pub use fixtures::{codegen_names_fixture, CODEGEN_NAMES_FIXTURE_ARTIFACT};
mod contract;
pub(crate) mod identity;
mod projections;
mod source_refs;
mod validation;

pub use compiler::{compile_catalog, compile_catalog_with_sources};
pub use contract::*;
pub use projections::*;
pub use source_refs::normalize_catalog_sources;
pub use validation::{validate_ir, validate_response};

#[cfg(test)]
mod tests;
