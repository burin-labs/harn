//! Cross-crate drift test for the builtin signature registry.
//!
//! The parser's static analyzer needs to know every builtin the VM
//! registers at runtime — otherwise typo suggestions, return-type inference,
//! and arity checks silently miss new builtins. Historically these lists
//! lived in two places (`typechecker::is_builtin`, `typechecker::builtin_return_type`)
//! and drifted with every stdlib change.
//!
//! Since v0.5.38 the parser has a single alphabetical registry at
//! `harn_parser::builtin_signatures`. This test enforces *bidirectional*
//! alignment between that registry and the VM's runtime truth:
//!
//! 1. Every name the runtime registers must appear in the parser registry
//!    (catches "added a VM builtin, forgot to tell the parser").
//! 2. Every name in the parser registry must still be registered by the
//!    runtime (catches "removed a VM builtin, left a dead parser entry").
//!
//! A handful of parser entries are legitimately parser-only (polymorphic
//! method-style calls like `len`, `starts_with`, `contains` that resolve
//! through method dispatch rather than the registered builtin table). They
//! are listed in [`PARSER_ONLY_EXCEPTIONS`] below.

use std::collections::BTreeSet;

/// Builtins that appear in the parser registry but are not registered with
/// the VM's `builtin_names()` because they resolve through method dispatch,
/// opcode handling, or are registered as math constants rather than through
/// the builtin table. Keep this list as small as possible — prefer
/// registering the name on both sides.
const PARSER_ONLY_EXCEPTIONS: &[&str] = &[
    // Method-style builtins that parse as free functions for type inference
    // but dispatch via method lookup at runtime.
    "contains",
    "ends_with",
    "extname",
    "len",
    "replace",
    "split",
    "starts_with",
    "substring",
    // Math constants that appear in `builtin_return_type` as `float` but
    // are registered at runtime as constants via a different mechanism than
    // `builtin_names()`. Treated as parser-only until the runtime
    // registration is normalized.
    "e",
    "pi",
    // Namespace globals can be called through dotted members, but the
    // namespace itself is not a builtin-table function.
    "stream",
];

/// Names returned by `stdlib_builtin_names()` that are legitimately NOT
/// user-callable builtins — they are compiler-synthesized helpers (sigil
/// prefix `__`), enum variant constructors (`Ok`, `Err`), or opcode
/// keywords that the linter tracks separately from the parser's
/// builtin registry.
const RUNTIME_ONLY_EXCEPTIONS: &[&str] = &[
    "Err",
    "Ok",
    "__assert_dict",
    "__assert_interface",
    "__assert_list",
    "__assert_schema",
    "__assert_shape",
    "__agent_state_delete",
    "__agent_state_handoff",
    "__agent_state_init",
    "__agent_state_list",
    "__agent_state_read",
    "__agent_state_resume",
    "__agent_state_write",
    "__dict_rest",
    "__memory_forget",
    "__memory_recall",
    "__memory_store",
    "__memory_summarize",
    "__make_struct",
    "__range__",
    "__select_list",
    "__select_timeout",
    "__select_try",
];

#[test]
fn every_runtime_builtin_has_a_parser_signature() {
    let runtime: BTreeSet<String> = harn_vm::stdlib::stdlib_builtin_names()
        .into_iter()
        .collect();
    let exceptions: BTreeSet<&str> = RUNTIME_ONLY_EXCEPTIONS.iter().copied().collect();

    let missing: Vec<&String> = runtime
        .iter()
        .filter(|name| !harn_parser::is_known_builtin(name) && !exceptions.contains(name.as_str()))
        .collect();

    assert!(
        missing.is_empty(),
        "The VM registers these builtins but the parser has no signature for them.\n\
         Add them to `crates/harn-parser/src/builtin_signatures.rs` (alphabetical),\n\
         or if they are compiler-synthesized helpers add them to\n\
         `RUNTIME_ONLY_EXCEPTIONS` in this test:\n  {:#?}",
        missing,
    );
}

#[test]
fn every_parser_builtin_exists_at_runtime() {
    let runtime: BTreeSet<String> = harn_vm::stdlib::stdlib_builtin_names()
        .into_iter()
        .collect();
    let exceptions: BTreeSet<&str> = PARSER_ONLY_EXCEPTIONS.iter().copied().collect();

    let stale: Vec<&str> = harn_parser::known_builtin_names()
        .filter(|name| !runtime.contains(*name) && !exceptions.contains(name))
        .collect();

    assert!(
        stale.is_empty(),
        "The parser registry has entries that no longer exist at runtime.\n\
         Either remove them from `crates/harn-parser/src/builtin_signatures.rs`\n\
         or, if they're intentionally parser-only (e.g. polymorphic method calls),\n\
         add them to `PARSER_ONLY_EXCEPTIONS` in this test:\n  {:#?}",
        stale,
    );
}
