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
