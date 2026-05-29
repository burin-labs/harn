//! Parser-facing signatures for the LLM builtins the typechecker treats as
//! first-class (boundary-source tracking + structured-output schema typing).
//!
//! These are the only LLM builtins `harn-parser` references directly — in its
//! own unit tests, which run without a driver and so cannot see the
//! `#[harn_builtin]` registry that harn-vm installs at startup. Rather than
//! hand-maintain a second definition here (the duplication that let LLM
//! signatures drift), each entry is the *same* `const` that the harn-vm macro
//! publishes via `sig_expr`. The definition lives once in
//! [`harn_builtin_meta::signatures`]; this table is a thin re-list, so the two
//! sides cannot diverge.
//!
//! Every other migrated LLM/agent builtin is published by the macro alone (no
//! static entry) — see `crates/harn-vm/src/llm/*.rs`.

use super::BuiltinSignature;

pub(crate) const SIGNATURES: &[BuiltinSignature] = &[
    harn_builtin_meta::signatures::LLM_CALL,
    harn_builtin_meta::signatures::LLM_CALL_SAFE,
    harn_builtin_meta::signatures::LLM_COMPLETION,
    harn_builtin_meta::signatures::LLM_CALL_STRUCTURED,
    harn_builtin_meta::signatures::LLM_CALL_STRUCTURED_SAFE,
    harn_builtin_meta::signatures::LLM_CALL_STRUCTURED_RESULT,
    harn_builtin_meta::signatures::SCHEMA_RECOVER,
    // Reachable from the parser via `harness.llm.{catalog,providers}` method
    // arity checking (see `harness_methods::harness_llm_ambient`).
    harn_builtin_meta::signatures::LLM_CATALOG,
    harn_builtin_meta::signatures::LLM_PROVIDER_STATUS,
];
