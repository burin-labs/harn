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

const LLM_CONFIG_BUILTINS: &[&str] = &[
    "llm_available_providers",
    "llm_config",
    "llm_healthcheck",
    "llm_infer_provider",
    "llm_known_models",
    "llm_model_info",
    "llm_model_tier",
    "llm_pick_model",
    "llm_provider_catalog",
    "llm_providers",
    "llm_qc_default_model",
    "llm_rate_limit",
    "llm_resolve_model",
    "provider_capabilities",
    "provider_capabilities_clear",
    "provider_capabilities_install",
    "provider_register",
];

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
    "__cost_route",
    "__dict_rest",
    "__host_agent_budget_pre_call_blocked",
    "__host_agent_build_turn_system",
    "__host_agent_capture_events",
    "__host_agent_dispatch_tool_batch",
    "__host_agent_dispatch_tool_call",
    "__host_agent_parse_tool_calls",
    "__host_agent_session_active_skills",
    "__host_agent_session_compact_if_needed",
    "__host_agent_session_drain_feedback",
    "__host_agent_session_finalize",
    "__host_agent_session_init",
    "__host_agent_session_inject_feedback",
    "__host_agent_session_messages",
    "__host_agent_session_record_assistant",
    "__host_agent_session_record_skill_event",
    "__host_agent_session_record_tool_results",
    "__host_agent_session_record_usage",
    "__host_agent_session_set_active_skills",
    "__host_agent_session_totals",
    "__host_mcp_bootstrap",
    "__host_mcp_disconnect",
    "__host_skill_score",
    "__host_sub_agent_run",
    "__host_worker_close",
    "__host_worker_list",
    "__host_worker_resume",
    "__host_worker_send_input",
    "__host_worker_spawn",
    "__host_worker_trigger",
    "__host_worker_wait",
    "__host_workflow_execute_stage",
    "__host_workflow_finalize_run",
    "__host_workflow_map_branch_artifact",
    "__host_workflow_map_execute_branch",
    "__host_workflow_map_finalize",
    "__host_workflow_map_plan",
    "__host_workflow_prepare_run",
    "__host_workflow_record_transitions",
    "__memory_forget",
    "__memory_recall",
    "__memory_store",
    "__memory_summarize",
    "__make_struct",
    "__range__",
    "__register_step",
    "__select_list",
    "__select_timeout",
    "__select_try",
    "__testing_call_body",
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

#[test]
fn llm_config_builtins_publish_runtime_metadata() {
    let metadata = harn_vm::stdlib::stdlib_builtin_metadata()
        .into_iter()
        .map(|entry| (entry.name().to_string(), entry))
        .collect::<std::collections::BTreeMap<_, _>>();

    for name in LLM_CONFIG_BUILTINS {
        let entry = metadata
            .get(*name)
            .unwrap_or_else(|| panic!("{name} must be registered"));
        assert!(
            entry.signature().is_some(),
            "{name} should carry registration metadata"
        );
        assert!(
            entry.category().is_some(),
            "{name} should carry category metadata"
        );
        assert!(entry.doc().is_some(), "{name} should carry doc metadata");
    }

    assert_eq!(
        metadata
            .get("llm_healthcheck")
            .expect("llm_healthcheck metadata")
            .kind(),
        harn_vm::VmBuiltinKind::Async
    );
    assert_eq!(
        metadata
            .get("llm_rate_limit")
            .expect("llm_rate_limit metadata")
            .category(),
        Some("llm.rate_limit")
    );
}
