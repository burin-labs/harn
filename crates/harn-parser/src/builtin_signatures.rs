//! Centralized registry of builtin function signatures for static analysis.
//!
//! The parser type checker needs to know two things about every runtime
//! builtin:
//!
//! 1. whether a bare identifier is a known builtin (used for call-arity
//!    bypass, typo suggestions, and unresolved-identifier diagnostics), and
//! 2. what its statically-known return type is, so expressions like
//!    `let x: string = snake_to_camel(y)` infer correctly.
//!
//! Historically these lived as two parallel hand-maintained match arms in
//! `typechecker.rs`, which drifted every time a new builtin was added to
//! the VM. This module is the single source of truth: the two old matches
//! now delegate to a single alphabetical slice of [`BuiltinSig`] entries.
//!
//! Adding a new builtin is one-line: insert the entry in alphabetical order
//! into [`BUILTIN_SIGNATURES`]. The `builtin_signatures_sorted` test enforces
//! alphabetical order so binary search stays valid, and the cross-crate
//! `builtin_registry_alignment` test in `harn-vm/tests/` asserts every
//! runtime builtin has a corresponding parser entry.

use crate::ast::TypeExpr;

/// Statically-known return type hint for a builtin. `None` on [`BuiltinSig`]
/// means "this is a recognized builtin, but its return type is dynamic or
/// polymorphic at the parse site" — matches the legacy `_ => None` fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BuiltinReturn {
    /// Simple named type: `"string"`, `"int"`, `"bool"`, `"nil"`, `"list"`,
    /// `"dict"`, `"float"`.
    Named(&'static str),
    /// Union of two or more named types (e.g. `["string", "nil"]` for
    /// `env` / `regex_match`).
    Union(&'static [&'static str]),
    /// The bottom type (never returns normally).
    Never,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltinMetadata {
    pub name: &'static str,
    pub return_types: &'static [&'static str],
}

/// One entry in the builtin registry.
#[derive(Debug, Clone, Copy)]
pub(crate) struct BuiltinSig {
    pub name: &'static str,
    pub return_type: Option<BuiltinReturn>,
}

const UNION_STRING_NIL: &[&str] = &["string", "nil"];
const EMPTY_RETURN_TYPES: &[&str] = &[];
const RETURN_BOOL: &[&str] = &["bool"];
const RETURN_DICT: &[&str] = &["dict"];
const RETURN_FLOAT: &[&str] = &["float"];
const RETURN_INT: &[&str] = &["int"];
const RETURN_LIST: &[&str] = &["list"];
const RETURN_NEVER: &[&str] = &["never"];
const RETURN_NIL: &[&str] = &["nil"];
const RETURN_STRING: &[&str] = &["string"];

/// Every builtin known to the parser. MUST stay alphabetically sorted by
/// `name` — `builtin_signatures_sorted` enforces this at test time and the
/// binary-search lookup relies on it.
///
/// When adding a new builtin:
/// 1. Register it in the VM stdlib (`crates/harn-vm/src/stdlib/*`).
/// 2. Add the entry here in the correct alphabetical position.
/// 3. The cross-crate `builtin_registry_alignment` test in
///    `crates/harn-vm/tests/` will fail the build if you forget step 2.
pub(crate) const BUILTIN_SIGNATURES: &[BuiltinSig] = &[
    BuiltinSig {
        name: "abs",
        return_type: None,
    },
    BuiltinSig {
        name: "acos",
        return_type: Some(BuiltinReturn::Named("float")),
    },
    BuiltinSig {
        name: "add_assistant",
        return_type: None,
    },
    BuiltinSig {
        name: "add_message",
        return_type: None,
    },
    BuiltinSig {
        name: "add_system",
        return_type: None,
    },
    BuiltinSig {
        name: "add_tool_result",
        return_type: None,
    },
    BuiltinSig {
        name: "add_user",
        return_type: None,
    },
    BuiltinSig {
        name: "addr_of",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "agent",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "agent_config",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "agent_loop",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "agent_name",
        return_type: None,
    },
    BuiltinSig {
        name: "agent_trace",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "agent_trace_summary",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "append_file",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "arch",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "artifact",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "artifact_apply_intent",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "artifact_command_result",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "artifact_context",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "artifact_derive",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "artifact_diff",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "artifact_diff_review",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "artifact_editor_selection",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "artifact_git_diff",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "artifact_patch_proposal",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "artifact_review_decision",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "artifact_select",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "artifact_test_result",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "artifact_verification_bundle",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "artifact_verification_result",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "artifact_workspace_file",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "artifact_workspace_snapshot",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "asin",
        return_type: Some(BuiltinReturn::Named("float")),
    },
    BuiltinSig {
        name: "assert",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "assert_eq",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "assert_ne",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "asset_root",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "atan",
        return_type: Some(BuiltinReturn::Named("float")),
    },
    BuiltinSig {
        name: "atan2",
        return_type: Some(BuiltinReturn::Named("float")),
    },
    BuiltinSig {
        name: "atomic",
        return_type: None,
    },
    BuiltinSig {
        name: "atomic_add",
        return_type: None,
    },
    BuiltinSig {
        name: "atomic_cas",
        return_type: Some(BuiltinReturn::Named("bool")),
    },
    BuiltinSig {
        name: "atomic_get",
        return_type: None,
    },
    BuiltinSig {
        name: "atomic_set",
        return_type: None,
    },
    BuiltinSig {
        name: "await",
        return_type: None,
    },
    BuiltinSig {
        name: "base64_decode",
        return_type: None,
    },
    BuiltinSig {
        name: "base64_encode",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "basename",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "bold",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "camel_to_kebab",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "camel_to_pascal",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "camel_to_snake",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "cancel",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "cancel_graceful",
        return_type: None,
    },
    BuiltinSig {
        name: "ceil",
        return_type: None,
    },
    BuiltinSig {
        name: "channel",
        return_type: None,
    },
    BuiltinSig {
        name: "checkpoint",
        return_type: None,
    },
    BuiltinSig {
        name: "checkpoint_clear",
        return_type: None,
    },
    BuiltinSig {
        name: "checkpoint_delete",
        return_type: None,
    },
    BuiltinSig {
        name: "checkpoint_exists",
        return_type: None,
    },
    BuiltinSig {
        name: "checkpoint_get",
        return_type: None,
    },
    BuiltinSig {
        name: "checkpoint_list",
        return_type: None,
    },
    BuiltinSig {
        name: "circuit_breaker",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "circuit_check",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "circuit_record_failure",
        return_type: Some(BuiltinReturn::Named("bool")),
    },
    BuiltinSig {
        name: "circuit_record_success",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "circuit_reset",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "clear_tool_hooks",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "close_agent",
        return_type: None,
    },
    BuiltinSig {
        name: "close_channel",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "color",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "compute_content_hash",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "contains",
        return_type: None,
    },
    BuiltinSig {
        name: "conversation",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "copy_file",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "cos",
        return_type: Some(BuiltinReturn::Named("float")),
    },
    BuiltinSig {
        name: "cwd",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "date_format",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "date_iso",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "date_now",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "date_parse",
        return_type: Some(BuiltinReturn::Named("float")),
    },
    BuiltinSig {
        name: "delete_file",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "dim",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "dirname",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "e",
        return_type: Some(BuiltinReturn::Named("float")),
    },
    BuiltinSig {
        name: "elapsed",
        return_type: Some(BuiltinReturn::Named("int")),
    },
    BuiltinSig {
        name: "enable_tracing",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "ends_with",
        return_type: None,
    },
    BuiltinSig {
        name: "entries",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "env",
        return_type: Some(BuiltinReturn::Union(UNION_STRING_NIL)),
    },
    BuiltinSig {
        name: "error_category",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "estimate_tokens",
        return_type: Some(BuiltinReturn::Named("int")),
    },
    BuiltinSig {
        name: "eval_metric",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "eval_metrics",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "eval_suite_manifest",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "eval_suite_run",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "exec",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "exec_at",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "execution_root",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "exit",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "exp",
        return_type: Some(BuiltinReturn::Named("float")),
    },
    BuiltinSig {
        name: "extname",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "file_exists",
        return_type: Some(BuiltinReturn::Named("bool")),
    },
    BuiltinSig {
        name: "floor",
        return_type: None,
    },
    BuiltinSig {
        name: "format",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "hash_value",
        return_type: Some(BuiltinReturn::Named("int")),
    },
    BuiltinSig {
        name: "home_dir",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "host_call",
        return_type: None,
    },
    BuiltinSig {
        name: "host_capabilities",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "host_has",
        return_type: Some(BuiltinReturn::Named("bool")),
    },
    BuiltinSig {
        name: "host_mock",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "host_mock_calls",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "host_mock_clear",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "hostname",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "http_delete",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "http_get",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "http_mock",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "http_mock_calls",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "http_mock_clear",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "http_patch",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "http_post",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "http_put",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "http_request",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "invalidate_facts",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "is_cancelled",
        return_type: None,
    },
    BuiltinSig {
        name: "is_err",
        return_type: Some(BuiltinReturn::Named("bool")),
    },
    BuiltinSig {
        name: "is_infinite",
        return_type: Some(BuiltinReturn::Named("bool")),
    },
    BuiltinSig {
        name: "is_nan",
        return_type: Some(BuiltinReturn::Named("bool")),
    },
    BuiltinSig {
        name: "is_ok",
        return_type: Some(BuiltinReturn::Named("bool")),
    },
    BuiltinSig {
        name: "is_rate_limited",
        return_type: Some(BuiltinReturn::Named("bool")),
    },
    BuiltinSig {
        name: "is_same",
        return_type: Some(BuiltinReturn::Named("bool")),
    },
    BuiltinSig {
        name: "is_timeout",
        return_type: Some(BuiltinReturn::Named("bool")),
    },
    BuiltinSig {
        name: "is_type",
        return_type: Some(BuiltinReturn::Named("bool")),
    },
    BuiltinSig {
        name: "join",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "json_extract",
        return_type: None,
    },
    BuiltinSig {
        name: "json_parse",
        return_type: None,
    },
    BuiltinSig {
        name: "json_stringify",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "json_validate",
        return_type: Some(BuiltinReturn::Named("bool")),
    },
    BuiltinSig {
        name: "kebab_to_camel",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "kebab_to_snake",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "keys",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "len",
        return_type: None,
    },
    BuiltinSig {
        name: "list_agents",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "list_dir",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "llm_budget",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "llm_budget_remaining",
        return_type: None,
    },
    BuiltinSig {
        name: "llm_call",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "llm_completion",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "llm_config",
        return_type: None,
    },
    BuiltinSig {
        name: "llm_cost",
        return_type: Some(BuiltinReturn::Named("float")),
    },
    BuiltinSig {
        name: "llm_healthcheck",
        return_type: None,
    },
    BuiltinSig {
        name: "llm_infer_provider",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "llm_info",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "llm_mock",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "llm_mock_calls",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "llm_mock_clear",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "llm_model_tier",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "llm_pick_model",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "llm_providers",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "llm_rate_limit",
        return_type: None,
    },
    BuiltinSig {
        name: "llm_resolve_model",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "llm_session_cost",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "llm_stream",
        return_type: None,
    },
    BuiltinSig {
        name: "llm_usage",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "ln",
        return_type: Some(BuiltinReturn::Named("float")),
    },
    BuiltinSig {
        name: "load_run_tree",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "log",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "log10",
        return_type: Some(BuiltinReturn::Named("float")),
    },
    BuiltinSig {
        name: "log2",
        return_type: Some(BuiltinReturn::Named("float")),
    },
    BuiltinSig {
        name: "log_debug",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "log_error",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "log_info",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "log_json",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "log_set_level",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "log_warn",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "lowercase",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "lowercase_first",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "max",
        return_type: None,
    },
    BuiltinSig {
        name: "mcp_call",
        return_type: None,
    },
    BuiltinSig {
        name: "mcp_connect",
        return_type: None,
    },
    BuiltinSig {
        name: "mcp_disconnect",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "mcp_get_prompt",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "mcp_list_prompts",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "mcp_list_resource_templates",
        return_type: None,
    },
    BuiltinSig {
        name: "mcp_list_resources",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "mcp_list_tools",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "mcp_prompt",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "mcp_read_resource",
        return_type: None,
    },
    BuiltinSig {
        name: "mcp_resource",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "mcp_resource_template",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "mcp_serve",
        return_type: None,
    },
    BuiltinSig {
        name: "mcp_server_info",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "mcp_tools",
        return_type: None,
    },
    BuiltinSig {
        name: "md5",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "metadata_entries",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "metadata_get",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "metadata_refresh_hashes",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "metadata_resolve",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "metadata_save",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "metadata_set",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "metadata_stale",
        return_type: None,
    },
    BuiltinSig {
        name: "metadata_status",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "microcompact",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "min",
        return_type: None,
    },
    BuiltinSig {
        name: "mkdir",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "pascal_to_camel",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "pascal_to_snake",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "path_basename",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "path_extension",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "path_is_absolute",
        return_type: Some(BuiltinReturn::Named("bool")),
    },
    BuiltinSig {
        name: "path_is_relative",
        return_type: Some(BuiltinReturn::Named("bool")),
    },
    BuiltinSig {
        name: "path_join",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "path_normalize",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "path_parent",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "path_parts",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "path_relative_to",
        return_type: None,
    },
    BuiltinSig {
        name: "path_segments",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "path_stem",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "path_to_native",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "path_to_posix",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "path_with_extension",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "path_with_stem",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "pi",
        return_type: Some(BuiltinReturn::Named("float")),
    },
    BuiltinSig {
        name: "pid",
        return_type: Some(BuiltinReturn::Named("int")),
    },
    BuiltinSig {
        name: "platform",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "pow",
        return_type: None,
    },
    BuiltinSig {
        name: "print",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "println",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "progress",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "project_root",
        return_type: Some(BuiltinReturn::Union(UNION_STRING_NIL)),
    },
    BuiltinSig {
        name: "prompt_user",
        return_type: Some(BuiltinReturn::Union(UNION_STRING_NIL)),
    },
    BuiltinSig {
        name: "provider_register",
        return_type: Some(BuiltinReturn::Named("bool")),
    },
    BuiltinSig {
        name: "random",
        return_type: Some(BuiltinReturn::Named("float")),
    },
    BuiltinSig {
        name: "random_int",
        return_type: None,
    },
    BuiltinSig {
        name: "read_file",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "receive",
        return_type: None,
    },
    BuiltinSig {
        name: "regex_captures",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "regex_match",
        return_type: Some(BuiltinReturn::Union(UNION_STRING_NIL)),
    },
    BuiltinSig {
        name: "regex_replace",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "register_tool_hook",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "render",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "render_prompt",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "replace",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "resume_agent",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "round",
        return_type: None,
    },
    BuiltinSig {
        name: "run_record",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "run_record_diff",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "run_record_eval",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "run_record_eval_suite",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "run_record_fixture",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "run_record_load",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "run_record_save",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "runtime_paths",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "scan_directory",
        return_type: None,
    },
    BuiltinSig {
        name: "schema_check",
        return_type: None,
    },
    BuiltinSig {
        name: "schema_expect",
        return_type: None,
    },
    BuiltinSig {
        name: "schema_extend",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "schema_from_json_schema",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "schema_from_openapi_schema",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "schema_is",
        return_type: Some(BuiltinReturn::Named("bool")),
    },
    BuiltinSig {
        name: "schema_omit",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "schema_parse",
        return_type: None,
    },
    BuiltinSig {
        name: "schema_partial",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "schema_pick",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "schema_to_json_schema",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "schema_to_openapi_schema",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "select",
        return_type: None,
    },
    BuiltinSig {
        name: "select_artifacts_adaptive",
        return_type: None,
    },
    BuiltinSig {
        name: "send",
        return_type: None,
    },
    BuiltinSig {
        name: "send_input",
        return_type: None,
    },
    BuiltinSig {
        name: "set",
        return_type: None,
    },
    BuiltinSig {
        name: "set_add",
        return_type: None,
    },
    BuiltinSig {
        name: "set_contains",
        return_type: Some(BuiltinReturn::Named("bool")),
    },
    BuiltinSig {
        name: "set_difference",
        return_type: None,
    },
    BuiltinSig {
        name: "set_intersect",
        return_type: None,
    },
    BuiltinSig {
        name: "set_is_disjoint",
        return_type: Some(BuiltinReturn::Named("bool")),
    },
    BuiltinSig {
        name: "set_is_subset",
        return_type: Some(BuiltinReturn::Named("bool")),
    },
    BuiltinSig {
        name: "set_is_superset",
        return_type: Some(BuiltinReturn::Named("bool")),
    },
    BuiltinSig {
        name: "set_remove",
        return_type: None,
    },
    BuiltinSig {
        name: "set_symmetric_difference",
        return_type: None,
    },
    BuiltinSig {
        name: "set_union",
        return_type: None,
    },
    BuiltinSig {
        name: "sha224",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "sha256",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "sha384",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "sha512",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "sha512_256",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "shell",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "shell_at",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "sign",
        return_type: Some(BuiltinReturn::Named("int")),
    },
    BuiltinSig {
        name: "sin",
        return_type: Some(BuiltinReturn::Named("float")),
    },
    BuiltinSig {
        name: "sleep",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "snake_to_camel",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "snake_to_kebab",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "snake_to_pascal",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "source_dir",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "spawn",
        return_type: None,
    },
    BuiltinSig {
        name: "spawn_agent",
        return_type: None,
    },
    BuiltinSig {
        name: "split",
        return_type: None,
    },
    BuiltinSig {
        name: "sqrt",
        return_type: None,
    },
    BuiltinSig {
        name: "starts_with",
        return_type: None,
    },
    BuiltinSig {
        name: "stat",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "store_clear",
        return_type: None,
    },
    BuiltinSig {
        name: "store_delete",
        return_type: None,
    },
    BuiltinSig {
        name: "store_get",
        return_type: None,
    },
    BuiltinSig {
        name: "store_list",
        return_type: None,
    },
    BuiltinSig {
        name: "store_save",
        return_type: None,
    },
    BuiltinSig {
        name: "store_set",
        return_type: None,
    },
    BuiltinSig {
        name: "substring",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "tan",
        return_type: Some(BuiltinReturn::Named("float")),
    },
    BuiltinSig {
        name: "temp_dir",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "throw_error",
        return_type: None,
    },
    BuiltinSig {
        name: "timer_end",
        return_type: Some(BuiltinReturn::Named("int")),
    },
    BuiltinSig {
        name: "timer_start",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "timestamp",
        return_type: Some(BuiltinReturn::Named("float")),
    },
    BuiltinSig {
        name: "title_case",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "to_float",
        return_type: Some(BuiltinReturn::Named("float")),
    },
    BuiltinSig {
        name: "to_int",
        return_type: Some(BuiltinReturn::Named("int")),
    },
    BuiltinSig {
        name: "to_list",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "to_string",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "toml_parse",
        return_type: None,
    },
    BuiltinSig {
        name: "toml_stringify",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "tool_count",
        return_type: Some(BuiltinReturn::Named("int")),
    },
    BuiltinSig {
        name: "tool_define",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "tool_describe",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "tool_find",
        return_type: None,
    },
    BuiltinSig {
        name: "tool_format_result",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "tool_list",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "tool_parse_call",
        return_type: None,
    },
    BuiltinSig {
        name: "tool_prompt",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "tool_registry",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "tool_remove",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "tool_schema",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "tool_select",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "trace_end",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "trace_id",
        return_type: Some(BuiltinReturn::Union(UNION_STRING_NIL)),
    },
    BuiltinSig {
        name: "trace_spans",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "trace_start",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "trace_summary",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "transcript",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "transcript_abandon",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "transcript_add_asset",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "transcript_archive",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "transcript_assets",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "transcript_auto_compact",
        return_type: None,
    },
    BuiltinSig {
        name: "transcript_compact",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "transcript_events",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "transcript_events_by_kind",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "transcript_export",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "transcript_fork",
        return_type: None,
    },
    BuiltinSig {
        name: "transcript_from_messages",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "transcript_id",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "transcript_import",
        return_type: None,
    },
    BuiltinSig {
        name: "transcript_messages",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "transcript_render_full",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "transcript_render_visible",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "transcript_reset",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "transcript_resume",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "transcript_stats",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "transcript_summarize",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "transcript_summary",
        return_type: Some(BuiltinReturn::Union(UNION_STRING_NIL)),
    },
    BuiltinSig {
        name: "trim",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "try_receive",
        return_type: None,
    },
    BuiltinSig {
        name: "type_of",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "unreachable",
        return_type: Some(BuiltinReturn::Never),
    },
    BuiltinSig {
        name: "unwrap",
        return_type: None,
    },
    BuiltinSig {
        name: "unwrap_err",
        return_type: None,
    },
    BuiltinSig {
        name: "unwrap_or",
        return_type: None,
    },
    BuiltinSig {
        name: "uppercase",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "uppercase_first",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "url_decode",
        return_type: None,
    },
    BuiltinSig {
        name: "url_encode",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "username",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "uuid",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "values",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "wait_agent",
        return_type: None,
    },
    BuiltinSig {
        name: "workflow_clone",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "workflow_commit",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "workflow_diff",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "workflow_execute",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "workflow_graph",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "workflow_insert_node",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "workflow_inspect",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "workflow_policy_report",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "workflow_replace_node",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "workflow_rewire",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "workflow_set_context_policy",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "workflow_set_model_policy",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "workflow_set_transcript_policy",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "workflow_validate",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "write_file",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "yaml_parse",
        return_type: None,
    },
    BuiltinSig {
        name: "yaml_stringify",
        return_type: Some(BuiltinReturn::Named("string")),
    },
];

/// Binary-search the registry for a given name.
fn lookup(name: &str) -> Option<&'static BuiltinSig> {
    BUILTIN_SIGNATURES
        .binary_search_by_key(&name, |sig| sig.name)
        .ok()
        .map(|idx| &BUILTIN_SIGNATURES[idx])
}

/// Is `name` a builtin known to the parser?
pub(crate) fn is_builtin(name: &str) -> bool {
    lookup(name).is_some()
}

/// Iterator over every builtin name known to the parser, in alphabetical
/// order. Exposed via [`crate::known_builtin_names`] for cross-crate drift
/// testing and future completion surfaces.
pub(crate) fn iter_builtin_names() -> impl Iterator<Item = &'static str> {
    BUILTIN_SIGNATURES.iter().map(|sig| sig.name)
}

pub(crate) fn iter_builtin_metadata() -> impl Iterator<Item = BuiltinMetadata> {
    BUILTIN_SIGNATURES.iter().map(|sig| BuiltinMetadata {
        name: sig.name,
        return_types: match sig.return_type {
            Some(BuiltinReturn::Named(name)) => match name {
                "bool" => RETURN_BOOL,
                "dict" => RETURN_DICT,
                "float" => RETURN_FLOAT,
                "int" => RETURN_INT,
                "list" => RETURN_LIST,
                "nil" => RETURN_NIL,
                "string" => RETURN_STRING,
                _ => EMPTY_RETURN_TYPES,
            },
            Some(BuiltinReturn::Union(names)) => names,
            Some(BuiltinReturn::Never) => RETURN_NEVER,
            None => EMPTY_RETURN_TYPES,
        },
    })
}

/// Statically-known return type for `name`, if any. Returns `None` when
/// the name is unknown OR when it is a builtin with a dynamic return type
/// (e.g. `json_parse`).
pub(crate) fn builtin_return_type(name: &str) -> Option<TypeExpr> {
    let sig = lookup(name)?;
    match sig.return_type? {
        BuiltinReturn::Named(ty) => Some(TypeExpr::Named(ty.into())),
        BuiltinReturn::Union(tys) => Some(TypeExpr::Union(
            tys.iter().map(|ty| TypeExpr::Named((*ty).into())).collect(),
        )),
        BuiltinReturn::Never => Some(TypeExpr::Never),
    }
}

/// Returns true if this builtin produces an untyped/opaque value that should
/// be validated before field access in strict types mode.
pub fn is_untyped_boundary_source(name: &str) -> bool {
    matches!(
        name,
        "json_parse"
            | "json_extract"
            | "yaml_parse"
            | "toml_parse"
            | "llm_call"
            | "llm_completion"
            | "http_get"
            | "http_post"
            | "http_put"
            | "http_patch"
            | "http_delete"
            | "http_request"
            | "host_call"
            | "mcp_call"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_signatures_sorted() {
        let mut prev = "";
        for sig in BUILTIN_SIGNATURES {
            assert!(
                sig.name > prev,
                "BUILTIN_SIGNATURES not sorted: `{prev}` must come before `{}`",
                sig.name
            );
            prev = sig.name;
        }
    }

    #[test]
    fn lookup_hits_and_misses() {
        assert!(is_builtin("snake_to_camel"));
        assert!(is_builtin("log"));
        assert!(is_builtin("await"));
        assert!(!is_builtin("definitely_not_a_builtin"));
        assert!(!is_builtin(""));
    }

    #[test]
    fn return_type_named_variant() {
        assert_eq!(
            builtin_return_type("snake_to_camel"),
            Some(TypeExpr::Named("string".into()))
        );
        assert_eq!(
            builtin_return_type("log"),
            Some(TypeExpr::Named("nil".into()))
        );
        assert_eq!(
            builtin_return_type("pi"),
            Some(TypeExpr::Named("float".into()))
        );
        assert_eq!(
            builtin_return_type("sign"),
            Some(TypeExpr::Named("int".into()))
        );
        assert_eq!(
            builtin_return_type("file_exists"),
            Some(TypeExpr::Named("bool".into()))
        );
    }

    #[test]
    fn return_type_union_variant() {
        assert_eq!(
            builtin_return_type("env"),
            Some(TypeExpr::Union(vec![
                TypeExpr::Named("string".into()),
                TypeExpr::Named("nil".into()),
            ]))
        );
        assert_eq!(
            builtin_return_type("transcript_summary"),
            Some(TypeExpr::Union(vec![
                TypeExpr::Named("string".into()),
                TypeExpr::Named("nil".into()),
            ]))
        );
    }

    #[test]
    fn return_type_unknown_for_dynamic_builtins() {
        assert!(is_builtin("json_parse"));
        assert_eq!(builtin_return_type("json_parse"), None);
        assert!(is_builtin("schema_parse"));
        assert_eq!(builtin_return_type("schema_parse"), None);
    }

    #[test]
    fn return_type_none_for_unknown_names() {
        assert_eq!(builtin_return_type("not_a_real_thing"), None);
    }
}
