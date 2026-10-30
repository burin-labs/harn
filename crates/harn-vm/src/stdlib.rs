//! Standard library builtins for the Harn VM.
//!
//! Every builtin is declared with the `#[harn_builtin]` proc-macro
//! (`crate::stdlib::macros::harn_builtin`). Each annotation emits a sibling
//! `static <FN>_DEF: VmBuiltinDef` carrying the signature, aliases, handler,
//! and metadata, and registers it into the workspace-global
//! [`macros::ALL_BUILTIN_DEFS`] distributed slice at link time. The CLI / LSP /
//! lint / serve / dap binaries call [`force_link`] to defeat rlib dead-code
//! stripping (linkme issue #36) so every static lands in the slice. Each
//! family declares its installer beside its `register_*_builtins` function
//! with `harn_vm_core::stdlib_installer!`, and `register_vm_stdlib` runs every
//! linked installer, so no central list names the families and a family's
//! owning crate can sit above this one. Membership of each installer comes from
//! the macro-captured module owner, not a second list of functions.
//! `register_vm_stdlib` then installs the signatures into the parser registry.
//!
//! See `CONTRIBUTING.md` ("Adding a stdlib builtin") for the full template.

pub mod junit;
pub mod macros;

pub(crate) use crate::value::args;

#[path = "stdlib/agent_sessions/mount.rs"]
mod agent_session_mount;
mod agent_sessions;
pub use agent_session_mount::{add_workspace_root, add_workspace_root_persisted};
pub(crate) mod agents;
pub(crate) mod agents_daemon;
pub(crate) mod assemble;
pub mod asset_paths;
mod bytes;
pub mod clock;
pub mod collections;
mod concurrency;
mod connectors;
mod crypto;
pub mod datetime;
pub(crate) use datetime::date_dict_from_millis;
pub(crate) mod files;
pub mod fs;
pub mod git_topology;
pub(crate) mod harn_entry;
pub mod hitl;
pub mod host;
pub mod io;
mod iter;
pub mod json;
mod json_query;
pub(crate) mod json_repair;
pub(crate) mod json_stream;
mod logging;
pub mod long_running;
mod math;
pub(crate) mod memory;
mod monitors;
mod net_policy;
pub(crate) mod observability;
mod package_snapshot;
pub use package_snapshot::PackageSnapshotRegistry;
pub(crate) mod canonical_store;
pub(crate) mod pool;
pub mod process;
pub(crate) mod process_spawn;
mod regex;
mod runtime_scope;
pub mod sandbox;
pub(crate) mod session_change;
pub(crate) mod session_store;
pub(crate) mod session_wal_watch;
mod sets;
pub(crate) mod shapes;
pub mod signals;
mod skills;
pub(crate) mod strings;
pub(crate) mod supervisor;
pub mod template;
mod testing;
pub mod token_redaction;
mod tool_projection;
pub(crate) mod tools;
pub mod tracing;
pub(crate) mod transcript_project;
mod triggers_stdlib;
mod types;
mod vision;
pub(crate) mod waitpoint;
pub(crate) mod xml;

use crate::vm::Vm;
use harn_vm_core::stdlib_installers::{install_stdlib_tier, StdlibTier};

pub(crate) use crate::schema::{json_to_vm_value, schema_result_value};
pub(crate) fn set_thread_source_dir(dir: &std::path::Path) {
    process::set_thread_source_dir(dir);
}

/// Register core builtins: pure/deterministic, no I/O.
pub fn register_core_stdlib(vm: &mut Vm) {
    install_stdlib_tier(vm, StdlibTier::Pure);
}

/// Register all standard builtins on a VM (core + io + agent). Also
/// installs the macro-emitted signature slice into the parser registry
/// (idempotent under repeat calls with the same slice pointer).
pub fn register_vm_stdlib(vm: &mut Vm) {
    sandbox::child_write::install_child_write_probe();
    if !crate::native_vm::install_shared_stdlib_registration(vm) {
        register_stdlib_bindings(vm);
    }
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

pub(crate) fn register_stdlib_bindings(vm: &mut Vm) {
    install_stdlib_tier(vm, StdlibTier::Pure);
    install_stdlib_tier(vm, StdlibTier::Host);
    vm.project_declared_capability_methods();
}

pub(crate) fn rebind_execution_state_builtins(vm: &mut Vm) {
    harn_vm_core::stdlib_installers::rebind_execution_state(vm);
}

fn stdlib_probe_vm() -> Vm {
    let mut vm = crate::new_vm();
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

pub(crate) use crate::vm::builtin_policy_metadata;
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
pub use crate::vm::{
    all_builtin_defs, all_builtin_manifest, builtin_for_harness_path, builtin_manifest_entry,
    capability_method_manifest_entry, harness_method_for_builtin,
    recorded_effect_builtin_manifest_entry,
};

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
    let len = std::hint::black_box(macros::ALL_BUILTIN_DEFS.len()).min(std::hint::black_box(
        harn_vm_core::stdlib_installers::STDLIB_INSTALLERS.len(),
    ));
    assert!(
        len >= 1,
        "linkme distributed_slice ALL_BUILTIN_DEFS is empty — \
         the binary is missing `harn_vm::stdlib::force_link()` at startup, \
         or the linker stripped the harn-vm rlib statics (see linkme issue #36)"
    );
}

mod harness_migration;

pub use harness_migration::{
    harness_migration_for_builtin, HarnessBuiltinArgumentMigration, HarnessBuiltinMigration,
};

/// Register every `#[harn_builtin]`-emitted def on the given VM. Drivers
/// that build the full stdlib via `register_vm_stdlib` get this for free —
/// module installers select definitions from this same registry.
/// This helper is exposed for embedders / tests that want a one-call entry.
pub fn register_all_macro_builtins(vm: &mut Vm) {
    for def in all_builtin_defs() {
        vm.register_builtin_def(def);
    }
}

/// Return the canonical list of all stdlib builtin names. Used by
/// harn-lint and harn-lsp to avoid hardcoded duplicate lists.
pub fn stdlib_builtin_names() -> Vec<String> {
    // Building the probe VM registers the whole stdlib, and the linter asks
    // for these names for every file it lints. Registration varies only with
    // the legacy ambient-capability bridge, so keep one answer per setting.
    static NAMES: [std::sync::OnceLock<Vec<String>>; 2] =
        [std::sync::OnceLock::new(), std::sync::OnceLock::new()];
    NAMES[usize::from(harn_parser::legacy_ambient_capabilities_enabled())]
        .get_or_init(|| {
            let vm = stdlib_probe_vm();
            let mut names = vm.builtin_names();
            // Special opcodes/keywords, not registered builtins, but linter
            // should recognize them as valid function calls.
            for extra in harn_parser::builtin_signatures::LANGUAGE_INTRINSICS {
                names.push(extra.to_string());
            }
            names
        })
        .clone()
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
        | BuiltinExposure::StdlibInternal
        | BuiltinExposure::RuntimeInternal => false,
    }
}

/// Reset thread-local stdlib state. Call between test runs. Each family
/// declares its reset with `harn_vm_core::stdlib_state_reset!`.
///
/// `long_running::reset_state()` is intentionally not declared
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
    harn_vm_core::stdlib_installers::reset_stdlib_state();
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
        let mut vm = crate::new_vm();
        register_vm_stdlib(&mut vm);

        assert!(vm.root_harness_value().is_some());
        assert!(vm.global("harness").is_none());
        let result = vm
            .execute(&chunk)
            .await
            .expect("execute harness clock probe");
        assert!(matches!(result, crate::value::VmValue::Bool(true)));
    }

    /// `harn_stdlib_catalog::builtin_reexports` names builtins from a crate that
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
        for entry in harn_modules::stdlib_catalog().sources {
            for name in harn_stdlib_catalog::builtin_reexports(entry.module) {
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
                    | BuiltinExposure::StdlibInternal
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
        let mut vm = crate::new_vm();
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

#[cfg(test)]
mod installer_tests {
    use harn_vm_core::stdlib_installers::{stdlib_installers, StdlibTier};
    use std::collections::{BTreeMap, BTreeSet};

    /// Builtin names and capability methods one installer adds to a fresh VM.
    fn installed_by(install: fn(&mut crate::vm::Vm)) -> BTreeSet<String> {
        let surface = |vm: &crate::vm::Vm| -> BTreeSet<String> {
            vm.builtin_names()
                .into_iter()
                .chain(
                    vm.capability_method_names()
                        .into_iter()
                        .map(|(capability, method)| format!("{capability:?}.{method}")),
                )
                .collect()
        };
        let mut vm = crate::new_vm();
        let before = surface(&vm);
        install(&mut vm);
        surface(&vm).difference(&before).cloned().collect()
    }

    #[test]
    fn installers_never_register_the_same_name() {
        let mut owners: BTreeMap<String, &str> = BTreeMap::new();
        let mut overlaps = Vec::new();
        for installer in stdlib_installers() {
            let names = installed_by(installer.install);
            assert!(
                !names.is_empty(),
                "installer `{}` registers nothing",
                installer.name
            );
            for name in names {
                if let Some(previous) = owners.insert(name.clone(), installer.name) {
                    overlaps.push(format!("{name}: {previous} and {}", installer.name));
                }
            }
        }
        assert!(
            overlaps.is_empty(),
            "two installers register the same name, so the result would depend on order:\n{}",
            overlaps.join("\n")
        );
    }

    #[test]
    fn both_tiers_have_linked_installers() {
        for tier in [StdlibTier::Pure, StdlibTier::Host] {
            assert!(
                stdlib_installers()
                    .iter()
                    .any(|installer| installer.tier == tier),
                "no linked installer for {tier:?}"
            );
        }
    }

    #[test]
    fn state_resets_clear_family_state() {
        let names: Vec<_> = harn_vm_core::stdlib_installers::STDLIB_STATE_RESETS
            .iter()
            .map(|reset| reset.name)
            .collect();
        assert!(
            names
                .iter()
                .any(|name| name.ends_with("::reset_clock_state")),
            "clock's state reset is not linked: {names:?}"
        );
        assert!(
            names.iter().all(|name| !name.contains("long_running")),
            "the process-global long-running store must not reset per test"
        );
    }
}
