//! Embedded JSON Schemas for every hostlib host method.
//!
//! Schemas live at `schemas/<module>/<method>.{request,response}.json` and
//! are baked into the crate at compile time via `include_str!`. They're the
//! source of truth for hostlib request/response compatibility: the schema
//! files ship with the crate (see the `include` field in `Cargo.toml`),
//! and consumers fetch them through this module.
//!
//! Schemas use JSON Schema draft 2020-12.

use std::sync::OnceLock;

use harn_vm::{VmDictExt, VmValue};

use crate::error::HostlibError;

/// Direction of a schema (request body vs. response body).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SchemaKind {
    /// Schema for the *input* of a host method.
    Request,
    /// Schema for the *output* of a host method.
    Response,
}

/// One `(module, method, kind, schema_text)` tuple for every shipped schema.
///
/// Embedders use this catalog to:
/// - assert that every registered builtin has a matching schema (drift test);
/// - export the schemas to downstream consumers;
/// - validate live request/response payloads in tests.
pub const SCHEMAS: &[(&str, &str, SchemaKind, &str)] = &[
    // ast/
    (
        "ast",
        "parse_file",
        SchemaKind::Request,
        include_str!("../schemas/ast/parse_file.request.json"),
    ),
    (
        "ast",
        "parse_file",
        SchemaKind::Response,
        include_str!("../schemas/ast/parse_file.response.json"),
    ),
    (
        "ast",
        "symbols",
        SchemaKind::Request,
        include_str!("../schemas/ast/symbols.request.json"),
    ),
    (
        "ast",
        "symbols",
        SchemaKind::Response,
        include_str!("../schemas/ast/symbols.response.json"),
    ),
    (
        "ast",
        "outline",
        SchemaKind::Request,
        include_str!("../schemas/ast/outline.request.json"),
    ),
    (
        "ast",
        "outline",
        SchemaKind::Response,
        include_str!("../schemas/ast/outline.response.json"),
    ),
    (
        "ast",
        "parse_errors",
        SchemaKind::Request,
        include_str!("../schemas/ast/parse_errors.request.json"),
    ),
    (
        "ast",
        "parse_errors",
        SchemaKind::Response,
        include_str!("../schemas/ast/parse_errors.response.json"),
    ),
    (
        "ast",
        "undefined_names",
        SchemaKind::Request,
        include_str!("../schemas/ast/undefined_names.request.json"),
    ),
    (
        "ast",
        "undefined_names",
        SchemaKind::Response,
        include_str!("../schemas/ast/undefined_names.response.json"),
    ),
    (
        "ast",
        "function_body",
        SchemaKind::Request,
        include_str!("../schemas/ast/function_body.request.json"),
    ),
    (
        "ast",
        "function_body",
        SchemaKind::Response,
        include_str!("../schemas/ast/function_body.response.json"),
    ),
    (
        "ast",
        "function_bodies",
        SchemaKind::Request,
        include_str!("../schemas/ast/function_bodies.request.json"),
    ),
    (
        "ast",
        "function_bodies",
        SchemaKind::Response,
        include_str!("../schemas/ast/function_bodies.response.json"),
    ),
    (
        "ast",
        "extract_imports",
        SchemaKind::Request,
        include_str!("../schemas/ast/extract_imports.request.json"),
    ),
    (
        "ast",
        "extract_imports",
        SchemaKind::Response,
        include_str!("../schemas/ast/extract_imports.response.json"),
    ),
    (
        "ast",
        "symbol_extract",
        SchemaKind::Request,
        include_str!("../schemas/ast/symbol_extract.request.json"),
    ),
    (
        "ast",
        "symbol_extract",
        SchemaKind::Response,
        include_str!("../schemas/ast/symbol_extract.response.json"),
    ),
    (
        "ast",
        "symbol_delete",
        SchemaKind::Request,
        include_str!("../schemas/ast/symbol_delete.request.json"),
    ),
    (
        "ast",
        "symbol_delete",
        SchemaKind::Response,
        include_str!("../schemas/ast/symbol_delete.response.json"),
    ),
    (
        "ast",
        "symbol_replace",
        SchemaKind::Request,
        include_str!("../schemas/ast/symbol_replace.request.json"),
    ),
    (
        "ast",
        "symbol_replace",
        SchemaKind::Response,
        include_str!("../schemas/ast/symbol_replace.response.json"),
    ),
    (
        "ast",
        "bracket_balance",
        SchemaKind::Request,
        include_str!("../schemas/ast/bracket_balance.request.json"),
    ),
    (
        "ast",
        "bracket_balance",
        SchemaKind::Response,
        include_str!("../schemas/ast/bracket_balance.response.json"),
    ),
    (
        "ast",
        "apply_node",
        SchemaKind::Request,
        include_str!("../schemas/ast/apply_node.request.json"),
    ),
    (
        "ast",
        "apply_node",
        SchemaKind::Response,
        include_str!("../schemas/ast/apply_node.response.json"),
    ),
    (
        "ast",
        "insert_at_anchor",
        SchemaKind::Request,
        include_str!("../schemas/ast/insert_at_anchor.request.json"),
    ),
    (
        "ast",
        "insert_at_anchor",
        SchemaKind::Response,
        include_str!("../schemas/ast/insert_at_anchor.response.json"),
    ),
    (
        "ast",
        "batch_apply",
        SchemaKind::Request,
        include_str!("../schemas/ast/batch_apply.request.json"),
    ),
    (
        "ast",
        "batch_apply",
        SchemaKind::Response,
        include_str!("../schemas/ast/batch_apply.response.json"),
    ),
    (
        "ast",
        "dry_run",
        SchemaKind::Request,
        include_str!("../schemas/ast/dry_run.request.json"),
    ),
    (
        "ast",
        "dry_run",
        SchemaKind::Response,
        include_str!("../schemas/ast/dry_run.response.json"),
    ),
    (
        "ast",
        "search",
        SchemaKind::Request,
        include_str!("../schemas/ast/search.request.json"),
    ),
    (
        "ast",
        "search",
        SchemaKind::Response,
        include_str!("../schemas/ast/search.response.json"),
    ),
    (
        "ast",
        "structural_diff",
        SchemaKind::Request,
        include_str!("../schemas/ast/structural_diff.request.json"),
    ),
    (
        "ast",
        "structural_diff",
        SchemaKind::Response,
        include_str!("../schemas/ast/structural_diff.response.json"),
    ),
    (
        "ast",
        "capabilities",
        SchemaKind::Request,
        include_str!("../schemas/ast/capabilities.request.json"),
    ),
    (
        "ast",
        "capabilities",
        SchemaKind::Response,
        include_str!("../schemas/ast/capabilities.response.json"),
    ),
    // code_index/
    (
        "code_index",
        "query",
        SchemaKind::Request,
        include_str!("../schemas/code_index/query.request.json"),
    ),
    (
        "code_index",
        "query",
        SchemaKind::Response,
        include_str!("../schemas/code_index/query.response.json"),
    ),
    (
        "code_index",
        "rebuild",
        SchemaKind::Request,
        include_str!("../schemas/code_index/rebuild.request.json"),
    ),
    (
        "code_index",
        "rebuild",
        SchemaKind::Response,
        include_str!("../schemas/code_index/rebuild.response.json"),
    ),
    (
        "code_index",
        "stats",
        SchemaKind::Request,
        include_str!("../schemas/code_index/stats.request.json"),
    ),
    (
        "code_index",
        "stats",
        SchemaKind::Response,
        include_str!("../schemas/code_index/stats.response.json"),
    ),
    (
        "code_index",
        "imports_for",
        SchemaKind::Request,
        include_str!("../schemas/code_index/imports_for.request.json"),
    ),
    (
        "code_index",
        "imports_for",
        SchemaKind::Response,
        include_str!("../schemas/code_index/imports_for.response.json"),
    ),
    (
        "code_index",
        "importers_of",
        SchemaKind::Request,
        include_str!("../schemas/code_index/importers_of.request.json"),
    ),
    (
        "code_index",
        "importers_of",
        SchemaKind::Response,
        include_str!("../schemas/code_index/importers_of.response.json"),
    ),
    // code_index — additive read-only secondary roots (#2403 follow-up)
    (
        "code_index",
        "add_readonly_roots",
        SchemaKind::Request,
        include_str!("../schemas/code_index/add_readonly_roots.request.json"),
    ),
    (
        "code_index",
        "add_readonly_roots",
        SchemaKind::Response,
        include_str!("../schemas/code_index/add_readonly_roots.response.json"),
    ),
    // code_index — file table accessors
    (
        "code_index",
        "path_to_id",
        SchemaKind::Request,
        include_str!("../schemas/code_index/path_to_id.request.json"),
    ),
    (
        "code_index",
        "path_to_id",
        SchemaKind::Response,
        include_str!("../schemas/code_index/path_to_id.response.json"),
    ),
    (
        "code_index",
        "id_to_path",
        SchemaKind::Request,
        include_str!("../schemas/code_index/id_to_path.request.json"),
    ),
    (
        "code_index",
        "id_to_path",
        SchemaKind::Response,
        include_str!("../schemas/code_index/id_to_path.response.json"),
    ),
    (
        "code_index",
        "file_ids",
        SchemaKind::Request,
        include_str!("../schemas/code_index/file_ids.request.json"),
    ),
    (
        "code_index",
        "file_ids",
        SchemaKind::Response,
        include_str!("../schemas/code_index/file_ids.response.json"),
    ),
    (
        "code_index",
        "file_meta",
        SchemaKind::Request,
        include_str!("../schemas/code_index/file_meta.request.json"),
    ),
    (
        "code_index",
        "file_meta",
        SchemaKind::Response,
        include_str!("../schemas/code_index/file_meta.response.json"),
    ),
    (
        "code_index",
        "file_hash",
        SchemaKind::Request,
        include_str!("../schemas/code_index/file_hash.request.json"),
    ),
    (
        "code_index",
        "file_hash",
        SchemaKind::Response,
        include_str!("../schemas/code_index/file_hash.response.json"),
    ),
    (
        "code_index",
        "file_hash_snapshot",
        SchemaKind::Request,
        include_str!("../schemas/code_index/file_hash_snapshot.request.json"),
    ),
    (
        "code_index",
        "file_hash_snapshot",
        SchemaKind::Response,
        include_str!("../schemas/code_index/file_hash_snapshot.response.json"),
    ),
    // code_index — cached reads
    (
        "code_index",
        "read_range",
        SchemaKind::Request,
        include_str!("../schemas/code_index/read_range.request.json"),
    ),
    (
        "code_index",
        "read_range",
        SchemaKind::Response,
        include_str!("../schemas/code_index/read_range.response.json"),
    ),
    (
        "code_index",
        "reindex_file",
        SchemaKind::Request,
        include_str!("../schemas/code_index/reindex_file.request.json"),
    ),
    (
        "code_index",
        "reindex_file",
        SchemaKind::Response,
        include_str!("../schemas/code_index/reindex_file.response.json"),
    ),
    (
        "code_index",
        "trigram_query",
        SchemaKind::Request,
        include_str!("../schemas/code_index/trigram_query.request.json"),
    ),
    (
        "code_index",
        "trigram_query",
        SchemaKind::Response,
        include_str!("../schemas/code_index/trigram_query.response.json"),
    ),
    (
        "code_index",
        "extract_trigrams",
        SchemaKind::Request,
        include_str!("../schemas/code_index/extract_trigrams.request.json"),
    ),
    (
        "code_index",
        "extract_trigrams",
        SchemaKind::Response,
        include_str!("../schemas/code_index/extract_trigrams.response.json"),
    ),
    (
        "code_index",
        "word_get",
        SchemaKind::Request,
        include_str!("../schemas/code_index/word_get.request.json"),
    ),
    (
        "code_index",
        "word_get",
        SchemaKind::Response,
        include_str!("../schemas/code_index/word_get.response.json"),
    ),
    (
        "code_index",
        "deps_get",
        SchemaKind::Request,
        include_str!("../schemas/code_index/deps_get.request.json"),
    ),
    (
        "code_index",
        "deps_get",
        SchemaKind::Response,
        include_str!("../schemas/code_index/deps_get.response.json"),
    ),
    (
        "code_index",
        "outline_get",
        SchemaKind::Request,
        include_str!("../schemas/code_index/outline_get.request.json"),
    ),
    (
        "code_index",
        "outline_get",
        SchemaKind::Response,
        include_str!("../schemas/code_index/outline_get.response.json"),
    ),
    // code_index — change log
    (
        "code_index",
        "current_seq",
        SchemaKind::Request,
        include_str!("../schemas/code_index/current_seq.request.json"),
    ),
    (
        "code_index",
        "current_seq",
        SchemaKind::Response,
        include_str!("../schemas/code_index/current_seq.response.json"),
    ),
    (
        "code_index",
        "changes_since",
        SchemaKind::Request,
        include_str!("../schemas/code_index/changes_since.request.json"),
    ),
    (
        "code_index",
        "changes_since",
        SchemaKind::Response,
        include_str!("../schemas/code_index/changes_since.response.json"),
    ),
    (
        "code_index",
        "version_record",
        SchemaKind::Request,
        include_str!("../schemas/code_index/version_record.request.json"),
    ),
    (
        "code_index",
        "version_record",
        SchemaKind::Response,
        include_str!("../schemas/code_index/version_record.response.json"),
    ),
    // code_index — agents + locks
    (
        "code_index",
        "agent_register",
        SchemaKind::Request,
        include_str!("../schemas/code_index/agent_register.request.json"),
    ),
    (
        "code_index",
        "agent_register",
        SchemaKind::Response,
        include_str!("../schemas/code_index/agent_register.response.json"),
    ),
    (
        "code_index",
        "agent_heartbeat",
        SchemaKind::Request,
        include_str!("../schemas/code_index/agent_heartbeat.request.json"),
    ),
    (
        "code_index",
        "agent_heartbeat",
        SchemaKind::Response,
        include_str!("../schemas/code_index/agent_heartbeat.response.json"),
    ),
    (
        "code_index",
        "agent_unregister",
        SchemaKind::Request,
        include_str!("../schemas/code_index/agent_unregister.request.json"),
    ),
    (
        "code_index",
        "agent_unregister",
        SchemaKind::Response,
        include_str!("../schemas/code_index/agent_unregister.response.json"),
    ),
    (
        "code_index",
        "lock_try",
        SchemaKind::Request,
        include_str!("../schemas/code_index/lock_try.request.json"),
    ),
    (
        "code_index",
        "lock_try",
        SchemaKind::Response,
        include_str!("../schemas/code_index/lock_try.response.json"),
    ),
    (
        "code_index",
        "lock_release",
        SchemaKind::Request,
        include_str!("../schemas/code_index/lock_release.request.json"),
    ),
    (
        "code_index",
        "lock_release",
        SchemaKind::Response,
        include_str!("../schemas/code_index/lock_release.response.json"),
    ),
    (
        "code_index",
        "status",
        SchemaKind::Request,
        include_str!("../schemas/code_index/status.request.json"),
    ),
    (
        "code_index",
        "status",
        SchemaKind::Response,
        include_str!("../schemas/code_index/status.response.json"),
    ),
    (
        "code_index",
        "current_agent_id",
        SchemaKind::Request,
        include_str!("../schemas/code_index/current_agent_id.request.json"),
    ),
    (
        "code_index",
        "current_agent_id",
        SchemaKind::Response,
        include_str!("../schemas/code_index/current_agent_id.response.json"),
    ),
    (
        "code_index",
        "cypher",
        SchemaKind::Request,
        include_str!("../schemas/code_index/cypher.request.json"),
    ),
    (
        "code_index",
        "cypher",
        SchemaKind::Response,
        include_str!("../schemas/code_index/cypher.response.json"),
    ),
    (
        "code_index",
        "repo_map",
        SchemaKind::Request,
        include_str!("../schemas/code_index/repo_map.request.json"),
    ),
    (
        "code_index",
        "repo_map",
        SchemaKind::Response,
        include_str!("../schemas/code_index/repo_map.response.json"),
    ),
    (
        "code_index",
        "branch_overlay",
        SchemaKind::Request,
        include_str!("../schemas/code_index/branch_overlay.request.json"),
    ),
    (
        "code_index",
        "branch_overlay",
        SchemaKind::Response,
        include_str!("../schemas/code_index/branch_overlay.response.json"),
    ),
    (
        "code_index",
        "freshness",
        SchemaKind::Request,
        include_str!("../schemas/code_index/freshness.request.json"),
    ),
    (
        "code_index",
        "freshness",
        SchemaKind::Response,
        include_str!("../schemas/code_index/freshness.response.json"),
    ),
    (
        "code_index",
        "rename_symbol",
        SchemaKind::Request,
        include_str!("../schemas/code_index/rename_symbol.request.json"),
    ),
    (
        "code_index",
        "rename_symbol",
        SchemaKind::Response,
        include_str!("../schemas/code_index/rename_symbol.response.json"),
    ),
    // scanner/
    (
        "scanner",
        "scan_project",
        SchemaKind::Request,
        include_str!("../schemas/scanner/scan_project.request.json"),
    ),
    (
        "scanner",
        "scan_project",
        SchemaKind::Response,
        include_str!("../schemas/scanner/scan_project.response.json"),
    ),
    (
        "scanner",
        "scan_incremental",
        SchemaKind::Request,
        include_str!("../schemas/scanner/scan_incremental.request.json"),
    ),
    (
        "scanner",
        "scan_incremental",
        SchemaKind::Response,
        include_str!("../schemas/scanner/scan_incremental.response.json"),
    ),
    // fs/
    (
        "fs",
        "set_mode",
        SchemaKind::Request,
        include_str!("../schemas/fs/set_mode.request.json"),
    ),
    (
        "fs",
        "set_mode",
        SchemaKind::Response,
        include_str!("../schemas/fs/set_mode.response.json"),
    ),
    (
        "fs",
        "staged_status",
        SchemaKind::Request,
        include_str!("../schemas/fs/staged_status.request.json"),
    ),
    (
        "fs",
        "staged_status",
        SchemaKind::Response,
        include_str!("../schemas/fs/staged_status.response.json"),
    ),
    (
        "fs",
        "commit_staged",
        SchemaKind::Request,
        include_str!("../schemas/fs/commit_staged.request.json"),
    ),
    (
        "fs",
        "commit_staged",
        SchemaKind::Response,
        include_str!("../schemas/fs/commit_staged.response.json"),
    ),
    (
        "fs",
        "discard_staged",
        SchemaKind::Request,
        include_str!("../schemas/fs/discard_staged.request.json"),
    ),
    (
        "fs",
        "discard_staged",
        SchemaKind::Response,
        include_str!("../schemas/fs/discard_staged.response.json"),
    ),
    (
        "fs",
        "safe_text_patch",
        SchemaKind::Request,
        include_str!("../schemas/fs/safe_text_patch.request.json"),
    ),
    (
        "fs",
        "safe_text_patch",
        SchemaKind::Response,
        include_str!("../schemas/fs/safe_text_patch.response.json"),
    ),
    (
        "fs",
        "read_text",
        SchemaKind::Request,
        include_str!("../schemas/fs/read_text.request.json"),
    ),
    (
        "fs",
        "read_text",
        SchemaKind::Response,
        include_str!("../schemas/fs/read_text.response.json"),
    ),
    (
        "fs",
        "emit_safe_text_patch_result",
        SchemaKind::Request,
        include_str!("../schemas/fs/emit_safe_text_patch_result.request.json"),
    ),
    (
        "fs",
        "emit_safe_text_patch_result",
        SchemaKind::Response,
        include_str!("../schemas/fs/emit_safe_text_patch_result.response.json"),
    ),
    (
        "fs",
        "snapshot",
        SchemaKind::Request,
        include_str!("../schemas/fs/snapshot.request.json"),
    ),
    (
        "fs",
        "snapshot",
        SchemaKind::Response,
        include_str!("../schemas/fs/snapshot.response.json"),
    ),
    (
        "fs",
        "restore",
        SchemaKind::Request,
        include_str!("../schemas/fs/restore.request.json"),
    ),
    (
        "fs",
        "restore",
        SchemaKind::Response,
        include_str!("../schemas/fs/restore.response.json"),
    ),
    (
        "fs",
        "list_snapshots",
        SchemaKind::Request,
        include_str!("../schemas/fs/list_snapshots.request.json"),
    ),
    (
        "fs",
        "list_snapshots",
        SchemaKind::Response,
        include_str!("../schemas/fs/list_snapshots.response.json"),
    ),
    (
        "fs",
        "drop_snapshot",
        SchemaKind::Request,
        include_str!("../schemas/fs/drop_snapshot.request.json"),
    ),
    (
        "fs",
        "drop_snapshot",
        SchemaKind::Response,
        include_str!("../schemas/fs/drop_snapshot.response.json"),
    ),
    // fs_watch/
    (
        "fs_watch",
        "subscribe",
        SchemaKind::Request,
        include_str!("../schemas/fs_watch/subscribe.request.json"),
    ),
    (
        "fs_watch",
        "subscribe",
        SchemaKind::Response,
        include_str!("../schemas/fs_watch/subscribe.response.json"),
    ),
    (
        "fs_watch",
        "unsubscribe",
        SchemaKind::Request,
        include_str!("../schemas/fs_watch/unsubscribe.request.json"),
    ),
    (
        "fs_watch",
        "unsubscribe",
        SchemaKind::Response,
        include_str!("../schemas/fs_watch/unsubscribe.response.json"),
    ),
    // tools/
    (
        "tools",
        "search",
        SchemaKind::Request,
        include_str!("../schemas/tools/search.request.json"),
    ),
    (
        "tools",
        "search",
        SchemaKind::Response,
        include_str!("../schemas/tools/search.response.json"),
    ),
    (
        "tools",
        "read_file",
        SchemaKind::Request,
        include_str!("../schemas/tools/read_file.request.json"),
    ),
    (
        "tools",
        "read_file",
        SchemaKind::Response,
        include_str!("../schemas/tools/read_file.response.json"),
    ),
    (
        "tools",
        "write_file",
        SchemaKind::Request,
        include_str!("../schemas/tools/write_file.request.json"),
    ),
    (
        "tools",
        "write_file",
        SchemaKind::Response,
        include_str!("../schemas/tools/write_file.response.json"),
    ),
    (
        "tools",
        "delete_file",
        SchemaKind::Request,
        include_str!("../schemas/tools/delete_file.request.json"),
    ),
    (
        "tools",
        "delete_file",
        SchemaKind::Response,
        include_str!("../schemas/tools/delete_file.response.json"),
    ),
    (
        "tools",
        "list_directory",
        SchemaKind::Request,
        include_str!("../schemas/tools/list_directory.request.json"),
    ),
    (
        "tools",
        "list_directory",
        SchemaKind::Response,
        include_str!("../schemas/tools/list_directory.response.json"),
    ),
    (
        "tools",
        "get_file_outline",
        SchemaKind::Request,
        include_str!("../schemas/tools/get_file_outline.request.json"),
    ),
    (
        "tools",
        "get_file_outline",
        SchemaKind::Response,
        include_str!("../schemas/tools/get_file_outline.response.json"),
    ),
    (
        "tools",
        "git",
        SchemaKind::Request,
        include_str!("../schemas/tools/git.request.json"),
    ),
    (
        "tools",
        "git",
        SchemaKind::Response,
        include_str!("../schemas/tools/git.response.json"),
    ),
    (
        "tools",
        "run_command",
        SchemaKind::Request,
        include_str!("../schemas/tools/run_command.request.json"),
    ),
    (
        "tools",
        "run_command",
        SchemaKind::Response,
        include_str!("../schemas/tools/run_command.response.json"),
    ),
    (
        "tools",
        "read_command_output",
        SchemaKind::Request,
        include_str!("../schemas/tools/read_command_output.request.json"),
    ),
    (
        "tools",
        "read_command_output",
        SchemaKind::Response,
        include_str!("../schemas/tools/read_command_output.response.json"),
    ),
    (
        "tools",
        "wait_command",
        SchemaKind::Request,
        include_str!("../schemas/tools/wait_command.request.json"),
    ),
    (
        "tools",
        "wait_command",
        SchemaKind::Response,
        include_str!("../schemas/tools/wait_command.response.json"),
    ),
    (
        "tools",
        "run_test",
        SchemaKind::Request,
        include_str!("../schemas/tools/run_test.request.json"),
    ),
    (
        "tools",
        "run_test",
        SchemaKind::Response,
        include_str!("../schemas/tools/run_test.response.json"),
    ),
    (
        "tools",
        "run_build_command",
        SchemaKind::Request,
        include_str!("../schemas/tools/run_build_command.request.json"),
    ),
    (
        "tools",
        "run_build_command",
        SchemaKind::Response,
        include_str!("../schemas/tools/run_build_command.response.json"),
    ),
    (
        "tools",
        "inspect_test_results",
        SchemaKind::Request,
        include_str!("../schemas/tools/inspect_test_results.request.json"),
    ),
    (
        "tools",
        "inspect_test_results",
        SchemaKind::Response,
        include_str!("../schemas/tools/inspect_test_results.response.json"),
    ),
    (
        "tools",
        "manage_packages",
        SchemaKind::Request,
        include_str!("../schemas/tools/manage_packages.request.json"),
    ),
    (
        "tools",
        "manage_packages",
        SchemaKind::Response,
        include_str!("../schemas/tools/manage_packages.response.json"),
    ),
    (
        "tools",
        "cancel_handle",
        SchemaKind::Request,
        include_str!("../schemas/tools/cancel_handle.request.json"),
    ),
    (
        "tools",
        "cancel_handle",
        SchemaKind::Response,
        include_str!("../schemas/tools/cancel_handle.response.json"),
    ),
    (
        "tools",
        "toolchain_facts",
        SchemaKind::Request,
        include_str!("../schemas/tools/toolchain_facts.request.json"),
    ),
    (
        "tools",
        "toolchain_facts",
        SchemaKind::Response,
        include_str!("../schemas/tools/toolchain_facts.response.json"),
    ),
    (
        "tools",
        "enable",
        SchemaKind::Request,
        include_str!("../schemas/tools/enable.request.json"),
    ),
    (
        "tools",
        "enable",
        SchemaKind::Response,
        include_str!("../schemas/tools/enable.response.json"),
    ),
    // secret_store/
    (
        "secret_store",
        "get",
        SchemaKind::Request,
        include_str!("../schemas/secret_store/get.request.json"),
    ),
    (
        "secret_store",
        "get",
        SchemaKind::Response,
        include_str!("../schemas/secret_store/get.response.json"),
    ),
    (
        "secret_store",
        "set",
        SchemaKind::Request,
        include_str!("../schemas/secret_store/set.request.json"),
    ),
    (
        "secret_store",
        "set",
        SchemaKind::Response,
        include_str!("../schemas/secret_store/set.response.json"),
    ),
    (
        "secret_store",
        "delete",
        SchemaKind::Request,
        include_str!("../schemas/secret_store/delete.request.json"),
    ),
    (
        "secret_store",
        "delete",
        SchemaKind::Response,
        include_str!("../schemas/secret_store/delete.response.json"),
    ),
    (
        "secret_store",
        "list",
        SchemaKind::Request,
        include_str!("../schemas/secret_store/list.request.json"),
    ),
    (
        "secret_store",
        "list",
        SchemaKind::Response,
        include_str!("../schemas/secret_store/list.response.json"),
    ),
    (
        "embed",
        "similarity",
        SchemaKind::Request,
        include_str!("../schemas/embed/similarity.request.json"),
    ),
    (
        "embed",
        "similarity",
        SchemaKind::Response,
        include_str!("../schemas/embed/similarity.response.json"),
    ),
    (
        "embed",
        "top_k",
        SchemaKind::Request,
        include_str!("../schemas/embed/top_k.request.json"),
    ),
    (
        "embed",
        "top_k",
        SchemaKind::Response,
        include_str!("../schemas/embed/top_k.response.json"),
    ),
    (
        "embed",
        "vector",
        SchemaKind::Request,
        include_str!("../schemas/embed/vector.request.json"),
    ),
    (
        "embed",
        "vector",
        SchemaKind::Response,
        include_str!("../schemas/embed/vector.response.json"),
    ),
    (
        "embed",
        "info",
        SchemaKind::Request,
        include_str!("../schemas/embed/info.request.json"),
    ),
    (
        "embed",
        "info",
        SchemaKind::Response,
        include_str!("../schemas/embed/info.response.json"),
    ),
    // Extension hostlibs registered through HostlibRegistry.
    (
        "rules",
        "search",
        SchemaKind::Request,
        include_str!("../schemas/rules/search.request.json"),
    ),
    (
        "rules",
        "search",
        SchemaKind::Response,
        include_str!("../schemas/rules/search.response.json"),
    ),
    (
        "rules",
        "report",
        SchemaKind::Request,
        include_str!("../schemas/rules/report.request.json"),
    ),
    (
        "rules",
        "report",
        SchemaKind::Response,
        include_str!("../schemas/rules/report.response.json"),
    ),
    (
        "rules",
        "diagnostics",
        SchemaKind::Request,
        include_str!("../schemas/rules/diagnostics.request.json"),
    ),
    (
        "rules",
        "diagnostics",
        SchemaKind::Response,
        include_str!("../schemas/rules/diagnostics.response.json"),
    ),
    (
        "rules",
        "apply",
        SchemaKind::Request,
        include_str!("../schemas/rules/apply.request.json"),
    ),
    (
        "rules",
        "apply",
        SchemaKind::Response,
        include_str!("../schemas/rules/apply.response.json"),
    ),
    (
        "rules",
        "fold",
        SchemaKind::Request,
        include_str!("../schemas/rules/fold.request.json"),
    ),
    (
        "rules",
        "fold",
        SchemaKind::Response,
        include_str!("../schemas/rules/fold.response.json"),
    ),
    (
        "lint",
        "run",
        SchemaKind::Request,
        include_str!("../schemas/lint/run.request.json"),
    ),
    (
        "lint",
        "run",
        SchemaKind::Response,
        include_str!("../schemas/lint/run.response.json"),
    ),
];

/// Look up a single schema as raw JSON text.
pub fn lookup(module: &str, method: &str, kind: SchemaKind) -> Option<&'static str> {
    SCHEMAS
        .iter()
        .find(|(m, mt, k, _)| *m == module && *mt == method && *k == kind)
        .map(|(_, _, _, body)| *body)
}

struct CompiledSchema {
    module: &'static str,
    method: &'static str,
    kind: SchemaKind,
    body: Result<VmValue, String>,
}

fn compiled_schemas() -> &'static [CompiledSchema] {
    static COMPILED: OnceLock<Vec<CompiledSchema>> = OnceLock::new();
    COMPILED.get_or_init(|| {
        SCHEMAS
            .iter()
            .filter(|(_, _, kind, _)| *kind == SchemaKind::Request)
            .map(|(module, method, kind, body)| {
                let body = serde_json::from_str::<serde_json::Value>(body)
                    .map_err(|err| format!("schema is not valid JSON: {err}"))
                    .map(|json| harn_vm::json_to_vm_value(&json))
                    .and_then(|schema| harn_vm::schema::canonicalize_json_schema(&schema));
                CompiledSchema {
                    module,
                    method,
                    kind: *kind,
                    body,
                }
            })
            .collect()
    })
}

fn compiled_schema(
    module: &str,
    method: &str,
    kind: SchemaKind,
) -> Option<&'static CompiledSchema> {
    compiled_schemas()
        .iter()
        .find(|schema| schema.module == module && schema.method == method && schema.kind == kind)
}

pub(crate) fn validate_request_args(
    builtin: &'static str,
    module: &'static str,
    method: &'static str,
    args: &[VmValue],
) -> Result<VmValue, HostlibError> {
    let request = normalize_request_arg(builtin, module, method, args)?;
    let schema = compiled_schema(module, method, SchemaKind::Request).ok_or_else(|| {
        HostlibError::Backend {
            builtin,
            message: format!("missing request schema for {module}.{method}"),
        }
    })?;
    let schema = schema
        .body
        .as_ref()
        .map_err(|message| HostlibError::Backend {
            builtin,
            message: format!("invalid request schema for {module}.{method}: {message}"),
        })?;
    harn_vm::schema::validate_value_against_canonical_schema(&request, schema, true).map_err(
        |message| HostlibError::InvalidParameter {
            builtin,
            param: "request",
            message,
        },
    )
}

fn normalize_request_arg(
    builtin: &'static str,
    module: &'static str,
    method: &'static str,
    args: &[VmValue],
) -> Result<VmValue, HostlibError> {
    if args.len() > 1 {
        return Err(HostlibError::InvalidParameter {
            builtin,
            param: "request",
            message: format!("expected exactly one request argument, got {}", args.len()),
        });
    }

    let first = args.first().ok_or(HostlibError::MissingParameter {
        builtin,
        param: "request",
    })?;
    match first {
        VmValue::Dict(map) => Ok(prune_top_level_nil_dict_fields(map)),
        VmValue::String(feature) if (module, method) == ("tools", "enable") => {
            let mut normalized = harn_vm::value::DictMap::new();
            normalized.put_str("feature", feature.to_string());
            Ok(VmValue::dict(normalized))
        }
        other => Err(HostlibError::InvalidParameter {
            builtin,
            param: "request",
            message: format!("expected a dict request body, got {}", other.type_name()),
        }),
    }
}

fn prune_top_level_nil_dict_fields(map: &harn_vm::value::DictMap) -> VmValue {
    let mut pruned = harn_vm::value::DictMap::new();
    for (key, child) in map.iter() {
        if matches!(child, VmValue::Nil) {
            continue;
        }
        pruned.insert(key.clone(), child.clone());
    }
    VmValue::dict_map(pruned)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use harn_vm::VmValue;

    use super::*;

    #[test]
    fn request_validation_prunes_nil_optional_fields() {
        let request = VmValue::dict([
            ("session_id", VmValue::string("session-1")),
            ("scope_id", VmValue::string("scope-1")),
            ("root", VmValue::Nil),
        ]);

        let validated = validate_request_args("hostlib_fs_snapshot", "fs", "snapshot", &[request])
            .expect("nil optional root should be omitted before schema validation");
        let fields = validated.as_dict().expect("validated request is a dict");
        assert!(fields.get("root").is_none());
        assert_eq!(
            fields.get("session_id").map(VmValue::display),
            Some("session-1".to_string())
        );
    }

    #[test]
    fn run_command_request_schema_accepts_env_remove() {
        // Regression pin: `env_remove` is part of the run_command request
        // surface (used by embedders to strip inherited observability vars
        // like HARN_EVENT_LOG_DIR from child processes). The v0.10.1 schema
        // rollout briefly rejected it as an unknown key, which made every
        // downstream shelled command spawn-fail.
        let request = VmValue::dict([
            (
                "argv",
                VmValue::List(Arc::new(vec![VmValue::string("env")])),
            ),
            (
                "env_remove",
                VmValue::List(Arc::new(vec![VmValue::string("HARN_EVENT_LOG_DIR")])),
            ),
        ]);

        validate_request_args(
            "hostlib_tools_run_command",
            "tools",
            "run_command",
            &[request],
        )
        .expect("env_remove must be a valid run_command request field");
    }

    #[test]
    fn request_validation_does_not_prune_nested_nil_fields() {
        let request = VmValue::dict([
            (
                "argv",
                VmValue::List(Arc::new(vec![VmValue::string("env")])),
            ),
            ("env", VmValue::dict([("FOO", VmValue::Nil)])),
        ]);

        let err = validate_request_args(
            "hostlib_tools_run_command",
            "tools",
            "run_command",
            &[request],
        )
        .expect_err("nested nil map values must remain visible to schema validation");
        match err {
            HostlibError::InvalidParameter { message, .. } => {
                assert!(
                    message.contains("env") && message.contains("string"),
                    "nested env nil should fail as a non-string value, got: {message}"
                );
            }
            other => panic!("expected request validation error, got {other:?}"),
        }
    }

    #[test]
    fn dry_run_request_schema_allows_handler_rejected_plan_ops() {
        let unknown = VmValue::dict([
            ("op", VmValue::string("blow_up_the_world")),
            ("path", VmValue::string("multi.txt")),
        ]);
        let missing_op = VmValue::dict_map(harn_vm::value::DictMap::new());
        let non_string_op = VmValue::dict([("op", VmValue::Int(1))]);
        let request = VmValue::dict([(
            "plan",
            VmValue::List(Arc::new(vec![unknown, missing_op, non_string_op])),
        )]);

        validate_request_args("hostlib_ast_dry_run", "ast", "dry_run", &[request]).expect(
            "dry_run unknown/missing/non-string ops must reach the handler for structured rejection",
        );
    }

    #[test]
    fn extension_hostlib_rules_search_has_request_schema() {
        let request = VmValue::dict([
            (
                "rule",
                VmValue::string(
                    "id = \"noop\"\nlanguage = \"typescript\"\n[rule]\npattern = \"$X\"",
                ),
            ),
            ("source", VmValue::string("foo();")),
            ("language", VmValue::string("typescript")),
        ]);

        validate_request_args("hostlib_rules_search", "rules", "search", &[request])
            .expect("rules.search should be covered by the shared hostlib schema catalog");
    }
}
