//! Standard library builtins for the Harn VM.
//!
//! Every builtin is declared with the `#[harn_builtin]` proc-macro
//! (`crate::stdlib::macros::harn_builtin`). Each annotation emits a sibling
//! `static <FN>_DEF: VmBuiltinDef` carrying the signature, aliases, handler,
//! and metadata, and registers it into the workspace-global
//! [`macros::ALL_BUILTIN_DEFS`] distributed slice at link time. The CLI / LSP /
//! lint / serve / dap binaries call [`force_link`] to defeat rlib dead-code
//! stripping (linkme issue #36) so every static lands in the slice. Modules
//! still expose a `register_<module>_builtins(vm)` helper for ordered eager
//! registration (e.g. so `clock::timestamp` can override `process::timestamp`).
//! `register_vm_stdlib` calls those helpers in order and then installs the
//! aggregated signatures into the parser registry.
//!
//! See `CONTRIBUTING.md` ("Adding a stdlib builtin") for the full template.

pub mod macros;

mod agent_sessions;
pub mod agent_state;
pub(crate) mod agents;
mod agents_daemon;
mod artifact_emit;
pub(crate) mod assemble;
pub mod asset_paths;
mod bytes;
mod calendar;
mod channel_guardrails;
mod channels;
pub(crate) mod clock;
pub(crate) mod collections;
mod command_policy;
pub(crate) mod compaction;
#[cfg(feature = "compression")]
mod compression;
mod concurrency;
pub(crate) use concurrency::cancelled_vm_error;
mod connectors;
mod cookies;
mod cron;
mod crypto;
mod csv;
mod datetime;
mod diff;
pub(crate) use datetime::date_dict_from_millis;
#[cfg(feature = "content")]
mod document;
mod durable_step;
mod event_log;
pub use event_log::mint_hypothesis_native_attestation;
mod external_agent;
pub(crate) mod files;
mod flow;
pub(crate) mod fs;
mod git;
pub(crate) mod git_topology;
mod grounding;
pub(crate) mod harn_entry;
pub(crate) mod hitl;
mod hitl_read;
pub mod host;
pub mod http_response;
pub(crate) mod io;
mod iter;
pub(crate) mod json;
mod json_query;
pub(crate) mod json_stream;
mod jsonrpc;
mod junit;
mod lifecycle_receipts;
mod logging;
pub mod long_running;
mod math;
pub(crate) use math::call_seeded_random_method;
pub(crate) mod memory;
mod monitors;
mod multipart;
mod net;
mod net_policy;
mod oauth_dynreg;
mod oauth_storage;
pub(crate) mod observability;
pub(crate) mod options;
mod package_snapshot;
pub(crate) use package_snapshot::PackageSnapshotRegistry;
mod path;
pub(crate) mod path_scope_guard;
pub(crate) mod pool;
mod portable;
#[cfg(feature = "postgres")]
mod postgres;
#[cfg(feature = "postgres")]
pub use postgres::install_shared_pool_registry;
pub mod process;
pub(crate) mod process_spawn;
mod project;
mod project_catalog;
mod project_enrich;
mod regex;
mod review;
mod runtime_scope;
pub(crate) mod sandbox;
pub mod secret_scan;
pub(crate) mod session_store;
mod sets;
pub(crate) mod shapes;
mod skills;
#[cfg(feature = "sqlite")]
mod sqlite;
pub(crate) mod strings;
pub(crate) mod supervisor;
pub mod template;
mod testbench;
mod testing;
mod timing;
pub mod token_redaction;
pub(crate) mod tool_hooks;
pub(crate) mod tools;
pub mod tracing;
mod transcript_compact;
pub(crate) mod transcript_project;
mod triggers_stdlib;
mod tui;
mod types;
mod url_parse;
mod vision;
pub(crate) mod waitpoint;
#[cfg(feature = "content")]
mod web;
pub mod workflow_messages;
pub(crate) mod xml;

use crate::http::register_http_builtins;
use crate::llm::register_llm_builtins;
use crate::mcp::register_mcp_builtins;
use crate::mcp_server::register_mcp_server_builtins;
use crate::vm::Vm;

pub(crate) use crate::schema::{json_to_vm_value, schema_result_value};
pub(crate) fn set_thread_source_dir(dir: &std::path::Path) {
    process::set_thread_source_dir(dir);
}

/// Register core builtins: pure/deterministic, no I/O.
pub fn register_core_stdlib(vm: &mut Vm) {
    types::register_type_builtins(vm);
    math::register_math_builtins(vm);
    strings::register_string_builtins(vm);
    json::register_json_builtins(vm);
    json_stream::register_json_stream_builtins(vm);
    xml::register_xml_builtins(vm);
    datetime::register_datetime_builtins(vm);
    diff::register_diff_builtins(vm);
    #[cfg(feature = "content")]
    document::register_document_builtins(vm);
    calendar::register_calendar_builtins(vm);
    cron::register_cron_builtins(vm);
    regex::register_regex_builtins(vm);
    bytes::register_bytes_builtins(vm);
    #[cfg(feature = "compression")]
    compression::register_compression_builtins(vm);
    command_policy::register_command_policy_builtins(vm);
    runtime_scope::register_runtime_scope_builtins(vm);
    crypto::register_crypto_builtins(vm);
    csv::register_csv_builtins(vm);
    junit::register_junit_builtins(vm);
    multipart::register_multipart_builtins(vm);
    url_parse::register_url_builtins(vm);
    #[cfg(feature = "content")]
    web::register_web_builtins(vm);
    cookies::register_cookie_builtins(vm);
    path::register_path_helper_builtins(vm);
    sets::register_set_builtins(vm);
    collections::register_collection_builtins(vm);
    iter::register_iter_builtins(vm);
    event_log::register_event_log_builtins(vm);
    durable_step::register_durable_step_builtins(vm);
    channels::register_channel_builtins(vm);
    channel_guardrails::register_channel_guardrail_builtins(vm);
    shapes::register_shape_builtins(vm);
    testing::register_testing_builtins(vm);
    flow::register_flow_builtins(vm);
    lifecycle_receipts::register_lifecycle_receipt_builtins(vm);
    net_policy::register_net_policy_builtins(vm);
    http_response::register_http_response_builtins(vm);
    portable::register_portable_builtins(vm);
}

/// Register I/O builtins (requires OS access).
pub fn register_io_stdlib(vm: &mut Vm) {
    io::register_io_builtins(vm);
    host::register_host_builtins(vm);
    fs::register_fs_builtins(vm);
    package_snapshot::register_package_snapshot_builtins(vm);
    files::register_file_builtins(vm);
    git::register_git_builtins(vm);
    vision::register_vision_builtins(vm);
    agent_state::register_agent_state_builtins(vm);
    memory::register_memory_builtins(vm);
    session_store::register_session_store_builtins(vm);
    net::register_net_builtins(vm);
    process::register_process_builtins(vm);
    process::register_path_builtins(vm);
    sandbox::register_sandbox_builtins(vm);
    // Clock builtins overlay process::timestamp/elapsed so they honor
    // mock_time / advance_time. Register AFTER process to take precedence.
    clock::register_clock_builtins(vm);
    crate::durable_rate_limit::register_durable_rate_limit_builtins(vm);
    testbench::register_testbench_builtins(vm);
    project::register_project_builtins(vm);
    grounding::register_grounding_builtins(vm);
    tracing::register_tracing_builtins(vm);
    observability::register_observability_builtins(vm);
    timing::register_timing_builtins(vm);
    tui::register_tui_builtins(vm);
}

fn register_agent_stdlib_before_llm(vm: &mut Vm) {
    concurrency::register_concurrency_builtins(vm);
    connectors::register_connector_builtins(vm);
    review::register_review_builtins(vm);
    secret_scan::register_secret_scan_builtins(vm);
    tools::register_tool_builtins(vm);
    tool_hooks::register_tool_hooks_builtins(vm);
    crate::composition::register_composition_builtins(vm);
    skills::register_skill_builtins(vm);
    agents_daemon::register_daemon_builtins(vm);
    triggers_stdlib::register_trigger_builtins(vm);
    #[cfg(feature = "postgres")]
    postgres::register_postgres_builtins(vm);
    #[cfg(feature = "sqlite")]
    sqlite::register_sqlite_builtins(vm);
    monitors::register_monitor_builtins(vm);
    hitl::register_hitl_builtins(vm);
    hitl_read::register_hitl_read_builtins(vm);
    waitpoint::register_waitpoint_builtins(vm);
    supervisor::register_supervisor_builtins(vm);
    agents::register_agent_builtins(vm);
    pool::register_pool_builtins(vm);
    oauth_storage::register_oauth_storage_builtins(vm);
    oauth_dynreg::register_oauth_dynreg_builtins(vm);
    token_redaction::register_token_redaction_builtins(vm);
    agent_sessions::register_agent_session_builtins(vm);
    artifact_emit::register_artifact_emit_builtins(vm);
    external_agent::register_external_agent_builtins(vm);
    path_scope_guard::register_path_scope_guard_builtins(vm);
    workflow_messages::register_workflow_message_builtins(vm);
    transcript_compact::register_transcript_compaction_builtins(vm);
    compaction::register_compaction_builtins(vm);
    transcript_project::register_transcript_projection_builtins(vm);
    assemble::register_assemble_context_builtin(vm);
    crate::egress::register_egress_builtins(vm);
    crate::security::register_security_builtins(vm);
    register_http_builtins(vm);
    jsonrpc::register_jsonrpc_builtins(vm);
}

fn register_agent_stdlib_after_llm(vm: &mut Vm) {
    register_mcp_builtins(vm);
    register_mcp_server_builtins(vm);
    crate::step_runtime::register_step_builtins(vm);
}

/// Register agent builtins (requires network access and async runtime).
pub fn register_agent_stdlib(vm: &mut Vm) {
    register_agent_stdlib_before_llm(vm);
    register_llm_builtins(vm);
    register_agent_stdlib_after_llm(vm);
}

/// Register all standard builtins on a VM (core + io + agent). Also
/// installs the macro-emitted signature slice into the parser registry
/// (idempotent under repeat calls with the same slice pointer).
pub fn register_vm_stdlib(vm: &mut Vm) {
    register_core_stdlib(vm);
    register_io_stdlib(vm);
    register_agent_stdlib(vm);
    vm.project_declared_capability_methods();
    if vm.harness().is_none() {
        vm.set_harness(crate::harness::Harness::real());
    }
    if harn_parser::legacy_ambient_capabilities_enabled() && vm.global("harness").is_none() {
        let harness = vm
            .root_harness_value()
            .expect("register_vm_stdlib installs a root Harness");
        vm.set_global("harness", harness);
    }
    vm.project_legacy_capability_globals();
    harn_builtin_registry::install_builtin_manifest(all_builtin_manifest());
}

pub(crate) fn rebind_execution_state_builtins(vm: &mut Vm) {
    concurrency::register_concurrency_builtins(vm);
}

fn stdlib_probe_vm() -> Vm {
    let mut vm = Vm::new();
    register_vm_stdlib(&mut vm);
    // Name-only/metadata introspection never accesses this path, but passing
    // a real per-platform temp dir keeps registration logic honest if a
    // callee someday validates its parent.
    let tmp = std::env::temp_dir();
    crate::store::register_store_builtins(&mut vm, &tmp);
    crate::checkpoint::register_checkpoint_builtins(&mut vm, &tmp, "default");
    crate::metadata::register_metadata_builtins(&mut vm, &tmp);
    // Install the macro-emitted signatures into the parser registry so any
    // probe-driven name/metadata query (e.g. the alignment test) sees the
    // post-migration sig set. Idempotent under repeat install with the same
    // pointer (which `all_builtin_manifest()` guarantees).
    harn_builtin_registry::install_builtin_manifest(all_builtin_manifest());
    vm
}

/// Aggregate of every `#[harn_builtin]`-emitted `VmBuiltinDef` in the stdlib.
///
/// Backed by the `linkme::distributed_slice` declared on
/// [`crate::stdlib::macros::ALL_BUILTIN_DEFS`] — every annotated fn
/// contributes one entry automatically at link time. Keep builtin registration
/// on this distributed slice instead of per-module arrays plus a central
/// hand-maintained aggregator.
///
/// **Force-link warning** (linkme issue #36): rlib dead-code stripping
/// can drop these statics when `harn-vm` is linked transitively. Every
/// binary that exercises builtins (`harn-cli`, `harn-lsp`, `harn-lint`,
/// `harn-serve`, `harn-dap`) calls [`force_link`] near `main()` to defeat
/// the stripping. The alignment test
/// `linkme_distributed_slice_populates_with_all_builtins` catches a silent
/// regression by asserting the slice is non-empty.
pub fn all_builtin_defs() -> &'static [&'static macros::VmBuiltinDef] {
    let defs = &macros::ALL_BUILTIN_DEFS;
    validate_builtin_contracts(defs);
    defs
}

fn validate_builtin_contracts(defs: &[&macros::VmBuiltinDef]) {
    use harn_builtin_meta::BuiltinExposure;
    let mut source_names = std::collections::BTreeSet::new();
    let mut capability_methods = std::collections::BTreeSet::new();
    for def in defs {
        assert!(
            def.contract.is_declared(),
            "builtin `{}` has no typed exposure/effect contract",
            def.sig.name
        );
        assert!(
            !matches!(def.contract.exposure, BuiltinExposure::PureGlobal)
                || def.contract.effects.is_empty(),
            "ambient global builtin `{}` declares effects; effects must flow through Harness",
            def.sig.name
        );
        if let BuiltinExposure::CapabilityFunction { authority_argument } = def.contract.exposure {
            assert!(
                !def.contract.effects.is_empty(),
                "capability function `{}` must declare effects",
                def.sig.name
            );
            assert!(
                usize::from(authority_argument) < def.sig.params.len(),
                "capability function `{}` authority argument is out of range",
                def.sig.name
            );
        }
        if def.runtime_only {
            assert!(
                matches!(def.contract.exposure, BuiltinExposure::RuntimeInternal),
                "runtime_only builtin `{}` must use runtime_internal exposure",
                def.sig.name
            );
        }
        if let BuiltinExposure::HarnessMethod { method, .. } = def.contract.exposure {
            assert!(
                !method.is_empty(),
                "harness method for `{}` cannot be empty",
                def.sig.name
            );
            let BuiltinExposure::HarnessMethod { capability, method } = def.contract.exposure
            else {
                unreachable!()
            };
            assert!(
                capability_methods.insert((capability, method)),
                "duplicate contract for harness.{}.{}",
                capability.field_name(),
                method
            );
        }
        if matches!(
            def.contract.exposure,
            BuiltinExposure::PureGlobal
                | BuiltinExposure::CapabilityFunction { .. }
                | BuiltinExposure::PrivilegedWire
                | BuiltinExposure::HarnessMethod { .. }
        ) {
            assert!(
                source_names.insert(def.sig.name),
                "duplicate source contract name `{}`",
                def.sig.name
            );
        }
    }
}

/// Force-link entry point: a `pub fn` that touches `ALL_BUILTIN_DEFS` so
/// the linker keeps every `#[harn_builtin]`-emitted static. Drivers
/// (`harn-cli`, `harn-lsp`, etc.) call this once at startup. Doing nothing
/// at runtime is fine — the side effect is purely a link-time signal.
///
/// See [`linkme issue #36`](https://github.com/dtolnay/linkme/issues/36)
/// for why the explicit touch is necessary on every supported target.
pub fn force_link() {
    // `black_box` prevents LLVM from constant-folding the length read away.
    // The `>= 1` guard never trips at runtime but is a load-bearing safety
    // net: it converts a silent slice-empty regression into a panic that
    // surfaces at the first builtin call instead of a confusing
    // `HARN-NAM-002` somewhere down the line.
    let len = std::hint::black_box(macros::ALL_BUILTIN_DEFS.len());
    assert!(
        len >= 1,
        "linkme distributed_slice ALL_BUILTIN_DEFS is empty — \
         the binary is missing `harn_vm::stdlib::force_link()` at startup, \
         or the linker stripped the harn-vm rlib statics (see linkme issue #36)"
    );
}

/// Driver-facing immutable manifest for parser, IR, policy, and docs.
pub fn all_builtin_manifest() -> &'static [&'static harn_builtin_registry::BuiltinManifestEntry] {
    use std::sync::OnceLock;
    static AGG: OnceLock<Vec<&'static harn_builtin_registry::BuiltinManifestEntry>> =
        OnceLock::new();
    AGG.get_or_init(|| {
        let mut out = Vec::new();
        let mut capability_methods = std::collections::BTreeSet::new();
        for def in all_builtin_defs() {
            if def.runtime_only {
                continue;
            }
            out.push(
                Box::leak(Box::new(harn_builtin_registry::BuiltinManifestEntry {
                    name: def.sig.name,
                    canonical_name: def.sig.name,
                    signature: &def.sig,
                    contract: def.contract,
                })) as &'static harn_builtin_registry::BuiltinManifestEntry,
            );
            if let harn_builtin_meta::BuiltinExposure::HarnessMethod { capability, method } =
                def.contract.exposure
            {
                capability_methods.insert((capability, method));
            }
            for alias in def.aliases {
                let signature = Box::leak(Box::new(harn_builtin_meta::BuiltinSignature {
                    name: alias,
                    ..def.sig
                }));
                out.push(
                    Box::leak(Box::new(harn_builtin_registry::BuiltinManifestEntry {
                        name: alias,
                        canonical_name: def.sig.name,
                        signature,
                        contract: def.contract,
                    })) as &'static harn_builtin_registry::BuiltinManifestEntry,
                );
            }
        }
        for entry in harn_capability_contracts::manifest() {
            let harn_builtin_meta::BuiltinExposure::HarnessMethod { capability, method } =
                entry.contract.exposure
            else {
                unreachable!("leaf capability manifest contains a non-method contract")
            };
            if !capability_methods.insert((capability, method)) {
                let runtime_entry = out
                    .iter()
                    .find(|candidate| candidate.contract.exposure == entry.contract.exposure)
                    .expect("duplicate capability key must have a runtime manifest entry");
                assert_eq!(
                    runtime_entry.contract,
                    entry.contract,
                    "runtime effect contract drift for harness.{}.{}",
                    capability.field_name(),
                    method
                );
                assert_eq!(
                    runtime_entry.signature,
                    entry.signature,
                    "runtime signature drift for harness.{}.{}",
                    capability.field_name(),
                    method
                );
                continue;
            }
            out.push(*entry);
        }
        for group in harn_builtin_meta::host_capabilities::all_host_capability_groups() {
            for method in group.methods {
                if !capability_methods.insert((group.capability, *method)) {
                    continue;
                }
                let internal_name: &'static str = Box::leak(
                    format!("__cap_{}_{}", group.capability.field_name(), method).into_boxed_str(),
                );
                let params: &'static [harn_builtin_meta::Param] =
                    Box::leak(Box::new([harn_builtin_meta::Param::new(
                        "request",
                        harn_builtin_meta::Ty::Named("dict"),
                    )]));
                let signature = Box::leak(Box::new(harn_builtin_meta::BuiltinSignature::simple(
                    internal_name,
                    params,
                    harn_builtin_meta::Ty::Named("dict"),
                )));
                out.push(Box::leak(Box::new(
                    harn_builtin_registry::BuiltinManifestEntry {
                        name: internal_name,
                        canonical_name: internal_name,
                        signature,
                        contract: harn_builtin_meta::BuiltinContract::harness(
                            group.capability,
                            method,
                            group.effects,
                        ),
                    },
                )));
            }
        }
        out
    })
    .as_slice()
}

/// Indexed view of the authoritative builtin manifest.
///
/// Runtime dispatch reaches this boundary for every typed Harness call. Keep
/// lookup policy here with the manifest owner instead of making each consumer
/// linearly rescan the full registry. The nested capability map also accepts a
/// borrowed `&str`, so a method call does not allocate an owned lookup key.
struct BuiltinManifestIndex {
    by_name: std::collections::HashMap<
        &'static str,
        &'static harn_builtin_registry::BuiltinManifestEntry,
    >,
    by_capability: std::collections::HashMap<
        harn_builtin_meta::CapabilityId,
        std::collections::HashMap<
            &'static str,
            &'static harn_builtin_registry::BuiltinManifestEntry,
        >,
    >,
    recorded_effects_by_name: std::collections::HashMap<
        &'static str,
        &'static harn_builtin_registry::BuiltinManifestEntry,
    >,
}

fn builtin_manifest_index() -> &'static BuiltinManifestIndex {
    use harn_builtin_meta::BuiltinExposure;
    use std::sync::OnceLock;

    static INDEX: OnceLock<BuiltinManifestIndex> = OnceLock::new();
    INDEX.get_or_init(|| {
        let mut by_name = std::collections::HashMap::new();
        let mut by_capability: std::collections::HashMap<
            harn_builtin_meta::CapabilityId,
            std::collections::HashMap<
                &'static str,
                &'static harn_builtin_registry::BuiltinManifestEntry,
            >,
        > = std::collections::HashMap::new();
        let mut recorded_effects_by_name = std::collections::HashMap::new();
        for entry in all_builtin_manifest() {
            assert!(
                by_name.insert(entry.name, *entry).is_none(),
                "duplicate builtin manifest name `{}`",
                entry.name
            );
            // An alias repeats its primary's contract under a second name, so
            // only the canonical entry may claim the capability method.
            if let BuiltinExposure::HarnessMethod { capability, method } = entry.contract.exposure {
                if entry.is_canonical() {
                    assert!(
                        by_capability
                            .entry(capability)
                            .or_default()
                            .insert(method, *entry)
                            .is_none(),
                        "duplicate Harness method manifest entry `harness.{}.{method}`",
                        capability.field_name()
                    );
                }
            }
            if matches!(
                entry.contract.exposure,
                BuiltinExposure::CapabilityFunction { .. }
                    | BuiltinExposure::HarnessMethod { .. }
                    | BuiltinExposure::PrivilegedWire
            ) && !entry.contract.effects.is_empty()
            {
                recorded_effects_by_name.insert(entry.name, *entry);
            }
        }
        BuiltinManifestIndex {
            by_name,
            by_capability,
            recorded_effects_by_name,
        }
    })
}

/// Resolve the registry name behind a public `harness.<capability>.<method>`
/// path.
///
/// Observation, diagnostics, and tooling classify a capability call by the
/// same registry entry the removed ambient global resolved to, so a call
/// through the typed handle profiles and audits identically.
pub fn builtin_for_harness_path(path: &str) -> Option<&'static str> {
    let (field, method) = path.strip_prefix("harness.")?.split_once('.')?;
    let capability = harn_builtin_meta::CapabilityId::from_field_name(field)?;
    capability_method_manifest_entry(capability, method).map(|entry| entry.name)
}

/// Resolve the typed Harness method that replaced an ambient builtin name.
pub fn harness_method_for_builtin(
    name: &str,
) -> Option<(harn_builtin_meta::CapabilityId, &'static str)> {
    match builtin_manifest_entry(name)?.contract.exposure {
        harn_builtin_meta::BuiltinExposure::HarnessMethod { capability, method } => {
            Some((capability, method))
        }
        _ => None,
    }
}

/// Resolve a source/runtime builtin contract without rescanning the manifest.
pub fn builtin_manifest_entry(
    name: &str,
) -> Option<&'static harn_builtin_registry::BuiltinManifestEntry> {
    builtin_manifest_index().by_name.get(name).copied()
}

/// Resolve the contract for one typed Harness method without allocation.
pub fn capability_method_manifest_entry(
    capability: harn_builtin_meta::CapabilityId,
    method: &str,
) -> Option<&'static harn_builtin_registry::BuiltinManifestEntry> {
    builtin_manifest_index()
        .by_capability
        .get(&capability)
        .and_then(|methods| methods.get(method))
        .copied()
}

/// Resolve only builtin contracts that can emit runtime effect receipts.
///
/// Pure builtins dominate VM call volume. Keeping them out of this projection
/// makes their receipt check one negative hash lookup instead of a full
/// manifest lookup plus exposure classification on every call.
pub fn recorded_effect_builtin_manifest_entry(
    name: &str,
) -> Option<&'static harn_builtin_registry::BuiltinManifestEntry> {
    builtin_manifest_index()
        .recorded_effects_by_name
        .get(name)
        .copied()
}

mod harness_migration;

pub use harness_migration::{
    harness_migration_for_builtin, HarnessBuiltinArgumentMigration, HarnessBuiltinMigration,
};

/// Register every `#[harn_builtin]`-emitted def on the given VM. Drivers
/// that build the full stdlib via `register_vm_stdlib` get this for free —
/// each module's `register_*_builtins` walks its `MODULE_BUILTINS` slice.
/// This helper is exposed for embedders / tests that want a one-call entry.
pub fn register_all_macro_builtins(vm: &mut Vm) {
    for def in all_builtin_defs() {
        vm.register_builtin_def(def);
    }
}

/// Return the canonical list of all stdlib builtin names. Used by
/// harn-lint and harn-lsp to avoid hardcoded duplicate lists.
pub fn stdlib_builtin_names() -> Vec<String> {
    let vm = stdlib_probe_vm();
    let mut names = vm.builtin_names();
    // Special opcodes/keywords, not registered builtins, but linter
    // should recognize them as valid function calls.
    for extra in harn_parser::builtin_signatures::LANGUAGE_INTRINSICS {
        names.push(extra.to_string());
    }
    names
}

/// Return discoverable metadata for registered stdlib builtins.
pub fn stdlib_builtin_metadata() -> Vec<crate::vm::VmBuiltinMetadata> {
    stdlib_probe_vm().builtin_metadata()
}

/// Declared exposure of a registered builtin, or `None` when the VM registers
/// no builtin by that name.
///
/// `harn_builtin_meta` calls itself "the semantic owner for which script
/// surface may reach a builtin", and its vocabulary is explicit:
/// `PrivilegedWire` is documented as "User modules cannot name or re-export
/// it" and `RuntimeInternal` as "never source-visible". The typechecker
/// enforces that. Nothing else read it — [`stdlib_builtin_names`] answers the
/// different question of what the VM has *registered*, so a consumer that
/// treats that set as the callable surface goes quiet on exactly the calls the
/// typechecker will reject. `host_call` is the case that surfaced it: declared
/// `privileged_wire`, rejected by `harn check`, and silent under `harn lint`
/// across 114 call sites in one downstream repo (harn#6126).
pub fn builtin_exposure(name: &str) -> Option<harn_builtin_meta::BuiltinExposure> {
    use std::sync::OnceLock;
    static BY_NAME: OnceLock<
        std::collections::HashMap<String, harn_builtin_meta::BuiltinExposure>,
    > = OnceLock::new();
    BY_NAME
        .get_or_init(|| {
            stdlib_probe_vm()
                .builtin_metadata()
                .into_iter()
                .map(|entry| (entry.name().to_string(), entry.contract().exposure))
                .collect()
        })
        .get(name)
        .copied()
}

/// The declared harness method that owns a host-wire operation name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HarnessMethodTarget {
    pub capability: harn_builtin_meta::CapabilityId,
    pub method: &'static str,
}

impl HarnessMethodTarget {
    /// Source spelling of the call target, without the harness root.
    pub fn path(&self) -> String {
        format!("{}.{}", self.capability.field_name(), self.method)
    }
}

/// Resolve a host-wire operation name such as `"prmonitor.run_commands"` to
/// the `harness.<capability>.<method>` that declares it, when one exists.
///
/// Host wires carry their destination as a *string*, so a call to one is
/// opaque to every name-keyed check in the toolchain: the operation name is
/// data, not a symbol. Reading it back through the declared contract is what
/// turns "you cannot call this" into "call this instead", which is the whole
/// difference for a downstream repo holding hundreds of such call sites
/// (harn#6126).
///
/// Resolution composes two owners rather than adding a third table.
/// `capability_binding_for_schema` goes first because it owns the deliberate
/// remappings — a `"session.open"` wire is `harness.agent.session_open`, not
/// the `harness.session` handle a namespace match alone would reach. Its
/// `HOST_CAPABILITY_GROUPS` domain is only part of the declared surface
/// (5 of 86 real targets), so the manifest answers the rest.
///
/// Returns `None` when the namespace names no capability, when the capability
/// declares no such method, or when a normalized match would be ambiguous. A
/// wrong destination is worse than none: it would aim a migration at the wrong
/// method.
pub fn harness_method_for_host_operation(operation: &str) -> Option<HarnessMethodTarget> {
    use harn_builtin_meta::{wire_identifier_key, BuiltinExposure, CapabilityId};
    use std::collections::HashMap;
    use std::sync::OnceLock;

    let (namespace, operation_method) = operation.split_once('.')?;
    if let Some((capability, method)) =
        harn_builtin_meta::host_capabilities::capability_binding_for_schema(
            namespace,
            operation_method,
        )
    {
        return Some(HarnessMethodTarget { capability, method });
    }

    static BY_CAPABILITY: OnceLock<HashMap<CapabilityId, Vec<&'static str>>> = OnceLock::new();
    let declared = BY_CAPABILITY.get_or_init(|| {
        let mut index: HashMap<CapabilityId, Vec<&'static str>> = HashMap::new();
        // The manifest, not a probe VM: `stdlib_probe_vm().builtin_metadata()`
        // sees only what that VM registers — 334 harness methods against the
        // manifest's 978 — and the ones it misses are the host-implemented
        // capabilities a wire actually targets. Alias entries repeat a
        // primary's contract under a second name, so a capability-method
        // projection must take only canonical entries.
        for entry in all_builtin_manifest() {
            if !entry.is_canonical() {
                continue;
            }
            if let BuiltinExposure::HarnessMethod { capability, method } = entry.contract.exposure {
                index.entry(capability).or_default().push(method);
            }
        }
        index
    });

    // Wires predate the typed vocabulary and spell the namespace without a
    // separator, so `prmonitor` names the `pr_monitor` capability.
    let capability = CapabilityId::from_host_namespace(namespace)?;
    let methods = declared.get(&capability)?;
    if let Some(exact) = methods.iter().find(|method| **method == operation_method) {
        return Some(HarnessMethodTarget {
            capability,
            method: exact,
        });
    }
    let wanted = wire_identifier_key(operation_method);
    let mut lenient = methods
        .iter()
        .filter(|method| wire_identifier_key(method) == wanted);
    let only = lenient.next()?;
    lenient.next().is_none().then_some(HarnessMethodTarget {
        capability,
        method: only,
    })
}

/// Whether Harn source may write this builtin's bare name in a call.
///
/// A harness method is reached as `harness.<capability>.<method>` rather than
/// as a global, so it is not bare-nameable either. `Undeclared` is a migration
/// state rather than a promise, and answering `true` there keeps this
/// predicate from inventing a restriction the contract has not made yet.
pub fn exposure_is_source_nameable(exposure: harn_builtin_meta::BuiltinExposure) -> bool {
    use harn_builtin_meta::BuiltinExposure;
    match exposure {
        BuiltinExposure::PureGlobal
        | BuiltinExposure::CapabilityFunction { .. }
        | BuiltinExposure::Undeclared => true,
        BuiltinExposure::HarnessMethod { .. }
        | BuiltinExposure::PrivilegedWire
        | BuiltinExposure::RuntimeInternal => false,
    }
}

/// Reset thread-local stdlib state. Call between test runs.
///
/// Note: `long_running::reset_state()` is intentionally NOT called here
/// because that store is process-global, not thread-local. Wiping it
/// from a per-test reset hook lets one test cancel another test's
/// in-flight worker thread (and lose its `agent_inbox::push`
/// notification), which surfaces as `walk_dir_long_running` /
/// `glob_long_running` timing out under parallel test load. The two
/// call sites that genuinely need a clean handle store —
/// `stdlib::fs::tests::{walk_dir_long_running,glob_long_running}` — call
/// `long_running::reset_state()` explicitly while holding
/// `LONG_RUNNING_TEST_LOCK`.
pub fn reset_stdlib_state() {
    logging::reset_logging_state();
    process::reset_process_state();
    clock::reset_clock_state();
    io::reset_io_state();
    sandbox::reset_sandbox_state();
    git::reset_git_state();
    fs::reset_fs_state();
    json::reset_json_state();
    json_stream::reset_json_stream_state();
    host::reset_host_state();
    host::reset_scoped_host_state();
    observability::reset_observability_state();
    timing::reset_timing_state();
    durable_step::reset_durable_step_state();
    crate::egress::reset_egress_policy_for_host();
    hitl::reset_hitl_state();
    crate::http::reset_http_state();
    crate::external_agent::reset_external_agent_state();
    monitors::reset_monitor_state();
    waitpoint::reset_waitpoint_state();
    triggers_stdlib::reset_auto_resume_timeouts();
    compaction::reset_compaction_state();
    agents::reset_agent_worker_state();
    agents::workflow::reset_workflow_run_states();
    pool::reset_pool_state();
    #[cfg(feature = "postgres")]
    postgres::reset_postgres_state();
    #[cfg(feature = "sqlite")]
    sqlite::reset_sqlite_state();
    supervisor::reset_supervisor_state();
    agents::records::reset_eval_metrics();
    agents::records::reset_friction_events();
    tools::clear_current_tool_registry();
    tools::clear_tool_synthesis_cache();
    vision::reset_vision_state();
    crate::skills::clear_current_skill_registry();
    template::reset_prompt_registry();
    crate::triggers::clear_webhook_intake_state();
    crate::llm::cache::reset_in_process_cache_state();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_operation_resolves_to_its_declared_harness_method() {
        let target = harness_method_for_host_operation("ast.outline")
            .expect("`ast.outline` is declared by harn-hostlib");
        assert_eq!(target.path(), "ast.outline");
    }

    #[test]
    fn host_operation_namespace_matching_ignores_underscores() {
        // Host wires spell the namespace without a separator. The capability
        // field name has one, and both must reach the same method.
        let squashed = harness_method_for_host_operation("prmonitor.run_commands")
            .expect("`prmonitor` names the `pr_monitor` capability");
        let spelled = harness_method_for_host_operation("pr_monitor.run_commands")
            .expect("the declared spelling resolves too");
        assert_eq!(squashed, spelled);
        assert_eq!(squashed.path(), "pr_monitor.run_commands");
    }

    #[test]
    fn host_operation_without_a_declared_owner_resolves_to_nothing() {
        // A wildcard, an unknown capability, and a real capability with no
        // such method. Each must decline rather than guess: naming the wrong
        // destination would point a migration at the wrong method.
        for operation in [
            "ast.*",
            "capability.operation",
            "runtime.set_result",
            "ast",
            "",
        ] {
            assert_eq!(
                harness_method_for_host_operation(operation),
                None,
                "`{operation}` must not resolve"
            );
        }
    }

    #[test]
    fn host_operation_honors_the_schema_tables_deliberate_remapping() {
        // Session persistence is owned by `HarnessAgent`, so the `session.*`
        // hostlib schema is exposed as `harness.agent.session_*`. A resolver
        // that matched the namespace against the capability vocabulary would
        // answer `harness.session.open` — a real handle, and the wrong one.
        // This is why the resolver defers to `capability_binding_for_schema`
        // rather than keeping a second table.
        let target = harness_method_for_host_operation("session.open")
            .expect("`session.open` is a declared hostlib schema operation");
        assert_eq!(target.path(), "agent.session_open");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn register_vm_stdlib_passes_default_harness_only_to_main() {
        let chunk = crate::compile_source(
            r"
fn __probe_harness_clock(clock: HarnessClock) {
  const now = clock.now_ms()
  return now >= 0
}

fn main(harness: Harness) {
  return __probe_harness_clock(harness.clock)
}
",
        )
        .expect("compile harness clock probe");
        let mut vm = Vm::new();
        register_vm_stdlib(&mut vm);

        assert!(vm.root_harness_value().is_some());
        assert!(vm.global("harness").is_none());
        let result = vm
            .execute(&chunk)
            .await
            .expect("execute harness clock probe");
        assert!(matches!(result, crate::value::VmValue::Bool(true)));
    }

    /// `harn_stdlib::builtin_reexports` names builtins from a crate that
    /// cannot see the builtin registry, so nothing there can catch a typo or a
    /// rename. This is that check: every re-exported name must resolve to a
    /// real builtin, or `import { … } from "std/…"` binds a reference to
    /// nothing and fails at the call site instead of the import.
    #[test]
    fn every_stdlib_builtin_reexport_names_a_registered_builtin() {
        let registered: std::collections::HashSet<&str> = all_builtin_defs()
            .iter()
            .flat_map(|def| std::iter::once(def.sig.name).chain(def.aliases.iter().copied()))
            .collect();

        let mut checked = 0;
        for entry in harn_stdlib::STDLIB_SOURCES {
            for name in harn_stdlib::builtin_reexports(entry.module) {
                assert!(
                    registered.contains(name),
                    "std/{} re-exports '{name}', which is not a registered builtin",
                    entry.module
                );
                checked += 1;
            }
        }
        assert!(
            checked > 0,
            "no re-exports were checked — the table or the module list is not being read"
        );
    }

    #[test]
    fn ambient_effect_builtin_allowlist_is_empty() {
        let offenders = all_builtin_defs()
            .iter()
            .filter(|def| {
                matches!(
                    def.contract.exposure,
                    harn_builtin_meta::BuiltinExposure::PureGlobal
                ) && !def.contract.effects.is_empty()
            })
            .map(|def| def.sig.name)
            .collect::<Vec<_>>();

        assert!(
            offenders.is_empty(),
            "ambient effect builtins are forbidden; route these through Harness: {offenders:?}"
        );
    }

    #[test]
    fn harness_builtin_migrations_preserve_canonical_call_shapes() {
        use harn_builtin_meta::CapabilityId;

        assert_eq!(
            harness_migration_for_builtin("provider_capabilities"),
            Some(HarnessBuiltinMigration {
                capability: CapabilityId::Llm,
                method: "provider_capabilities",
                arguments: HarnessBuiltinArgumentMigration::Forward,
            })
        );
        assert_eq!(
            harness_migration_for_builtin("log_info"),
            Some(HarnessBuiltinMigration {
                capability: CapabilityId::Observability,
                method: "log_info",
                arguments: HarnessBuiltinArgumentMigration::Forward,
            })
        );
        assert_eq!(
            harness_migration_for_builtin("runtime_introspection"),
            Some(HarnessBuiltinMigration {
                capability: CapabilityId::Runtime,
                method: "introspection",
                arguments: HarnessBuiltinArgumentMigration::Forward,
            })
        );
        assert_eq!(
            harness_migration_for_builtin("project_fingerprint"),
            Some(HarnessBuiltinMigration {
                capability: CapabilityId::Project,
                method: "fingerprint",
                arguments: HarnessBuiltinArgumentMigration::Forward,
            })
        );
        assert_eq!(
            harness_migration_for_builtin("project_scan_tree_native"),
            Some(HarnessBuiltinMigration {
                capability: CapabilityId::Project,
                method: "scan_tree",
                arguments: HarnessBuiltinArgumentMigration::Forward,
            })
        );
        assert_eq!(
            harness_migration_for_builtin("llm_call"),
            Some(HarnessBuiltinMigration {
                capability: CapabilityId::Llm,
                method: "call",
                arguments: HarnessBuiltinArgumentMigration::Forward,
            })
        );
        assert_eq!(
            harness_migration_for_builtin("llm_call_structured"),
            Some(HarnessBuiltinMigration {
                capability: CapabilityId::Llm,
                method: "call_structured",
                arguments: HarnessBuiltinArgumentMigration::Forward,
            })
        );
        assert_eq!(
            harness_migration_for_builtin("security_policy"),
            Some(HarnessBuiltinMigration {
                capability: CapabilityId::System,
                method: "security_policy",
                arguments: HarnessBuiltinArgumentMigration::Forward,
            })
        );
        assert_eq!(
            harness_migration_for_builtin("llm_provider_status"),
            Some(HarnessBuiltinMigration {
                capability: CapabilityId::Llm,
                method: "providers",
                arguments: HarnessBuiltinArgumentMigration::Forward,
            })
        );
        assert_eq!(
            harness_migration_for_builtin("agent_session_current_id"),
            Some(HarnessBuiltinMigration {
                capability: CapabilityId::Agent,
                method: "current_id",
                arguments: HarnessBuiltinArgumentMigration::Forward,
            })
        );
        assert_eq!(
            harness_migration_for_builtin("metadata_set"),
            Some(HarnessBuiltinMigration {
                capability: CapabilityId::Project,
                method: "metadata_set",
                arguments: HarnessBuiltinArgumentMigration::RequestRecord(&[
                    "dir",
                    "namespace",
                    "data",
                ]),
            })
        );
        assert_eq!(
            harness_migration_for_builtin("metadata_save"),
            Some(HarnessBuiltinMigration {
                capability: CapabilityId::Project,
                method: "metadata_save",
                arguments: HarnessBuiltinArgumentMigration::RequestRecord(&[]),
            })
        );
        assert_eq!(
            harness_migration_for_builtin("platform"),
            Some(HarnessBuiltinMigration {
                capability: CapabilityId::System,
                method: "platform",
                arguments: HarnessBuiltinArgumentMigration::CallThenProperty("os"),
            })
        );
        assert_eq!(
            harness_migration_for_builtin("arch"),
            Some(HarnessBuiltinMigration {
                capability: CapabilityId::System,
                method: "platform",
                arguments: HarnessBuiltinArgumentMigration::CallThenProperty("arch"),
            })
        );
        assert_eq!(
            harness_migration_for_builtin("home_dir"),
            Some(HarnessBuiltinMigration {
                capability: CapabilityId::Fs,
                method: "home_dir",
                arguments: HarnessBuiltinArgumentMigration::Forward,
            })
        );
        assert_eq!(harness_migration_for_builtin("json_parse"), None);
    }

    #[test]
    fn every_source_named_runtime_callable_has_a_typed_contract() {
        let manifest_names = all_builtin_manifest()
            .iter()
            .map(|entry| entry.name)
            .collect::<std::collections::HashSet<_>>();
        let offenders = stdlib_probe_vm()
            .builtin_names()
            .into_iter()
            .filter(|name| {
                !name.starts_with("__")
                    && !manifest_names.contains(name.as_str())
                    && !harn_parser::builtin_signatures::is_language_intrinsic(name)
            })
            .collect::<Vec<_>>();

        assert!(
            offenders.is_empty(),
            "runtime callables without typed source contracts bypass the Harness gate: {offenders:?}"
        );
    }

    #[test]
    fn builtin_manifest_indexes_preserve_the_authoritative_contracts() {
        use harn_builtin_meta::BuiltinExposure;

        let mut capability_entries = 0;
        for entry in all_builtin_manifest() {
            assert!(
                std::ptr::eq(
                    builtin_manifest_entry(entry.name).expect("indexed builtin entry"),
                    *entry,
                ),
                "name index projected a different contract for `{}`",
                entry.name
            );
            let should_record = matches!(
                entry.contract.exposure,
                BuiltinExposure::CapabilityFunction { .. }
                    | BuiltinExposure::HarnessMethod { .. }
                    | BuiltinExposure::PrivilegedWire
            ) && !entry.contract.effects.is_empty();
            assert_eq!(
                recorded_effect_builtin_manifest_entry(entry.name).is_some(),
                should_record,
                "recorded-effect index drifted for `{}`",
                entry.name
            );
            if let BuiltinExposure::HarnessMethod { capability, method } = entry.contract.exposure {
                capability_entries += 1;
                let indexed = capability_method_manifest_entry(capability, method)
                    .expect("indexed Harness method entry");
                // An alias shares its primary's contract under a second name.
                // The capability index answers with the primary either way.
                assert_eq!(
                    indexed.name,
                    entry.canonical_name,
                    "capability index projected a different builtin for `harness.{}.{method}`",
                    capability.field_name()
                );
                assert_eq!(
                    indexed.contract,
                    entry.contract,
                    "capability index projected a different contract for `harness.{}.{method}`",
                    capability.field_name()
                );
            }
        }
        assert!(capability_entries > 0, "no Harness contracts were indexed");
        assert!(builtin_manifest_entry("__definitely_missing_builtin").is_none());
        assert!(capability_method_manifest_entry(
            harn_builtin_meta::CapabilityId::Fs,
            "__definitely_missing_method",
        )
        .is_none());
    }
}

#[cfg(test)]
mod ambient_host_internal_projection_tests {
    use super::*;

    #[test]
    fn ambient_bridge_projects_host_internal_emit_event_alias() {
        let previous = std::env::var_os(harn_parser::HARN_LEGACY_AMBIENT_CAPABILITIES_ENV);
        unsafe {
            std::env::set_var(harn_parser::HARN_LEGACY_AMBIENT_CAPABILITIES_ENV, "1");
        }
        harn_parser::refresh_legacy_ambient_capabilities();
        let mut vm = Vm::new();
        register_vm_stdlib(&mut vm);
        assert!(
            vm.builtin_metadata_for("agent_emit_event").is_some(),
            "ambient bridge must project __host_agent_emit_event as agent_emit_event"
        );
        assert!(
            vm.builtin_metadata_for("__host_agent_emit_event").is_some(),
            "canonical host internal must remain registered"
        );
        unsafe {
            match previous {
                Some(value) => {
                    std::env::set_var(harn_parser::HARN_LEGACY_AMBIENT_CAPABILITIES_ENV, value);
                }
                None => std::env::remove_var(harn_parser::HARN_LEGACY_AMBIENT_CAPABILITIES_ENV),
            }
        }
        harn_parser::refresh_legacy_ambient_capabilities();
    }
}
