//! Prepared public-export wire types shared by runtime adapters and generators.

mod codec;
pub(crate) mod graph;
pub(crate) mod identity;
pub(crate) mod metadata;
pub(crate) mod schema;
pub(crate) mod scope;
mod type_expr;
mod types;

pub use codec::{
    decimal_wire_pattern, ENUM_FIELDS_KEY, ENUM_NAME_KEY, ENUM_VARIANT_KEY, MAX_WIRE_VALUE_DEPTH,
};
pub use graph::{PreparedWireGraph, WIRE_INTEGER_TOKEN_PATTERN};
pub use scope::{declaration_name, ResolvedWireDeclaration, ResolvedWireScope};
pub use type_expr::PreparedWireType;
pub use types::*;

#[cfg(test)]
mod tests;
