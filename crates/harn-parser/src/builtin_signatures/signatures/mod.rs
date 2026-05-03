//! Namespace-grouped builtin signature tables.
//!
//! Each group is a `&'static [BuiltinSignature]` of entries describing one
//! conceptual area (core stdlib, agents, flow predicates, integrations,
//! etc.). They are concatenated and alphabetically sorted by
//! [`super::all_signatures`] so binary-search lookups stay O(log N).

mod agents;
mod flow;
mod integrations;
mod project;
mod schema;
mod stdlib;
mod workflow;

pub(crate) use super::types::{
    BuiltinSignature, Param, ShapeFieldDescriptor, Ty, TY_ANY, TY_BOOL, TY_BYTES, TY_BYTES_OR_NIL,
    TY_CLOSURE, TY_DICT, TY_DICT_OR_NIL, TY_DURATION, TY_FLOAT, TY_INT, TY_LIST, TY_NEVER, TY_NIL,
    TY_NUMBER, TY_STRING, TY_STRING_OR_NIL,
};

pub(crate) fn groups() -> [&'static [BuiltinSignature]; 7] {
    [
        stdlib::SIGNATURES,
        agents::SIGNATURES,
        flow::SIGNATURES,
        integrations::SIGNATURES,
        project::SIGNATURES,
        schema::SIGNATURES,
        workflow::SIGNATURES,
    ]
}
