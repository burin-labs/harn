//! Namespace-grouped builtin signature tables.
//!
//! Each group is a `&'static [BuiltinSignature]` of entries describing one
//! conceptual area (core stdlib, agents, flow predicates, integrations,
//! etc.). [`super::lookup`] scans the installed slice first, then walks
//! these groups linearly — O(N+M) per call but well below the typecheck
//! hot path.

mod agents;
mod integrations;
mod llm;
mod project;
mod shapes;
mod stdlib;
mod workflow;

pub(crate) use super::types::{
    BuiltinSignature, Param, Ty, TY_ANY, TY_BOOL, TY_BYTES, TY_BYTES_OR_NIL, TY_CLOSURE, TY_DICT,
    TY_DICT_OR_NIL, TY_DURATION, TY_FLOAT, TY_INT, TY_LIST, TY_NEVER, TY_NIL, TY_NUMBER, TY_STRING,
    TY_STRING_OR_NIL,
};

pub(crate) fn groups() -> [&'static [BuiltinSignature]; 6] {
    [
        stdlib::SIGNATURES,
        agents::SIGNATURES,
        integrations::SIGNATURES,
        llm::SIGNATURES,
        project::SIGNATURES,
        workflow::SIGNATURES,
    ]
}
