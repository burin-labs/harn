//! Shared structural-record [`Ty::Shape`] aliases reused across signature
//! groups.
//!
//! Before this module existed, every agent/llm/io builtin that took or
//! returned a dict declared its slot as plain [`TY_DICT`], throwing away the
//! type checker's ability to flag typos in well-known option keys. Each
//! [`Ty::Shape`] in this file captures the *publicly documented* shape of one
//! option-bag or return contract. Adding new options to a shape only requires
//! editing this file — the call-site signatures still reference the named
//! constant.
//!
//! Conventions:
//! - Fields appear in roughly the order documented in `docs/llm/harn-quickref.md`.
//! - Only the genuinely-required keys are marked non-optional. The type
//!   checker still accepts a generic `dict` (see
//!   `crates/harn-parser/src/typechecker/inference/subtyping.rs:315`), so this
//!   is purely additive: dict-literal callsites get checked, dynamic-dict
//!   callsites still work.
//! - Field types reuse the convenience aliases from `types.rs`.
//!
//! See [`super::schema::SCHEMA_RECOVER_ENVELOPE`] for the first example of
//! [`Ty::Shape`] used in a return position — these aliases extend the same
//! pattern to inputs and to other return contracts.

use super::{
    ShapeFieldDescriptor, Ty, TY_ANY, TY_BOOL, TY_DICT, TY_DICT_OR_NIL, TY_FLOAT, TY_INT, TY_LIST,
    TY_NIL, TY_STRING, TY_STRING_OR_NIL,
};

// ---------------------------------------------------------------------------
// Agent option bags
// ---------------------------------------------------------------------------

/// Configuration accepted by `spawn_agent` and `agent.spawn`.
///
/// `task` is the only required field; everything else has stdlib-side defaults.
/// `graph` xor `node` is required at runtime — the type checker doesn't enforce
/// that constraint (would require a sum-of-shapes), but the runtime returns a
/// clear error.
///
/// **Not yet applied** to the parser signature: the runtime call-site type
/// check rejects internally-passed dicts whose `tools` field is a
/// tool_registry-dict (vs the documented `list`). Tracked for adoption once
/// `agent_loop` / `agent_turn` / `sub_agent_run` converge on this shape.
#[allow(dead_code)]
pub(crate) const AGENT_SPAWN_CONFIG: Ty = Ty::Shape(&[
    ShapeFieldDescriptor::new("task", TY_STRING),
    ShapeFieldDescriptor::optional("name", TY_STRING),
    ShapeFieldDescriptor::optional("wait", TY_BOOL),
    ShapeFieldDescriptor::optional("graph", TY_ANY),
    ShapeFieldDescriptor::optional("node", TY_ANY),
    ShapeFieldDescriptor::optional("artifacts", TY_LIST),
    ShapeFieldDescriptor::optional("transcript", TY_ANY),
    ShapeFieldDescriptor::optional("permissions", TY_ANY),
    ShapeFieldDescriptor::optional("options", TY_DICT),
    ShapeFieldDescriptor::optional("execution", TY_DICT),
    ShapeFieldDescriptor::optional("audit", TY_DICT),
    ShapeFieldDescriptor::optional("artifact_mode", TY_STRING),
    ShapeFieldDescriptor::optional("transcript_mode", TY_ANY),
    ShapeFieldDescriptor::optional("context_policy", TY_ANY),
    ShapeFieldDescriptor::optional("resume_workflow", TY_BOOL),
    ShapeFieldDescriptor::optional("persist_state", TY_BOOL),
    ShapeFieldDescriptor::optional("retriggerable", TY_BOOL),
    ShapeFieldDescriptor::optional("policy", TY_ANY),
]);

/// Options dict accepted by `sub_agent_run` / `sub_agent_request` as their
/// second positional argument. `task` is provided separately as the first
/// positional arg, so this shape has *all* fields optional.
///
/// Field set mirrors `parse_sub_agent_request` in
/// `crates/harn-vm/src/stdlib/agents_sub_agent.rs`.
///
/// **Not yet applied** to the parser signature for the same reason as
/// [`AGENT_SPAWN_CONFIG`].
#[allow(dead_code)]
pub(crate) const SUB_AGENT_OPTIONS: Ty = Ty::Shape(&[
    ShapeFieldDescriptor::optional("_type", TY_STRING),
    ShapeFieldDescriptor::optional("name", TY_STRING),
    ShapeFieldDescriptor::optional("system", TY_STRING),
    ShapeFieldDescriptor::optional("session_id", TY_STRING),
    ShapeFieldDescriptor::optional("background", TY_BOOL),
    ShapeFieldDescriptor::optional("allowed_tools", TY_LIST),
    ShapeFieldDescriptor::optional("policy", TY_ANY),
    ShapeFieldDescriptor::optional("returns_schema", TY_ANY),
    ShapeFieldDescriptor::optional("returns", TY_DICT),
    ShapeFieldDescriptor::optional("execution", TY_DICT),
    ShapeFieldDescriptor::optional("model", TY_STRING),
    ShapeFieldDescriptor::optional("provider", TY_STRING),
    ShapeFieldDescriptor::optional("max_tokens", TY_INT),
    ShapeFieldDescriptor::optional("temperature", TY_FLOAT),
    ShapeFieldDescriptor::optional("tools", TY_LIST),
    ShapeFieldDescriptor::optional("artifact_mode", TY_STRING),
    ShapeFieldDescriptor::optional("transcript_mode", TY_ANY),
]);

/// Configuration accepted by `daemon_spawn`.
///
/// Required: `task` and `persist_path` (the former-`prompt`/`state_dir`
/// aliases are dropped in Phase D — see CHANGELOG). Everything else is
/// optional and forwarded to the agent runtime as `options`.
pub(crate) const DAEMON_CONFIG: Ty = Ty::Shape(&[
    ShapeFieldDescriptor::new("task", TY_STRING),
    ShapeFieldDescriptor::new("persist_path", TY_STRING),
    ShapeFieldDescriptor::optional("name", TY_STRING),
    ShapeFieldDescriptor::optional("session_id", TY_STRING),
    ShapeFieldDescriptor::optional("system", TY_STRING),
    ShapeFieldDescriptor::optional("event_queue_capacity", TY_INT),
    ShapeFieldDescriptor::optional("model", TY_STRING),
    ShapeFieldDescriptor::optional("provider", TY_STRING),
    ShapeFieldDescriptor::optional("tools", TY_LIST),
    ShapeFieldDescriptor::optional("options", TY_DICT),
]);

/// `agent_session_seed_from_jsonl` `opts?` argument.
pub(crate) const AGENT_SESSION_SEED_OPTS: Ty = Ty::Shape(&[
    ShapeFieldDescriptor::optional("id", TY_STRING),
    ShapeFieldDescriptor::optional("model", TY_STRING),
    ShapeFieldDescriptor::optional("provider", TY_STRING),
    ShapeFieldDescriptor::optional("system", TY_STRING),
    ShapeFieldDescriptor::optional("tool_format", TY_STRING),
]);

/// `agent_session_compact` `opts?` argument.
pub(crate) const AGENT_SESSION_COMPACT_OPTS: Ty = Ty::Shape(&[
    ShapeFieldDescriptor::optional("keep_last", TY_INT),
    ShapeFieldDescriptor::optional("system", TY_STRING),
    ShapeFieldDescriptor::optional("model", TY_STRING),
    ShapeFieldDescriptor::optional("provider", TY_STRING),
    ShapeFieldDescriptor::optional("max_tokens", TY_INT),
    ShapeFieldDescriptor::optional("temperature", TY_FLOAT),
]);

// ---------------------------------------------------------------------------
// LLM option bags
// ---------------------------------------------------------------------------

/// Options dict for `llm_call`, `llm_call_safe`, `llm_call_structured`,
/// `llm_stream_call`, and friends. Field set lifted from
/// `docs/llm/harn-quickref.md` and the canonical `llm_call` options table.
///
/// **Not yet applied** to the parser signatures: `llm_call*` is called from
/// `agent_loop` / `agent_turn` with dicts that include broader runtime
/// types (e.g. `tools` as a tool_registry-dict, plus loop-control fields like
/// `loop_until_done` / `max_iterations`). Adoption is gated on the internal
/// callers converging on this shape.
#[allow(dead_code)]
pub(crate) const LLM_CALL_OPTIONS: Ty = Ty::Shape(&[
    ShapeFieldDescriptor::optional("model", TY_STRING),
    ShapeFieldDescriptor::optional("provider", TY_STRING),
    ShapeFieldDescriptor::optional("max_tokens", TY_INT),
    ShapeFieldDescriptor::optional("temperature", TY_FLOAT),
    ShapeFieldDescriptor::optional("top_p", TY_FLOAT),
    ShapeFieldDescriptor::optional("stop", Ty::Union(&[TY_STRING, TY_LIST])),
    ShapeFieldDescriptor::optional("system", TY_STRING),
    ShapeFieldDescriptor::optional("tools", TY_LIST),
    ShapeFieldDescriptor::optional("tool_choice", Ty::Union(&[TY_STRING, TY_DICT])),
    ShapeFieldDescriptor::optional("schema", TY_ANY),
    ShapeFieldDescriptor::optional("schema_retries", TY_INT),
    ShapeFieldDescriptor::optional("schema_recover", TY_BOOL),
    ShapeFieldDescriptor::optional("cache", Ty::Union(&[TY_BOOL, TY_DICT])),
    ShapeFieldDescriptor::optional("transcript", TY_ANY),
    ShapeFieldDescriptor::optional("budget", Ty::Union(&[TY_FLOAT, TY_INT, TY_DICT])),
    ShapeFieldDescriptor::optional("mock", TY_ANY),
    ShapeFieldDescriptor::optional("messages", TY_LIST),
    ShapeFieldDescriptor::optional("session_id", TY_STRING),
    ShapeFieldDescriptor::optional("response_format", Ty::Union(&[TY_STRING, TY_DICT])),
    ShapeFieldDescriptor::optional("metadata", TY_DICT),
    ShapeFieldDescriptor::optional("seed", TY_INT),
]);

// ---------------------------------------------------------------------------
// IO / TUI option bags (recently-added surface — type early before it ossifies)
// ---------------------------------------------------------------------------

/// Options dict for `std/io::read_line`.
pub(crate) const READ_LINE_OPTIONS: Ty = Ty::Shape(&[
    ShapeFieldDescriptor::optional("prompt", TY_STRING),
    ShapeFieldDescriptor::optional("timeout_ms", Ty::Union(&[TY_INT, Ty::Named("duration")])),
    ShapeFieldDescriptor::optional("trim", TY_BOOL),
    ShapeFieldDescriptor::optional("echo", TY_BOOL),
    ShapeFieldDescriptor::optional("raw", TY_BOOL),
]);

/// Options dict for `std/tui::select_from`. Implemented in pure Harn at
/// `crates/harn-stdlib/src/stdlib/stdlib_tui.harn`, so this shape is not yet
/// wired to a `BuiltinSignature`; it documents the contract and is ready for
/// adoption if the picker ever moves to a Rust builtin.
#[allow(dead_code)]
pub(crate) const SELECT_FROM_OPTIONS: Ty = Ty::Shape(&[
    ShapeFieldDescriptor::optional("prompt", TY_STRING),
    ShapeFieldDescriptor::optional("default_index", TY_INT),
    ShapeFieldDescriptor::optional("multi", TY_BOOL),
    ShapeFieldDescriptor::optional("cancel_value", TY_ANY),
    ShapeFieldDescriptor::optional("prefer_external", TY_STRING),
    ShapeFieldDescriptor::optional("display", TY_ANY),
    ShapeFieldDescriptor::optional("preview", TY_ANY),
]);

/// Options dict for `__tui_page` (the runtime backing `std/tui::page`).
/// Mirrors `parse_page_options` in `crates/harn-vm/src/stdlib/tui.rs`.
pub(crate) const PAGER_OPTIONS: Ty = Ty::Shape(&[
    ShapeFieldDescriptor::new("body", TY_STRING),
    ShapeFieldDescriptor::optional("title", TY_STRING),
    ShapeFieldDescriptor::optional("footer", TY_STRING),
    ShapeFieldDescriptor::optional("format", TY_STRING),
    ShapeFieldDescriptor::optional("no_pager", TY_BOOL),
]);

/// Options dict for `signal_install` / cooperative signal handler primitives.
pub(crate) const SIGNAL_HANDLER_OPTIONS: Ty = Ty::Shape(&[
    ShapeFieldDescriptor::optional("once", TY_BOOL),
    ShapeFieldDescriptor::optional("restore", TY_BOOL),
]);

// ---------------------------------------------------------------------------
// Return contracts
// ---------------------------------------------------------------------------

/// Return type of `spawn_agent`, `wait_agent` (scalar form), `worker_*` lookups
/// and `sub_agent_run` background mode. Mirrors `clone_worker_state` in
/// `crates/harn-vm/src/stdlib/agents_workers/mod.rs`.
pub(crate) const WORKER_SUMMARY: Ty = Ty::Shape(&[
    ShapeFieldDescriptor::new("_type", TY_STRING),
    ShapeFieldDescriptor::new("id", TY_STRING),
    ShapeFieldDescriptor::new("name", TY_STRING),
    ShapeFieldDescriptor::new("task", TY_STRING),
    ShapeFieldDescriptor::new("mode", TY_STRING),
    ShapeFieldDescriptor::new("status", TY_STRING),
    ShapeFieldDescriptor::new("created_at", TY_STRING),
    ShapeFieldDescriptor::new("started_at", TY_STRING),
    ShapeFieldDescriptor::optional("finished_at", TY_STRING_OR_NIL),
    ShapeFieldDescriptor::optional("awaiting_started_at", TY_STRING_OR_NIL),
    ShapeFieldDescriptor::new("history", TY_LIST),
    ShapeFieldDescriptor::optional("request", TY_DICT_OR_NIL),
    ShapeFieldDescriptor::new("provenance", TY_DICT),
    ShapeFieldDescriptor::optional("result", TY_ANY),
    ShapeFieldDescriptor::optional("error", TY_STRING_OR_NIL),
    ShapeFieldDescriptor::new("artifact_count", TY_INT),
    ShapeFieldDescriptor::new("has_transcript", TY_BOOL),
    ShapeFieldDescriptor::optional("parent_worker_id", TY_STRING_OR_NIL),
    ShapeFieldDescriptor::optional("parent_stage_id", TY_STRING_OR_NIL),
    ShapeFieldDescriptor::optional("child_run_id", TY_STRING_OR_NIL),
    ShapeFieldDescriptor::optional("child_run_path", TY_STRING_OR_NIL),
    ShapeFieldDescriptor::new("execution", TY_DICT),
    ShapeFieldDescriptor::new("snapshot_path", TY_STRING),
    ShapeFieldDescriptor::new("audit", TY_DICT),
]);

/// Return type of `daemon_spawn`, `daemon_snapshot`, `daemon_resume`,
/// `daemon_stop`, `daemon_trigger`. Mirrors `daemon_summary` in
/// `crates/harn-vm/src/stdlib/agents_daemon.rs`.
pub(crate) const DAEMON_SUMMARY: Ty = Ty::Shape(&[
    ShapeFieldDescriptor::new("id", TY_STRING),
    ShapeFieldDescriptor::new("name", TY_STRING),
    ShapeFieldDescriptor::new("status", TY_STRING),
    ShapeFieldDescriptor::new("session_id", TY_STRING),
    ShapeFieldDescriptor::new("persist_path", TY_STRING),
    ShapeFieldDescriptor::new("snapshot_path", TY_STRING),
    ShapeFieldDescriptor::new("pending_event_count", TY_INT),
    ShapeFieldDescriptor::new("queued_event_count", TY_INT),
    ShapeFieldDescriptor::new("event_queue_capacity", TY_INT),
    ShapeFieldDescriptor::optional("error", TY_STRING_OR_NIL),
    ShapeFieldDescriptor::optional("result", TY_ANY),
    ShapeFieldDescriptor::optional("daemon_state", TY_ANY),
    ShapeFieldDescriptor::optional("saved_at", TY_STRING_OR_NIL),
    ShapeFieldDescriptor::optional("inflight_event", TY_DICT_OR_NIL),
]);

/// Result returned by `agent_session_ancestry`.
pub(crate) const SESSION_ANCESTRY: Ty = Ty::Shape(&[
    ShapeFieldDescriptor::optional("parent_id", TY_STRING_OR_NIL),
    ShapeFieldDescriptor::new("child_ids", TY_LIST),
]);

/// Standard `{ ok, status, ...payload }` result envelope returned by I/O
/// builtins such as `std/io::read_line`. Distinct from
/// [`super::schema::SCHEMA_RECOVER_ENVELOPE`] because the IO envelope uses
/// `status` instead of an `error_category`.
pub(crate) const IO_RESULT_ENVELOPE: Ty = Ty::Shape(&[
    ShapeFieldDescriptor::new("ok", TY_BOOL),
    ShapeFieldDescriptor::new("status", TY_STRING),
    ShapeFieldDescriptor::optional("value", TY_STRING),
    ShapeFieldDescriptor::optional("error", TY_STRING),
]);

/// `_` keeps the `TY_NIL` import live for shape callers that want it without
/// noise from the prelude. Remove once a real consumer references it.
const _: Ty = TY_NIL;
