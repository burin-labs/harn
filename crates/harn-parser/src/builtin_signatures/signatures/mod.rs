//! Namespace-grouped builtin signature tables.

mod agents;
mod flow;
mod integrations;
mod project;
mod schema;
mod stdlib;
mod workflow;

pub(crate) use super::{
    BuiltinReturn, BuiltinSig, UNION_BYTES_NIL, UNION_DICT_NIL, UNION_STRING_NIL,
};

pub(crate) fn groups() -> [&'static [BuiltinSig]; 7] {
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
