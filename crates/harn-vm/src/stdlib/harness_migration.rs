//! Where a global that moved onto a `Harness` handle went.
//!
//! One question with one answer: given the name of a global that Harn source
//! can no longer call, which capability method replaced it and how do its
//! arguments map over? `harn lint` turns that into `HARN-LNT-071` and
//! `harn fix --apply --safety surface-changing` turns it into an edit, so
//! anything missing here shows up downstream as a bare "not defined" with no
//! way forward.

use std::collections::{BTreeMap, BTreeSet};

use harn_builtin_meta::CapabilityId;

use super::{
    all_builtin_manifest, builtin_manifest_entry, capability_method_manifest_entry,
    harness_method_for_builtin, stdlib_probe_vm,
};

/// How an ambient builtin's arguments map onto its typed Harness replacement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HarnessBuiltinArgumentMigration {
    /// Preserve the original positional argument list.
    Forward,
    /// Wrap positional arguments in a named request record.
    RequestRecord(&'static [&'static str]),
    /// Replace a zero-argument legacy projection with a property of a typed
    /// Harness snapshot, for example `platform()` with
    /// `harness.system.platform().os`.
    CallThenProperty(&'static str),
}

/// Complete migration recipe for a removed ambient builtin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HarnessBuiltinMigration {
    pub capability: harn_builtin_meta::CapabilityId,
    pub method: &'static str,
    pub arguments: HarnessBuiltinArgumentMigration,
}

/// Resolve an ambient builtin to its canonical typed Harness call shape.
///
/// Most recipes project mechanically from `HarnessMethod` exposure. Legacy
/// metadata globals and process/environment projections predate that manifest
/// contract, so their explicit recipes preserve named request records and the
/// structured System/Fs/Term snapshots instead of adding compatibility
/// overloads to the typed handles.
pub fn harness_migration_for_builtin(name: &str) -> Option<HarnessBuiltinMigration> {
    if let Some((capability, method)) = harness_method_for_builtin(name) {
        return Some(HarnessBuiltinMigration {
            capability,
            method,
            arguments: HarnessBuiltinArgumentMigration::Forward,
        });
    }
    use harn_builtin_meta::CapabilityId;
    use HarnessBuiltinArgumentMigration::{CallThenProperty, Forward, RequestRecord};
    let request_record = |method, fields| HarnessBuiltinMigration {
        capability: CapabilityId::Project,
        method,
        arguments: RequestRecord(fields),
    };
    let projection = |capability, method, property| HarnessBuiltinMigration {
        capability,
        method,
        arguments: CallThenProperty(property),
    };
    let forward = |capability, method| HarnessBuiltinMigration {
        capability,
        method,
        arguments: Forward,
    };
    let (method, fields): (&'static str, &'static [&'static str]) = match name {
        "metadata_get" | "metadata_resolve" => ("metadata_get", &["dir", "namespace"]),
        "metadata_set" => ("metadata_set", &["dir", "namespace", "data"]),
        "metadata_entries" => ("metadata_entries", &["namespace"]),
        "metadata_save" => ("metadata_save", &[]),
        "metadata_stale" => ("metadata_stale", &["dir"]),
        "metadata_refresh_hashes" => ("metadata_refresh_hashes", &[]),
        "metadata_status" => ("metadata_status", &["namespace"]),
        "path_metadata_get" => ("path_metadata_get", &["path", "namespace", "options"]),
        "path_metadata_set" => (
            "path_metadata_set",
            &["path", "namespace", "data", "options"],
        ),
        "path_metadata_entries" => ("path_metadata_entries", &["namespace", "options"]),
        "platform" => return Some(projection(CapabilityId::System, "platform", "os")),
        "arch" => return Some(projection(CapabilityId::System, "platform", "arch")),
        "username" => return Some(projection(CapabilityId::System, "identity", "username")),
        "hostname" => return Some(projection(CapabilityId::System, "identity", "hostname")),
        "pid" => return Some(projection(CapabilityId::System, "identity", "pid")),
        "execution_root" => {
            return Some(projection(
                CapabilityId::Fs,
                "runtime_paths",
                "execution_root",
            ));
        }
        "asset_root" => {
            return Some(projection(CapabilityId::Fs, "runtime_paths", "asset_root"));
        }
        "home_dir" => return Some(forward(CapabilityId::Fs, "home_dir")),
        "runtime_paths" => return Some(forward(CapabilityId::Fs, "runtime_paths")),
        "source_dir" => return Some(forward(CapabilityId::Fs, "source_dir")),
        "project_root" => return Some(forward(CapabilityId::Fs, "project_root")),
        "date_iso" => return Some(forward(CapabilityId::Clock, "date_iso")),
        "term_width" => return Some(forward(CapabilityId::Term, "width")),
        "term_height" => return Some(forward(CapabilityId::Term, "height")),
        "security_policy" => {
            return Some(forward(CapabilityId::System, "security_policy"));
        }
        "security_stamp_directive" => {
            return Some(forward(CapabilityId::System, "security_stamp_directive"));
        }
        "security_verify_directive" => {
            return Some(forward(CapabilityId::System, "security_verify_directive"));
        }
        "llm_catalog" => return Some(forward(CapabilityId::Llm, "catalog")),
        "llm_catalog_refresh" => {
            return Some(forward(CapabilityId::Llm, "catalog_refresh"));
        }
        "llm_provider_status" => return Some(forward(CapabilityId::Llm, "providers")),
        "llm_session_cost" => return Some(forward(CapabilityId::Llm, "session_cost")),
        "llm_budget" => return Some(forward(CapabilityId::Llm, "budget")),
        "llm_budget_remaining" => {
            return Some(forward(CapabilityId::Llm, "budget_remaining"));
        }
        "transport_mock_clear" => {
            return Some(forward(CapabilityId::Testing, "transport_mock_clear"));
        }
        "transport_mock_calls" => {
            return Some(forward(CapabilityId::Testing, "transport_mock_calls"));
        }
        "sse_mock" => return Some(forward(CapabilityId::Testing, "sse_mock")),
        "sse_server_mock_receive" => {
            return Some(forward(CapabilityId::Testing, "sse_server_mock_receive"));
        }
        "sse_server_mock_disconnect" => {
            return Some(forward(CapabilityId::Testing, "sse_server_mock_disconnect"));
        }
        "websocket_mock" => return Some(forward(CapabilityId::Testing, "websocket_mock")),
        // The connector runtime used to inject `secret_get` for the duration
        // of an export call. Connector exports now receive a root Harness, so
        // the same read is a plain capability method.
        "secret_get" => return Some(forward(CapabilityId::Secrets, "read")),
        // These globals were renamed on the way onto their handle, so the
        // name match below has nothing to follow. Everything that kept its
        // name resolves without an entry here.
        "mock_time" => return Some(forward(CapabilityId::Testing, "clock_set")),
        "unmock_time" => return Some(forward(CapabilityId::Testing, "clock_reset")),
        "advance_time" => return Some(forward(CapabilityId::Testing, "clock_advance")),
        "mock_stdin" => return Some(forward(CapabilityId::Testing, "stdin_set")),
        "unmock_stdin" => return Some(forward(CapabilityId::Testing, "stdin_reset")),
        "mock_tty" => return Some(forward(CapabilityId::Testing, "tty_set")),
        "unmock_tty" => return Some(forward(CapabilityId::Testing, "tty_reset")),
        "host_mock_push_scope" => return Some(forward(CapabilityId::Testing, "push_scope")),
        "host_mock_pop_scope" => return Some(forward(CapabilityId::Testing, "pop_scope")),
        "host_mock_calls" => return Some(forward(CapabilityId::Testing, "calls")),
        "llm_mock" => return Some(forward(CapabilityId::Llm, "mock_enqueue")),
        "render_string" => return Some(forward(CapabilityId::Fs, "render_template")),
        "render_with_provenance" => {
            return Some(forward(CapabilityId::Fs, "render_prompt_with_provenance"));
        }
        "crypto_random_bytes" => return Some(forward(CapabilityId::Random, "bytes")),
        "emit_channel" => return Some(forward(CapabilityId::Channels, "append")),
        "flush_trigger_aggregations" => {
            return Some(forward(CapabilityId::Channels, "flush_aggregations"));
        }
        // The handle is `harness.channels`; the globals were singular.
        "channel_ack" => return Some(forward(CapabilityId::Channels, "ack")),
        "channel_events" => return Some(forward(CapabilityId::Channels, "events")),
        "channel_subscribe" => return Some(forward(CapabilityId::Channels, "subscribe")),
        "channel_consumer_cursor" => {
            return Some(forward(CapabilityId::Channels, "consumer_cursor"));
        }
        "pg_connect" => return Some(forward(CapabilityId::Postgres, "connect")),
        "pg_pool" => return Some(forward(CapabilityId::Postgres, "pool")),
        _ => {
            return derived_capability_owner(name)
                .map(|(capability, method)| forward(capability, method));
        }
    };
    Some(request_record(method, fields))
}

/// Every typed Harness method, indexed by method name and by owning
/// capability.
///
/// Two registration paths reach the same dispatch table. `#[harn_builtin]`
/// declares most methods through `HarnessMethod` exposure; store, checkpoint,
/// metadata, and host-injected methods are installed on the VM at startup and
/// never reach the builtin manifest at all. Projecting both keeps the
/// migration recipe — and with it the linter and `harn fix` — aware of the
/// whole surface instead of a hand-maintained list that drifts every time a
/// host adds a capability.
struct CapabilityMethodIndex {
    /// Owner of a method installed by `register_capability_method`, or `None`
    /// when several capabilities install it. This is the surface the legacy
    /// ambient bridge restores, so it decides what a bare global used to mean.
    bridged_owner: BTreeMap<&'static str, Option<CapabilityId>>,
    methods_by_capability: BTreeMap<CapabilityId, BTreeSet<&'static str>>,
}

fn capability_method_index() -> &'static CapabilityMethodIndex {
    static INDEX: std::sync::OnceLock<CapabilityMethodIndex> = std::sync::OnceLock::new();
    INDEX.get_or_init(|| {
        let mut index = CapabilityMethodIndex {
            bridged_owner: BTreeMap::new(),
            methods_by_capability: BTreeMap::new(),
        };
        for entry in all_builtin_manifest() {
            if let harn_builtin_meta::BuiltinExposure::HarnessMethod { capability, method } =
                entry.contract.exposure
            {
                index
                    .methods_by_capability
                    .entry(capability)
                    .or_default()
                    .insert(method);
            }
        }
        for (capability, method) in stdlib_probe_vm().capability_method_names() {
            let method: &'static str = Box::leak(method.into_boxed_str());
            index
                .bridged_owner
                .entry(method)
                .and_modify(|owner| {
                    if *owner != Some(capability) {
                        *owner = None;
                    }
                })
                .or_insert(Some(capability));
            index
                .methods_by_capability
                .entry(capability)
                .or_default()
                .insert(method);
        }
        index
    })
}

/// Where a pre-cutover global went, derived from the capability surface
/// itself rather than from a parallel list.
///
/// A global that moved onto a handle nearly always kept its own name, so the
/// method index doubles as the migration table. Three spellings show up:
///
///   * the name survived verbatim — `exit` became `harness.runtime.exit`;
///   * the name already carried its capability — `hostlib_code_index_rebuild`
///     became `harness.code_index.rebuild`;
///   * the name carried a family prefix the handle now supplies —
///     `agent_session_open` became `harness.agent.open`.
///
/// Two rules keep a guess from becoming a wrong rewrite. A name that is still
/// a callable global resolves to nothing, because that call works as written
/// and has not moved. A name several methods answer to is settled by parameter
/// list — `agent_session_open(id?, opts?)` matches `harness.agent.open` and not
/// `harness.agent.session_open` — and if that still leaves a tie, the name
/// resolves to nothing rather than sending a bare `read` to `harness.fs.read`
/// when the caller meant `harness.secrets.read`.
fn derived_capability_owner(name: &str) -> Option<(CapabilityId, &'static str)> {
    if is_source_visible_global(name) {
        return None;
    }
    let index = capability_method_index();
    if let Some((method, Some(owner))) = index.bridged_owner.get_key_value(name) {
        return Some((*owner, method));
    }

    let mut candidates: Vec<(CapabilityId, &'static str)> = Vec::new();
    let unprefixed = name.strip_prefix("hostlib_").unwrap_or(name);
    for (capability, methods) in &index.methods_by_capability {
        if let Some(method) = methods.get(name) {
            candidates.push((*capability, method));
        }
        for prefix in [
            capability.field_name().to_string(),
            snake_case(capability.variant_name()),
        ] {
            let Some(rest) = unprefixed
                .strip_prefix(&prefix)
                .and_then(|rest| rest.strip_prefix('_'))
            else {
                continue;
            };
            // `agent_session_open` and `agent_open` both mean an agent method:
            // the handle already says which session.
            for method in [Some(rest), rest.strip_prefix("session_")]
                .into_iter()
                .flatten()
                .filter_map(|method| methods.get(method))
            {
                candidates.push((*capability, method));
            }
        }
    }
    candidates.sort_unstable();
    candidates.dedup();
    if candidates.len() > 1 {
        candidates
            .retain(|(capability, method)| takes_the_same_parameters(name, *capability, method));
    }
    match candidates.as_slice() {
        [only] => Some(*only),
        _ => None,
    }
}

/// Whether a capability method accepts what the removed global accepted.
///
/// Capability methods repeat the global's parameter list verbatim, so this
/// separates a genuine successor from a same-named neighbour.
fn takes_the_same_parameters(removed_global: &str, capability: CapabilityId, method: &str) -> bool {
    let Some(before) = builtin_manifest_entry(removed_global) else {
        return false;
    };
    let Some(after) = capability_method_manifest_entry(capability, method) else {
        return false;
    };
    let names = |entry: &'static harn_builtin_registry::BuiltinManifestEntry| {
        entry
            .signature
            .params
            .iter()
            .map(|param| param.name)
            .collect::<Vec<_>>()
    };
    names(before) == names(after)
}

fn snake_case(camel: &str) -> String {
    let mut out = String::with_capacity(camel.len() + 2);
    for (index, ch) in camel.char_indices() {
        if ch.is_ascii_uppercase() && index > 0 {
            out.push('_');
        }
        out.push(ch.to_ascii_lowercase());
    }
    out
}

/// Whether Harn source can still call `name` as a plain global.
fn is_source_visible_global(name: &str) -> bool {
    builtin_manifest_entry(name).is_some_and(|entry| {
        matches!(
            entry.contract.exposure,
            harn_builtin_meta::BuiltinExposure::PureGlobal
                | harn_builtin_meta::BuiltinExposure::CapabilityFunction { .. }
        )
    })
}
#[cfg(test)]
mod registered_capability_migration_tests {
    use harn_builtin_meta::CapabilityId;

    use super::{
        all_builtin_manifest, harness_migration_for_builtin, HarnessBuiltinArgumentMigration,
        HarnessBuiltinMigration,
    };
    use crate::stdlib::stdlib_probe_vm;

    #[test]
    fn migration_recipes_follow_names_that_moved_onto_a_handle() {
        let forward = |capability, method| {
            Some(HarnessBuiltinMigration {
                capability,
                method,
                arguments: HarnessBuiltinArgumentMigration::Forward,
            })
        };
        // The name survived verbatim.
        assert_eq!(
            harness_migration_for_builtin("exit"),
            forward(CapabilityId::Runtime, "exit")
        );
        // The name already carried its capability.
        assert_eq!(
            harness_migration_for_builtin("hostlib_code_index_rebuild"),
            forward(CapabilityId::CodeIndex, "rebuild")
        );
        // The name carried a family prefix the handle now supplies.
        assert_eq!(
            harness_migration_for_builtin("agent_session_open"),
            forward(CapabilityId::Agent, "open")
        );
        // A global that still resolves has not moved anywhere.
        assert_eq!(harness_migration_for_builtin("len"), None);
    }

    /// Whether the linter can tell a caller where this global went.
    ///
    /// The clock, stdio, fs, env, random, and net families predate the recipe
    /// table and keep their own replacement maps in `harn-parser`, which the
    /// linter consults first.
    fn has_a_repair(name: &str) -> bool {
        use harn_parser::diagnostic::{
            harness_clock_replacement, harness_env_replacement, harness_fs_replacement,
            harness_net_replacement, harness_random_replacement, harness_stdio_replacement,
        };
        harness_migration_for_builtin(name).is_some()
            || harness_clock_replacement(name).is_some()
            || harness_stdio_replacement(name).is_some()
            || harness_fs_replacement(name).is_some()
            || harness_env_replacement(name).is_some()
            || harness_random_replacement(name).is_some()
            || harness_net_replacement(name).is_some()
    }

    /// Runtime plumbing that never had a script-facing name to migrate.
    ///
    /// `exec_opts` and `exec_at_opts` parse option records for the process
    /// builtins, `render` backs the template engine, `host_tool_*` is the
    /// host tool wire, and the rest are internals of the LLM mock and the
    /// fact cache. None of them is a global that moved onto a handle.
    const RUNTIME_PLUMBING: &[&str] = &[
        "exec_at_opts",
        "exec_opts",
        "host_tool_call",
        "host_tool_list",
        "invalidate_facts",
        "llm_mock_known_scopes",
        "llm_mock_load_jsonl",
        "llm_mock_receipts",
        "render",
    ];

    /// A builtin Harn source cannot name is either a global that moved onto a
    /// handle or runtime plumbing. The first kind needs a repair, or the
    /// cutover leaves callers with a bare "not defined" and no way forward;
    /// the second kind belongs on the list above, where a reviewer sees it.
    #[test]
    fn every_runtime_internal_builtin_is_migrated_or_named_as_plumbing() {
        use harn_builtin_meta::BuiltinExposure;

        let offenders = all_builtin_manifest()
            .iter()
            .filter(|entry| entry.is_canonical())
            .filter(|entry| matches!(entry.contract.exposure, BuiltinExposure::RuntimeInternal))
            .filter(|entry| !entry.name.starts_with("__"))
            .filter(|entry| !RUNTIME_PLUMBING.contains(&entry.name))
            .filter(|entry| !has_a_repair(entry.name))
            .map(|entry| entry.name)
            .collect::<Vec<_>>();

        assert!(
            offenders.is_empty(),
            "these globals moved onto a handle but report no repair: {offenders:?}"
        );
    }

    /// Every capability method the legacy ambient bridge can restore as a
    /// global needs a migration recipe, or a script running under the bridge
    /// gets an error with nowhere to go.
    #[test]
    fn every_uniquely_owned_capability_method_has_a_migration() {
        let vm = stdlib_probe_vm();
        let declared: std::collections::BTreeSet<String> = super::all_builtin_manifest()
            .iter()
            .map(|entry| entry.name.to_string())
            .collect();
        let mut owners: std::collections::BTreeMap<String, std::collections::BTreeSet<_>> =
            std::collections::BTreeMap::new();
        for (capability, method) in vm.capability_method_names() {
            owners.entry(method).or_default().insert(capability);
        }

        let missing: Vec<_> = owners
            .iter()
            .filter(|(method, capabilities)| {
                capabilities.len() == 1
                    && !declared.contains(*method)
                    && harness_migration_for_builtin(method).is_none()
            })
            .map(|(method, _)| method.clone())
            .collect();
        assert!(
            missing.is_empty(),
            "capability methods without a migration recipe: {missing:?}"
        );
    }

    #[test]
    fn runtime_registered_store_methods_migrate_to_their_owning_capability() {
        for method in ["store_get", "store_set", "store_delete", "store_list"] {
            let migration =
                harness_migration_for_builtin(method).expect("store method has a migration");
            assert_eq!(
                migration.capability,
                harn_builtin_meta::CapabilityId::Runtime
            );
            assert_eq!(migration.method, method);
            assert_eq!(
                migration.arguments,
                HarnessBuiltinArgumentMigration::Forward
            );
        }
    }

    /// A name two capabilities both answer to has no single rewrite target,
    /// so it must stay uncovered rather than pick an owner arbitrarily.
    #[test]
    fn ambiguously_owned_methods_have_no_migration() {
        let vm = stdlib_probe_vm();
        let mut owners: std::collections::BTreeMap<String, std::collections::BTreeSet<_>> =
            std::collections::BTreeMap::new();
        for (capability, method) in vm.capability_method_names() {
            owners.entry(method).or_default().insert(capability);
        }
        let Some((method, _)) = owners
            .iter()
            .find(|(method, capabilities)| {
                capabilities.len() > 1 && super::harness_method_for_builtin(method).is_none()
            })
            .map(|(method, capabilities)| (method.clone(), capabilities.clone()))
        else {
            return;
        };
        assert!(
            super::derived_capability_owner(&method).is_none(),
            "`{method}` is owned by several capabilities and must not resolve to one"
        );
    }
}
