//! Namespace-grouped builtin signature tables.

mod agents;
mod integrations;
mod project;
mod schema;
mod stdlib;
mod workflow;

pub(crate) use super::{
    BuiltinReturn, BuiltinSig, UNION_BYTES_NIL, UNION_DICT_NIL, UNION_STRING_NIL,
};

pub(crate) fn groups() -> [&'static [BuiltinSig]; 6] {
    [
        stdlib::SIGNATURES,
        agents::SIGNATURES,
        integrations::SIGNATURES,
        project::SIGNATURES,
        schema::SIGNATURES,
        workflow::SIGNATURES,
    ]
}
