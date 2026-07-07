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
    "llm_complementary_reviewer",
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
    "__host_agent_capture_events",
    "__host_agent_daemon_snapshot",
    "__host_agent_daemon_wait",
    "__host_agent_dispatch_tool_batch",
    "__host_agent_dispatch_tool_call",
    "__host_agent_emit_event",
    "__host_agent_parse_tool_calls",
    "__host_agent_reminder_providers_fire",
    "__host_agent_record_compaction",
    "__host_agent_record_native_tool_fallback",
    "__host_agent_session_active_skills",
    "__host_agent_session_apply_reminder_post_turn",
    "__host_agent_session_claim_tool_format",
    "__host_agent_session_compact_if_needed",
    "__host_agent_session_drain_bridge_injections",
    "__host_agent_session_drain_feedback",
    "__host_agent_session_pending_injections",
    "__host_agent_session_push_bridge_injection",
    "__host_agent_session_push_user_message",
    "__host_agent_session_revoke_reminder",
    "__host_agent_session_finalize",
    "__host_agent_session_init",
    "__host_agent_session_inject_feedback",
    "__host_agent_session_inject_reminder",
    "__host_agent_session_messages",
    "__host_agent_session_pair_orphaned_tool_use",
    "__host_agent_session_pop_last_assistant",
    "__host_agent_session_post_event",
    "__host_agent_session_project_turn",
    "__host_agent_session_record_assistant",
    "__host_agent_session_record_skill_event",
    "__host_agent_session_record_tool_results",
    "__host_agent_session_record_usage",
    "__host_agent_session_replace_messages",
    "__host_agent_session_set_active_skills",
    "__host_agent_session_totals",
    "__host_agent_truncated_tool_call",
    "__host_agent_undispatched_tool_results",
    "__host_autonomy_budget_check",
    "__host_code_mode_run",
    "__host_drain_file_edits",
    "__host_fire_session_hook",
    "__host_llm_stream_collect",
    "__host_llm_usage_delta",
    "__host_llm_usage_snapshot",
    "__host_mcp_bootstrap",
    "__host_mcp_disconnect",
    "__host_resume_conditions_parse",
    "__host_settlement_agent_active",
    "__host_stage_execute_once",
    "__host_stage_record_attempt",
    "__host_stage_select_artifacts",
    "__host_skill_score",
    "__host_tool_search_score",
    "__host_top_level_agent_suspend",
    "__host_typed_checkpoint_trace",
    "__host_sub_agent_run",
    "__host_worker_close",
    "__host_worker_list",
    "__host_worker_resume",
    "__host_worker_send_input",
    "__host_worker_spawn",
    "__host_worker_stop",
    "__host_worker_suspend",
    "__host_worker_trigger",
    "__host_worker_wait",
    "__host_workflow_finalize_run",
    "__host_workflow_map_branch_artifact",
    "__host_workflow_map_execute_branch",
    "__host_workflow_map_finalize",
    "__host_workflow_map_plan",
    "__host_workflow_prepare_run",
    "__host_workflow_record_transitions",
    "__host_workflow_stage_complete",
    "__host_workflow_stage_prepare",
    "__harn_with_execution_policy_override",
    "__make_struct",
    "__persona_output_style",
    "__pool_create",
    "__pool_get",
    "__pool_list",
    "__pool_simulate_restart",
    "__pool_size",
    "__pool_snapshot",
    "__pool_submit",
    "__pool_wait",
    "__progress_nudge_text",
    "__range__",
    "__register_persona",
    "__register_step",
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
         `RUNTIME_ONLY_EXCEPTIONS` in this test:\n  {missing:#?}",
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
         add them to `PARSER_ONLY_EXCEPTIONS` in this test:\n  {stale:#?}",
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

#[test]
fn migrated_stdlib_modules_publish_runtime_metadata() {
    let metadata = harn_vm::stdlib::stdlib_builtin_metadata();
    let migrated_categories = [
        ("concurrency", 53usize),
        ("fs", 22usize),
        ("io", 35usize),
        ("tui", 3usize),
    ];

    for (category, expected_count) in migrated_categories {
        let entries = metadata
            .iter()
            .filter(|entry| entry.category() == Some(category))
            .collect::<Vec<_>>();
        assert_eq!(
            entries.len(),
            expected_count,
            "unexpected builtin count for `{category}` category"
        );

        for entry in entries {
            assert!(
                entry.signature().is_some(),
                "{} should carry signature metadata",
                entry.name()
            );
            assert!(
                entry.arity_metadata().is_some(),
                "{} should carry arity metadata",
                entry.name()
            );
            assert!(
                entry.doc().is_some(),
                "{} should carry doc metadata",
                entry.name()
            );
        }
    }

    let names = metadata
        .iter()
        .map(|entry| entry.name())
        .collect::<std::collections::BTreeSet<_>>();
    for name in ["__select_list", "__select_timeout", "__select_try"] {
        assert!(
            harn_parser::is_known_builtin(name),
            "{name} should have a parser signature"
        );
        assert!(
            names.contains(name),
            "{name} should be registered at runtime"
        );
    }
}

/// Anti-drift guard: a `runtime_only` `#[harn_builtin]` def must never share
/// a name with a hand-written static parser entry.
///
/// `runtime_only = true` suppresses the macro's own `BuiltinSignature` from
/// the published registry, so if a static entry carries the same name it
/// becomes a *second*, independently-edited source of truth that the
/// typechecker consults instead — and the two silently drift. That is exactly
/// how the LLM config signatures (`provider_capabilities`, `llm_config`,
/// `llm_rate_limit`, …) drifted before they were migrated to published macro
/// sigs backed by the shared `harn_builtin_meta::shapes` vocabulary.
///
/// If this fails: either drop `runtime_only = true` and delete the redundant
/// static entry from `crates/harn-parser/src/builtin_signatures/signatures/*.rs`
/// (the SSOT migration), or, if the builtin is genuinely host-internal, add it
/// to `RUNTIME_ONLY_EXCEPTIONS` *and* ensure it has no static parser entry.
///
/// Note: published (non-`runtime_only`) macros that still carry a shadowed
/// static entry are pre-existing, benign tech debt — `lookup` prefers the
/// installed macro signature, so there is no enforcement drift. Cleaning those
/// is tracked separately; this guard intentionally covers only the
/// `runtime_only` drift surface.
#[test]
fn runtime_only_builtins_never_shadow_a_static_parser_entry() {
    let static_names: BTreeSet<&str> = harn_parser::static_signature_names().collect();

    let mut collisions: Vec<String> = Vec::new();
    for def in harn_vm::stdlib::macros::ALL_BUILTIN_DEFS.iter() {
        if !def.runtime_only {
            continue;
        }
        let names = std::iter::once(def.sig.name).chain(def.aliases.iter().copied());
        for name in names {
            if static_names.contains(name) {
                collisions.push(name.to_string());
            }
        }
    }
    collisions.sort();
    collisions.dedup();

    assert!(
        collisions.is_empty(),
        "These builtins are `runtime_only` (macro signature suppressed) yet still \
         have a static parser entry — the silent-drift surface this guard prevents. \
         Drop `runtime_only` and delete the static entry, or keep it host-internal \
         with no static entry:\n  {collisions:#?}",
    );
}

#[test]
fn linkme_distributed_slice_populates_with_all_builtins() {
    let linkme_count = harn_vm::stdlib::macros::ALL_BUILTIN_DEFS.len();
    let manual_count = harn_vm::stdlib::all_builtin_defs().len();
    assert!(
        linkme_count > 0,
        "linkme distributed slice ALL_BUILTIN_DEFS is empty — likely rlib dead-code stripping, see linkme issue #36"
    );
    assert_eq!(
        linkme_count, manual_count,
        "linkme slice and manual aggregator out of sync: linkme={linkme_count}, manual={manual_count}"
    );
}

/// Shift-left guard for the "declared but never installed" builtin footgun.
///
/// `#[harn_builtin]` auto-adds every annotated fn to the linkme
/// `ALL_BUILTIN_DEFS` slice, but *installing* it onto a live VM still runs
/// through hand-maintained `register_*` functions (the
/// `LLM_RUNTIME_PRIMITIVE_BUILTINS` array in `crates/harn-vm/src/llm/mod.rs`,
/// `register_agent_session_host_primitives`, the per-module
/// `register_*_builtins`, …). A def can therefore sit in `ALL_BUILTIN_DEFS` —
/// and satisfy every parser-alignment test above — yet never be wired into the
/// runtime dispatch table. Calling it then throws `Undefined builtin: X`, which
/// the agent loop's outer `try {}` swallows, leaving the feature silently inert
/// while its status still reports "done".
///
/// That is exactly how `__host_agent_undispatched_tool_results` shipped broken
/// (fixed in #3835): the def existed, the parser knew its name (it even sat in
/// `RUNTIME_ONLY_EXCEPTIONS` above), but it was missing from
/// `LLM_RUNTIME_PRIMITIVE_BUILTINS`, so no live VM could dispatch it. None of
/// the pre-existing alignment tests model runtime *installation* — they align
/// the parser registry against `stdlib_builtin_names()`, which is itself
/// derived from an already-installed probe VM, so a never-installed def is
/// invisible to them. This test closes that gap by walking `ALL_BUILTIN_DEFS`
/// (the macro's own source of truth) against the installed set.
#[test]
fn every_runtime_handler_builtin_is_installed_on_a_full_vm() {
    use harn_vm::stdlib::macros::VmBuiltinHandler;

    // `stdlib_builtin_names()` is derived from a fully-configured stdlib VM
    // (core + io + agent + llm), i.e. the exact production registration path.
    let installed: BTreeSet<String> = harn_vm::stdlib::stdlib_builtin_names()
        .into_iter()
        .collect();

    let mut missing: Vec<String> = Vec::new();
    for def in harn_vm::stdlib::macros::ALL_BUILTIN_DEFS.iter() {
        // Only defs with a real runtime handler are meant to be dispatchable.
        // Parser-only defs (`VmBuiltinHandler::None`, always `parser_only`)
        // resolve via method dispatch / opcodes and are never installed.
        let has_runtime_handler = matches!(
            def.handler,
            VmBuiltinHandler::Sync(_) | VmBuiltinHandler::Async(_)
        );
        if !has_runtime_handler {
            continue;
        }
        for name in std::iter::once(def.sig.name).chain(def.aliases.iter().copied()) {
            if !installed.contains(name) {
                missing.push(name.to_string());
            }
        }
    }
    missing.sort();
    missing.dedup();

    assert!(
        missing.is_empty(),
        "These `#[harn_builtin]` defs carry a runtime handler and land in \
         `ALL_BUILTIN_DEFS`, but are NOT installed on a fully-configured stdlib \
         VM — every call throws `Undefined builtin` at runtime (silently \
         swallowed by the agent loop's outer `try`). Wire each into the matching \
         `register_*` function — e.g. add its `_DEF` to \
         `LLM_RUNTIME_PRIMITIVE_BUILTINS` in `crates/harn-vm/src/llm/mod.rs`:\n  \
         {missing:#?}",
    );
}
