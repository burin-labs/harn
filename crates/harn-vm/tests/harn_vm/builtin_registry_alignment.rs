//! Cross-crate drift test for the typed builtin contract registry.
//!
//! The parser's static analyzer needs to know every builtin the VM
//! registers at runtime — otherwise typo suggestions, return-type inference,
//! and arity checks silently miss new builtins. Historically these lists
//! lived in two places (`typechecker::is_builtin`, `typechecker::builtin_return_type`)
//! and drifted with every stdlib change.
//!
//! Source exposure is no longer equivalent to runtime registration: Harness
//! methods and privileged/runtime-only handlers deliberately exist in the VM
//! without becoming ambient global functions. These tests align the runtime
//! and parser through the typed manifest rather than comparing two
//! unqualified name sets.
//!
//! Ordinary global/capability-function entries must have both a runtime
//! implementation and a parser projection. Harness methods are checked by
//! their nominal capability contract instead of by their hidden handler name.

use std::collections::BTreeSet;

#[test]
fn every_runtime_builtin_has_a_parser_signature() {
    let runtime: BTreeSet<String> = harn_vm::stdlib::stdlib_builtin_names()
        .into_iter()
        .collect();
    let missing: Vec<&str> = harn_vm::stdlib::all_builtin_manifest()
        .iter()
        .filter(|entry| {
            matches!(
                entry.contract.exposure,
                harn_builtin_meta::BuiltinExposure::PureGlobal
                    | harn_builtin_meta::BuiltinExposure::CapabilityFunction { .. }
            )
        })
        .map(|entry| entry.name)
        .filter(|name| !runtime.contains(*name) || !harn_parser::is_known_builtin(name))
        .collect();

    assert!(
        missing.is_empty(),
        "ordinary source-visible manifest entries must be installed and \
         parser-visible; missing={missing:#?}",
    );
}

#[test]
fn every_source_manifest_builtin_exists_at_runtime() {
    let runtime: BTreeSet<String> = harn_vm::stdlib::stdlib_builtin_names()
        .into_iter()
        .collect();
    let missing: Vec<&str> = harn_vm::stdlib::all_builtin_manifest()
        .iter()
        .filter(|entry| {
            matches!(
                entry.contract.exposure,
                harn_builtin_meta::BuiltinExposure::PureGlobal
                    | harn_builtin_meta::BuiltinExposure::CapabilityFunction { .. }
            )
        })
        .map(|entry| entry.name)
        .filter(|name| !runtime.contains(*name))
        .collect();

    assert!(
        missing.is_empty(),
        "source-visible manifest entries missing runtime handlers: {missing:#?}",
    );
}

#[test]
fn llm_config_builtins_publish_runtime_metadata() {
    let metadata = harn_vm::stdlib::stdlib_builtin_metadata()
        .into_iter()
        .map(|entry| (entry.name().to_string(), entry))
        .collect::<std::collections::BTreeMap<_, _>>();

    let expected = harn_vm::stdlib::macros::ALL_BUILTIN_DEFS
        .iter()
        .filter(|def| def.category == Some("llm.config"))
        .filter(|def| !matches!(def.handler, harn_vm::stdlib::macros::VmBuiltinHandler::None))
        .map(|def| def.sig.name)
        .collect::<BTreeSet<_>>();
    assert!(!expected.is_empty(), "llm.config definitions must exist");

    for name in expected {
        let entry = metadata
            .get(name)
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
    let migrated_categories = ["concurrency", "fs", "io", "tui"];

    for category in migrated_categories {
        let entries = metadata
            .iter()
            .filter(|entry| entry.category() == Some(category))
            .collect::<Vec<_>>();
        let actual_names = entries
            .iter()
            .map(|entry| entry.name())
            .collect::<BTreeSet<_>>();
        let expected_names = harn_vm::stdlib::macros::ALL_BUILTIN_DEFS
            .iter()
            .filter(|def| def.category == Some(category))
            .filter(|def| !matches!(def.handler, harn_vm::stdlib::macros::VmBuiltinHandler::None))
            .flat_map(|def| std::iter::once(def.sig.name).chain(def.aliases.iter().copied()))
            .collect::<BTreeSet<_>>();
        assert!(
            !expected_names.is_empty(),
            "migrated builtin category `{category}` must retain at least one runtime definition"
        );
        assert_eq!(
            actual_names, expected_names,
            "runtime metadata for `{category}` must match its builtin definitions"
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
/// (the SSOT migration), or keep it host-internal with no static parser entry.
///
/// Note: published (non-`runtime_only`) macros that still carry a shadowed
/// static entry are pre-existing, benign tech debt — `lookup` prefers the
/// installed macro signature, so there is no enforcement drift. Cleaning those
/// is tracked separately; this guard intentionally covers only the
/// `runtime_only` drift surface.
#[test]
fn runtime_only_builtins_never_shadow_a_static_parser_entry() {
    // `Ok` and `Err` are language-owned enum constructors. The parser owns
    // their generic typing contract while the VM supplies the constructor
    // implementation, so they are the deliberate exception to the
    // runtime-builtin ownership rule below.
    const LANGUAGE_INTRINSIC_IMPLEMENTATIONS: &[&str] = &["Err", "Ok"];
    let static_names: BTreeSet<&str> = harn_parser::static_signature_names().collect();

    let mut collisions: Vec<String> = Vec::new();
    for def in harn_vm::stdlib::macros::ALL_BUILTIN_DEFS.iter() {
        if !def.runtime_only {
            continue;
        }
        let names = std::iter::once(def.sig.name).chain(def.aliases.iter().copied());
        for name in names {
            if static_names.contains(name) && !LANGUAGE_INTRINSIC_IMPLEMENTATIONS.contains(&name) {
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

/// Every parser-recognized fallback name must resolve to an installed runtime
/// contract or to a mechanical typed-Harness migration. Otherwise the parser
/// advertises a source API that the compiler deliberately refuses to execute.
#[test]
fn static_parser_signatures_have_runtime_owners_or_migrations() {
    // These signatures describe named exports from typed stdlib modules. They
    // become callable only after import creates a local binding, so they are
    // neither ambient APIs nor runtime-registry entries.
    const STATIC_MODULE_EXPORTS: &[&str] = &[
        "agent_chat_route_input",
        "agent_chat_wait_for_user_tools",
        "agent_preset",
        "agent_preset_kinds",
        "agent_preset_register",
        "agent_typed_output_checkpoint",
        "agentic_user",
        "fixture_user",
        "runtime_introspection_tools",
        "scripted_user",
        "simulated_user_post_turn",
        "simulated_user_read_tools",
        "simulated_user_respond",
        "simulated_user_status",
        "transcript.clear_reminders",
        "transcript.inject_reminder",
        "user_tools",
        "workflow_typed_output_checkpoint",
    ];
    let has_legacy_capability_migration = |name| {
        let diagnostic = harn_parser::diagnostic::harness_clock_replacement(name)
            .or_else(|| harn_parser::diagnostic::harness_stdio_replacement(name))
            .or_else(|| harn_parser::diagnostic::harness_fs_replacement(name))
            .or_else(|| harn_parser::diagnostic::harness_env_replacement(name))
            .or_else(|| harn_parser::diagnostic::harness_random_replacement(name))
            .or_else(|| harn_parser::diagnostic::harness_net_replacement(name));
        diagnostic.is_some() || harn_vm::stdlib::harness_migration_for_builtin(name).is_some()
    };
    let manifest_names: BTreeSet<&str> = harn_vm::stdlib::all_builtin_manifest()
        .iter()
        .map(|entry| entry.name)
        .collect();
    let mut missing: Vec<&str> = harn_parser::static_signature_names()
        .filter(|name| {
            !manifest_names.contains(name)
                && !has_legacy_capability_migration(name)
                && !harn_parser::builtin_signatures::is_language_intrinsic(name)
                && !name.starts_with("__")
                && !matches!(*name, "e" | "pi")
                && !STATIC_MODULE_EXPORTS.contains(name)
        })
        .collect();
    missing.sort_unstable();
    missing.dedup();

    assert!(
        missing.is_empty(),
        "parser-only builtin signatures advertise APIs with no runtime owner or typed-Harness migration:\n  {missing:#?}"
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
/// (fixed in #3835): the def existed and the parser knew its name, but it was missing from
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
