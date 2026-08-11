//! Optional host capability surface shared by parser, VM, and embedders.
//!
//! Each group is a closed, typed contract. Implementations may be supplied by
//! `harn-hostlib` or another embedder, but registration never changes the
//! language namespace or the maximum effect summary.

use crate::{CapabilityId, EffectAccess, EffectKind, EffectSpec, ResourceSelector};

#[derive(Debug, Clone, Copy)]
pub struct HostCapabilityGroup {
    pub capability: CapabilityId,
    pub methods: &'static [&'static str],
    pub effects: &'static [EffectSpec],
}

/// Resolve a hostlib schema name to its typed `Harness` capability method.
///
/// Most schema modules project directly to a same-named capability. Session
/// persistence is deliberately owned by `HarnessAgent`, so its compact
/// `session.*` hostlib schema is exposed as `harness.agent.session_*`.
pub fn capability_binding_for_schema(
    module: &str,
    method: &str,
) -> Option<(CapabilityId, &'static str)> {
    let capability = match module {
        "ast" => CapabilityId::Ast,
        "code_index" => CapabilityId::CodeIndex,
        "computer" => CapabilityId::Computer,
        "embed" => CapabilityId::Embed,
        "fs" => CapabilityId::Fs,
        "fs_watch" => CapabilityId::FsWatch,
        "host_conditions" => CapabilityId::System,
        "host_lease" => CapabilityId::HostLease,
        "lint" => CapabilityId::Lint,
        "rules" => CapabilityId::Rules,
        "scanner" => CapabilityId::Scanner,
        "secret_store" => CapabilityId::SecretStore,
        "session" => CapabilityId::Agent,
        "terminal_session" => CapabilityId::TerminalSession,
        "tools" => CapabilityId::Tools,
        "verdict" => CapabilityId::Verdict,
        _ => return None,
    };
    let requested_method = match module {
        "host_conditions" => match method {
            "sample" => "host_conditions",
            _ => return None,
        },
        "session" => match method {
            "open" => "session_open",
            "update" => "session_update",
            "append" => "session_append",
            "close" => "session_close",
            "get" => "session_get",
            "list" => "session_list",
            "fork" => "session_fork",
            "search_fts" => "session_search_fts",
            "search_semantic" => "session_search_semantic",
            "search_hybrid" => "session_search_hybrid",
            _ => return None,
        },
        _ => method,
    };
    let capability_method = HOST_CAPABILITY_GROUPS
        .iter()
        .filter(|group| group.capability == capability)
        .flat_map(|group| group.methods.iter().copied())
        .find(|candidate| *candidate == requested_method)?;
    Some((capability, capability_method))
}

/// Decode one historical `hostlib_<module>_<method>` spelling through the
/// authoritative typed capability registry.
///
/// Module and method names may both contain underscores, so each boundary is
/// tried against `capability_binding_for_schema` instead of maintaining a
/// second module-name table in compatibility consumers.
#[expect(
    clippy::string_slice,
    reason = "match_indices('_') cuts at a one-byte char, so both slice ends are char boundaries"
)]
pub fn capability_binding_for_legacy_hostlib_name(
    name: &str,
) -> Option<(CapabilityId, &'static str)> {
    let suffix = name.strip_prefix("hostlib_")?;
    suffix.match_indices('_').find_map(|(boundary, _)| {
        capability_binding_for_schema(&suffix[..boundary], &suffix[boundary + 1..])
    })
}

const DYNAMIC: &[ResourceSelector] = &[ResourceSelector::Dynamic];
const FS_READ: &[EffectSpec] = &[EffectSpec::new(EffectKind::Fs, EffectAccess::Read, DYNAMIC)];
const FS_WRITE: &[EffectSpec] = &[EffectSpec::new(
    EffectKind::Fs,
    EffectAccess::Write,
    DYNAMIC,
)];
const FS_MUTATE: &[EffectSpec] = &[EffectSpec::new(
    EffectKind::Fs,
    EffectAccess::Mutate,
    DYNAMIC,
)];
const FS_OBSERVE: &[EffectSpec] = &[EffectSpec::new(
    EffectKind::Fs,
    EffectAccess::Observe,
    DYNAMIC,
)];
const PROCESS_WRITE: &[EffectSpec] = &[EffectSpec::new(
    EffectKind::Process,
    EffectAccess::Write,
    DYNAMIC,
)];
const PROCESS_OBSERVE: &[EffectSpec] = &[EffectSpec::new(
    EffectKind::Process,
    EffectAccess::Observe,
    DYNAMIC,
)];
const STATE_READ: &[EffectSpec] = &[EffectSpec::new(
    EffectKind::State,
    EffectAccess::Read,
    DYNAMIC,
)];
const STATE_MUTATE: &[EffectSpec] = &[EffectSpec::new(
    EffectKind::State,
    EffectAccess::Mutate,
    DYNAMIC,
)];
const SECRET_READ: &[EffectSpec] = &[EffectSpec::new(
    EffectKind::Secret,
    EffectAccess::Read,
    DYNAMIC,
)];
const SECRET_MUTATE: &[EffectSpec] = &[EffectSpec::new(
    EffectKind::Secret,
    EffectAccess::Mutate,
    DYNAMIC,
)];
const HOST_READ: &[EffectSpec] = &[EffectSpec::new(
    EffectKind::Host,
    EffectAccess::Read,
    DYNAMIC,
)];
const HOST_MUTATE: &[EffectSpec] = &[EffectSpec::new(
    EffectKind::Host,
    EffectAccess::Mutate,
    DYNAMIC,
)];
/// Assistant-visible response projection. Classified like `stdio.write` so ACP
/// presentation ceilings (`read_only`) admit the typed
/// `harness.runtime.emit_response` path after capability migration, instead of
/// treating a UI stream as a workspace-mutating host call (harn#6374).
const PRESENTATION_WRITE: &[EffectSpec] = &[EffectSpec::new(
    EffectKind::Stdio,
    EffectAccess::Write,
    &[ResourceSelector::Constant("stdout")],
)];
const SESSION_READ: &[EffectSpec] = &[
    EffectSpec::new(EffectKind::Fs, EffectAccess::Read, DYNAMIC),
    EffectSpec::new(EffectKind::State, EffectAccess::Read, DYNAMIC),
];
const SESSION_MUTATE: &[EffectSpec] = &[
    EffectSpec::new(EffectKind::Fs, EffectAccess::Write, DYNAMIC),
    EffectSpec::new(EffectKind::State, EffectAccess::Mutate, DYNAMIC),
];

pub const HOST_CAPABILITY_GROUPS: &[HostCapabilityGroup] = &[
    HostCapabilityGroup {
        capability: CapabilityId::Ast,
        methods: &[
            "bracket_balance",
            "capabilities",
            "dry_run",
            "extract_imports",
            "function_bodies",
            "function_body",
            "outline",
            "parse_errors",
            "parse_file",
            "search",
            "structural_diff",
            "symbol_extract",
            "symbols",
            "undefined_names",
        ],
        effects: FS_READ,
    },
    HostCapabilityGroup {
        capability: CapabilityId::Ast,
        methods: &["changeset_summary"],
        effects: HOST_READ,
    },
    HostCapabilityGroup {
        capability: CapabilityId::Ast,
        methods: &[
            "apply_node",
            "batch_apply",
            "insert_at_anchor",
            "symbol_delete",
            "symbol_replace",
        ],
        effects: FS_MUTATE,
    },
    HostCapabilityGroup {
        capability: CapabilityId::CodeIndex,
        methods: &[
            "changes_since",
            "current_agent_id",
            "current_seq",
            "cypher",
            "deps_get",
            "extract_trigrams",
            "file_hash",
            "file_hash_snapshot",
            "file_ids",
            "file_meta",
            "freshness",
            "id_to_path",
            "importers_of",
            "imports_for",
            "outline_get",
            "path_to_id",
            "query",
            "read_range",
            "repo_map",
            "stats",
            "status",
            "trigram_query",
            "word_get",
        ],
        effects: STATE_READ,
    },
    HostCapabilityGroup {
        capability: CapabilityId::CodeIndex,
        methods: &[
            "add_readonly_roots",
            "agent_heartbeat",
            "agent_register",
            "agent_unregister",
            "branch_overlay",
            "lock_release",
            "lock_try",
            "rebuild",
            "reindex_file",
            "rename_symbol",
            "version_record",
        ],
        effects: STATE_MUTATE,
    },
    HostCapabilityGroup {
        capability: CapabilityId::Embed,
        methods: &["info", "similarity", "top_k", "vector"],
        effects: &[],
    },
    HostCapabilityGroup {
        capability: CapabilityId::Agent,
        methods: &[
            "session_get",
            "session_list",
            "session_search_fts",
            "session_search_hybrid",
            "session_search_semantic",
        ],
        effects: SESSION_READ,
    },
    HostCapabilityGroup {
        capability: CapabilityId::Agent,
        methods: &[
            "session_append",
            "session_close",
            "session_fork",
            "session_open",
            "session_update",
        ],
        effects: SESSION_MUTATE,
    },
    HostCapabilityGroup {
        capability: CapabilityId::Fs,
        methods: &["list_snapshots", "staged_read_text", "staged_status"],
        effects: FS_READ,
    },
    HostCapabilityGroup {
        capability: CapabilityId::Fs,
        methods: &["snapshot"],
        effects: FS_WRITE,
    },
    HostCapabilityGroup {
        capability: CapabilityId::Fs,
        methods: &[
            "commit_staged",
            "discard_staged",
            "drop_snapshot",
            "emit_safe_text_patch_result",
            "restore",
            "safe_text_patch",
            "set_mode",
        ],
        effects: FS_MUTATE,
    },
    HostCapabilityGroup {
        capability: CapabilityId::FsWatch,
        methods: &["subscribe", "unsubscribe"],
        effects: FS_OBSERVE,
    },
    HostCapabilityGroup {
        capability: CapabilityId::HostLease,
        methods: &["status"],
        effects: STATE_READ,
    },
    HostCapabilityGroup {
        capability: CapabilityId::HostLease,
        methods: &["acquire", "release", "update_metadata"],
        effects: STATE_MUTATE,
    },
    HostCapabilityGroup {
        capability: CapabilityId::Rules,
        methods: &["apply", "diagnostics", "fold", "report", "search"],
        effects: &[],
    },
    HostCapabilityGroup {
        capability: CapabilityId::Lint,
        methods: &["run"],
        effects: &[],
    },
    HostCapabilityGroup {
        capability: CapabilityId::Scanner,
        methods: &["scan_incremental", "scan_project"],
        effects: FS_READ,
    },
    HostCapabilityGroup {
        capability: CapabilityId::SecretStore,
        methods: &["get", "list"],
        effects: SECRET_READ,
    },
    HostCapabilityGroup {
        capability: CapabilityId::SecretStore,
        methods: &["delete", "set"],
        effects: SECRET_MUTATE,
    },
    HostCapabilityGroup {
        capability: CapabilityId::System,
        methods: &["host_conditions"],
        effects: HOST_READ,
    },
    HostCapabilityGroup {
        capability: CapabilityId::TerminalSession,
        methods: &["capture", "wait_idle"],
        effects: PROCESS_OBSERVE,
    },
    HostCapabilityGroup {
        capability: CapabilityId::TerminalSession,
        methods: &["end", "resize", "send_keys", "start"],
        effects: PROCESS_WRITE,
    },
    HostCapabilityGroup {
        capability: CapabilityId::Tools,
        methods: &[
            "get_file_outline",
            "inspect_test_results",
            "list_directory",
            "read_file",
            "search",
            "toolchain_facts",
        ],
        effects: FS_READ,
    },
    HostCapabilityGroup {
        capability: CapabilityId::Tools,
        methods: &["delete_file", "manage_packages", "write_file"],
        effects: FS_MUTATE,
    },
    HostCapabilityGroup {
        capability: CapabilityId::Tools,
        methods: &[
            "cancel_handle",
            "git",
            "run_build_command",
            "run_command",
            "run_test",
        ],
        effects: PROCESS_WRITE,
    },
    HostCapabilityGroup {
        capability: CapabilityId::Tools,
        methods: &[
            "list_handles",
            "read_command_output",
            "wait_command",
            "wait_command_output",
        ],
        effects: PROCESS_OBSERVE,
    },
    HostCapabilityGroup {
        capability: CapabilityId::Verdict,
        methods: &["issue"],
        effects: HOST_MUTATE,
    },
    HostCapabilityGroup {
        capability: CapabilityId::Computer,
        methods: &["permissions", "screenshot", "ui_tree"],
        effects: HOST_READ,
    },
    HostCapabilityGroup {
        capability: CapabilityId::Computer,
        methods: &["execute"],
        effects: HOST_MUTATE,
    },
];

/// Embedder-owned protocol methods with a language-level typed contract.
///
/// Unlike [`HOST_CAPABILITY_GROUPS`], these methods do not claim a portable
/// `harn-hostlib` implementation or request/response schema. They remain a
/// closed language namespace and carry an effect ceiling, while the embedding
/// application owns the richer protocol schema and concrete implementation.
pub const EMBEDDER_CAPABILITY_GROUPS: &[HostCapabilityGroup] = &[
    HostCapabilityGroup {
        capability: CapabilityId::Agent,
        methods: &["emit_plan", "request_planner_question"],
        effects: HOST_MUTATE,
    },
    HostCapabilityGroup {
        capability: CapabilityId::Ast,
        methods: &[
            "diagnose_undefined",
            "extract_function_body",
            "extract_symbol",
            "file_outline",
            "imports",
            "symbol_suggestions",
        ],
        effects: FS_READ,
    },
    HostCapabilityGroup {
        capability: CapabilityId::Ast,
        methods: &["remove_symbol", "replace_symbol"],
        effects: FS_MUTATE,
    },
    HostCapabilityGroup {
        capability: CapabilityId::Dashboard,
        methods: &["get_state"],
        effects: STATE_READ,
    },
    HostCapabilityGroup {
        capability: CapabilityId::Dashboard,
        methods: &[
            "attach_widget",
            "create_task",
            "open_canvas",
            "patch_state",
            "schedule_job",
            "update_task",
            "write_bulletin",
        ],
        effects: STATE_MUTATE,
    },
    HostCapabilityGroup {
        capability: CapabilityId::Workspace,
        methods: &[
            "metadata_summary",
            "project_root",
            "search",
            "validate_structured",
        ],
        effects: FS_READ,
    },
    HostCapabilityGroup {
        capability: CapabilityId::Workspace,
        methods: &["service_status"],
        effects: PROCESS_OBSERVE,
    },
    HostCapabilityGroup {
        capability: CapabilityId::Workspace,
        methods: &[
            "append_jsonl",
            "compare_swap_text",
            "create_directory",
            "delete",
            "invalidate_caches",
            "move",
            "write_text",
        ],
        effects: FS_MUTATE,
    },
    HostCapabilityGroup {
        capability: CapabilityId::Workspace,
        methods: &["service_stop"],
        effects: PROCESS_WRITE,
    },
    HostCapabilityGroup {
        capability: CapabilityId::Env,
        methods: &["host", "list", "scan"],
        effects: HOST_READ,
    },
    HostCapabilityGroup {
        capability: CapabilityId::Env,
        methods: &["install"],
        effects: HOST_MUTATE,
    },
    HostCapabilityGroup {
        capability: CapabilityId::MergeCaptain,
        methods: &["status"],
        effects: STATE_READ,
    },
    HostCapabilityGroup {
        capability: CapabilityId::MergeCaptain,
        methods: &["disable", "pause", "resume", "sweep"],
        effects: STATE_MUTATE,
    },
    HostCapabilityGroup {
        capability: CapabilityId::Process,
        methods: &["list_handles", "read_handle"],
        effects: PROCESS_OBSERVE,
    },
    HostCapabilityGroup {
        capability: CapabilityId::Process,
        methods: &["cancel"],
        effects: PROCESS_WRITE,
    },
    HostCapabilityGroup {
        capability: CapabilityId::Project,
        methods: &[
            "file_index",
            "intake_status",
            "mcp_config",
            "peer_presence",
            "skills",
            "symbol_index",
            "symbol_rank",
            "symbol_similarity",
        ],
        effects: STATE_READ,
    },
    HostCapabilityGroup {
        capability: CapabilityId::Project,
        methods: &[
            "compute_content_hash",
            "ensure_enriched",
            "generate_skills",
            "generate_templates",
            "peer_message",
        ],
        effects: STATE_MUTATE,
    },
    HostCapabilityGroup {
        capability: CapabilityId::Session,
        methods: &[
            "active_roots",
            "changed_paths",
            "preread_get",
            "preread_read_many",
        ],
        effects: STATE_READ,
    },
    HostCapabilityGroup {
        capability: CapabilityId::Session,
        methods: &["preread_put"],
        effects: STATE_MUTATE,
    },
    HostCapabilityGroup {
        capability: CapabilityId::Permission,
        methods: &["request"],
        effects: HOST_MUTATE,
    },
    HostCapabilityGroup {
        capability: CapabilityId::Text,
        methods: &["bracket_balance"],
        effects: &[],
    },
    HostCapabilityGroup {
        capability: CapabilityId::Lsp,
        methods: &["definition", "diagnostics", "hover", "references"],
        effects: FS_READ,
    },
    HostCapabilityGroup {
        capability: CapabilityId::Lsp,
        methods: &["rename"],
        effects: FS_MUTATE,
    },
    HostCapabilityGroup {
        capability: CapabilityId::Credentials,
        methods: &["get", "list"],
        effects: SECRET_READ,
    },
    HostCapabilityGroup {
        capability: CapabilityId::Credentials,
        methods: &["delete", "set"],
        effects: SECRET_MUTATE,
    },
    HostCapabilityGroup {
        capability: CapabilityId::Runtime,
        methods: &[
            "hook_stats",
            "qc_default_model",
            "read_resource",
            "reminder_rules",
        ],
        effects: HOST_READ,
    },
    HostCapabilityGroup {
        capability: CapabilityId::Runtime,
        methods: &["emit_response"],
        effects: PRESENTATION_WRITE,
    },
    HostCapabilityGroup {
        capability: CapabilityId::Runtime,
        methods: &[
            "execute_hook",
            "run_pipeline",
            "set_feature_audit",
            "set_resolved_flags",
            "set_tool_guard",
        ],
        effects: HOST_MUTATE,
    },
    HostCapabilityGroup {
        capability: CapabilityId::PrMonitor,
        methods: &[
            "gh_snapshot",
            "log_query",
            "now_iso",
            "resolve_repository",
            "schedule_iso",
        ],
        effects: HOST_READ,
    },
    HostCapabilityGroup {
        capability: CapabilityId::PrMonitor,
        methods: &["append_supervision", "persist_bundle", "persist_report"],
        effects: FS_MUTATE,
    },
    HostCapabilityGroup {
        capability: CapabilityId::PrMonitor,
        methods: &["commit_and_push", "prepare_worktree", "run_commands"],
        effects: PROCESS_WRITE,
    },
    HostCapabilityGroup {
        capability: CapabilityId::Workflow,
        methods: &[
            "inspect_bundle",
            "inspect_candidate",
            "inspect_proposal",
            "list",
        ],
        effects: STATE_READ,
    },
    HostCapabilityGroup {
        capability: CapabilityId::Workflow,
        methods: &["patch", "run_bundle", "shadow"],
        effects: STATE_MUTATE,
    },
];

pub fn all_host_capability_groups() -> impl Iterator<Item = &'static HostCapabilityGroup> {
    HOST_CAPABILITY_GROUPS
        .iter()
        .chain(EMBEDDER_CAPABILITY_GROUPS)
}

pub fn is_host_capability_method(capability: CapabilityId, method: &str) -> bool {
    all_host_capability_groups()
        .any(|group| group.capability == capability && group.methods.contains(&method))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn portable_and_embedder_capability_methods_are_disjoint() {
        let portable: BTreeSet<_> = HOST_CAPABILITY_GROUPS
            .iter()
            .flat_map(|group| {
                group
                    .methods
                    .iter()
                    .map(move |method| (group.capability, *method))
            })
            .collect();
        let embedder: BTreeSet<_> = EMBEDDER_CAPABILITY_GROUPS
            .iter()
            .flat_map(|group| {
                group
                    .methods
                    .iter()
                    .map(move |method| (group.capability, *method))
            })
            .collect();

        let overlap: Vec<_> = portable.intersection(&embedder).collect();
        assert!(
            overlap.is_empty(),
            "a capability method must have exactly one implementation owner: {overlap:?}"
        );
    }

    #[test]
    fn every_capability_method_is_declared_once() {
        let mut seen = BTreeSet::new();
        for group in all_host_capability_groups() {
            for method in group.methods {
                assert!(
                    seen.insert((group.capability, *method)),
                    "duplicate capability method contract: {}.{method}",
                    group.capability.field_name()
                );
            }
        }
    }

    #[test]
    fn host_conditions_schema_binds_to_the_typed_system_method() {
        assert_eq!(
            capability_binding_for_schema("host_conditions", "sample"),
            Some((CapabilityId::System, "host_conditions"))
        );
        assert!(is_host_capability_method(
            CapabilityId::System,
            "host_conditions"
        ));
        assert!(!is_host_capability_method(CapabilityId::System, "sample"));
    }

    #[test]
    fn runtime_emit_response_is_presentation_not_host_mutate() {
        let emit = all_host_capability_groups()
            .find(|group| {
                group.capability == CapabilityId::Runtime
                    && group.methods.contains(&"emit_response")
            })
            .expect("runtime.emit_response contract");

        assert_eq!(emit.effects, PRESENTATION_WRITE);
        assert_ne!(
            emit.effects, HOST_MUTATE,
            "emit_response must stay under presentation ceilings so ACP typed callers survive capability migration"
        );
    }

    #[test]
    fn mutating_embedder_methods_do_not_claim_read_only_effects() {
        let rename = all_host_capability_groups()
            .find(|group| {
                group.capability == CapabilityId::Lsp && group.methods.contains(&"rename")
            })
            .expect("lsp.rename contract");

        assert_eq!(rename.effects, FS_MUTATE);

        let service_stop = all_host_capability_groups()
            .find(|group| {
                group.capability == CapabilityId::Workspace
                    && group.methods.contains(&"service_stop")
            })
            .expect("workspace.service_stop contract");
        assert_eq!(service_stop.effects, PROCESS_WRITE);

        let run_commands = all_host_capability_groups()
            .find(|group| {
                group.capability == CapabilityId::PrMonitor
                    && group.methods.contains(&"run_commands")
            })
            .expect("pr_monitor.run_commands contract");
        assert_eq!(run_commands.effects, PROCESS_WRITE);
    }
}
