//! Host primitives backing the Harn-driven agent loop in
//! `std/agent/loop.harn`.
//!
//! These are CRUD-shaped primitives over per-session host state. The
//! decision logic (iterate, sentinel-check, dispatch tools, judge,
//! continue/break) lives in Harn; Rust is reduced to data plumbing,
//! provider/tool capability surfaces, and resource lifecycle.

use crate::value::VmDictExt;
use std::{cell::RefCell, collections::BTreeMap, sync::Arc};

use crate::agent_events::AgentEvent;
use crate::stdlib::macros::{harn_builtin, register_builtin_defs, VmBuiltinDef};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

use super::agent_terminal_class::{
    agent_loop_made_no_llm_call, agent_terminal_class, session_status_indicates_error,
};
use super::tools::build_assistant_response_message;
use super::{emit_live_agent_event_sync as emit_event, permissions};

const HOST_SESSION_FINALIZE: &str = "__host_agent_session_finalize";
const HOST_SESSION_RECORD_ASSISTANT: &str = "__host_agent_session_record_assistant";
const HOST_SESSION_RECORD_TOOL_RESULTS: &str = "__host_agent_session_record_tool_results";
const HOST_SESSION_RECORD_USAGE: &str = "__host_agent_session_record_usage";
const HOST_SESSION_DRAIN_FEEDBACK: &str = "__host_agent_session_drain_feedback";
const HOST_SESSION_DRAIN_COMMAND_UPDATES: &str = "__host_agent_session_drain_command_updates";
const HOST_SESSION_AWAIT_INBOX: &str = "__host_agent_session_await_inbox";
const HOST_SESSION_DRAIN_HOST_INJECTIONS: &str = "__host_agent_session_drain_host_injections";
const HOST_SESSION_DRAIN_BRIDGE_INJECTIONS: &str = "__host_agent_session_drain_bridge_injections";
const HOST_SESSION_PUSH_BRIDGE_INJECTION: &str = "__host_agent_session_push_bridge_injection";
const HOST_SESSION_PUSH_USER_MESSAGE: &str = "__host_agent_session_push_user_message";
const HOST_SESSION_PENDING_INJECTIONS: &str = "__host_agent_session_pending_injections";
const HOST_SESSION_REVOKE_REMINDER: &str = "__host_agent_session_revoke_reminder";
const HOST_SESSION_INJECT_REMINDER: &str = "__host_agent_session_inject_reminder";
const HOST_SESSION_TOTALS: &str = "__host_agent_session_totals";
const HOST_SESSION_POST_EVENT: &str = "__host_agent_session_post_event";
const HOST_SESSION_APPLY_REMINDER_POST_TURN: &str = "__host_agent_session_apply_reminder_post_turn";
const HOST_SESSION_SET_ACTIVE_SKILLS: &str = "__host_agent_session_set_active_skills";
const HOST_SESSION_ACTIVE_SKILLS: &str = "__host_agent_session_active_skills";
const HOST_SESSION_REPLACE_MESSAGES: &str = "__host_agent_session_replace_messages";
const HOST_SESSION_PROJECT_TURN: &str = "__host_agent_session_project_turn";
const HOST_SESSION_CLAIM_TOOL_FORMAT: &str = "__host_agent_session_claim_tool_format";
const HOST_DAEMON_SNAPSHOT: &str = "__host_agent_daemon_snapshot";
const HOST_DAEMON_WAIT: &str = "__host_agent_daemon_wait";
const HOST_AGENT_RECORD_NATIVE_TOOL_FALLBACK: &str = "__host_agent_record_native_tool_fallback";
const HOST_AGENT_RECORD_COMPACTION: &str = "__host_agent_record_compaction";

mod assistant_messages;
pub(crate) mod cancellation;
mod daemon_bridge;
mod inbox;
mod lifecycle;
mod tool_result_messages;
use assistant_messages::durable_anthropic_blocks;
use cancellation::CancelSafeNestedExecutionGuard;
mod live_transcript_journal;
mod message_history;
mod plan_document;
mod run_identity;
mod session_policies;
mod skill_state;
mod turn_projection;
mod usage_accounting;
use session_policies::{
    build_nested_budget_denial, install_session_nested_budget, release_session_policies,
};
pub(crate) use session_policies::{install_session_policy_guard, options_request_session_policies};
mod visible_messages;
#[cfg(test)]
pub(crate) use visible_messages::visible_messages_with_lineage;

#[cfg(test)]
use inbox::{
    host_agent_session_drain_command_updates_builtin, host_agent_session_drain_feedback_builtin,
    host_agent_session_totals_builtin,
};
#[cfg(test)]
use lifecycle::text_has_tool_call_prefix;
pub(crate) use lifecycle::{
    canonical_acp_stop_reason, canonical_provider_stop_reason, is_length_truncation,
    truncated_tool_call_should_continue,
};
#[cfg(test)]
use message_history::{
    assistant_message_from_llm_result, host_agent_session_record_assistant_builtin,
    pair_orphaned_tool_use,
};
use plan_document::{next_plan_document_events, plan_artifact_from_result};
pub(crate) use run_identity::active_run_id;
use run_identity::{agent_init_control, agent_init_control_done};
use usage_accounting::{resolve_call_accounting, SessionUsageTotals};

#[cfg(test)]
pub(crate) use tool_result_messages::record_tool_results_for_test;
use tool_result_messages::{
    assistant_tool_use_blocks, paired_tool_result_ids, screenshots_from_tool_result,
    synthesize_orphan_tool_results, tool_result_message, ToolResultMessageInput,
};

/// Session-keyed record for Harn-driven agent loops. The Harn loop owns
/// iteration and decision logic; this struct holds only session-scoped
/// scalars (totals, active skills) that primitives need to read/write
/// atomically. Larger per-session state (transcript, subscribers) lives
/// in `crate::agent_sessions`.
struct AgentHostSession {
    session_id: String,
    run_id: String,
    task: String,
    tokens_used: i64,
    /// Sum of calls whose price is known. This remains useful as a lower bound
    /// when another call in the same session is unpriced.
    cost_used: f64,
    unpriced_calls: i64,
    usage_unknown_calls: i64,
    input_tokens: i64,
    output_tokens: i64,
    cache_read_tokens: i64,
    cache_write_tokens: i64,
    active_skills: Vec<String>,
    tool_calls: Vec<serde_json::Value>,
    successful_tools: Vec<String>,
    rejected_tools: Vec<String>,
    tool_mode: String,
    last_provider: Option<String>,
    last_model: Option<String>,
    /// Exact grammar used by the last successful provider transaction. Unlike
    /// the immutable session preference, this follows custom callers, route
    /// switches, and runtime channel degradation turn by turn.
    last_tool_format: Option<String>,
    pushed_transcript_dir: bool,
    started_at: String,
    /// Iteration cap from `agent_loop(options.max_iterations)`. Captured
    /// here so finalize can disambiguate `final_status == "budget_exhausted"`
    /// caused by hitting the cap (→ ACP `max_turn_requests`) from other
    /// budget paths.
    max_iterations: i64,
    daemon_state: Option<String>,
    daemon_snapshot_path: Option<String>,
    resumed_iterations: usize,
    daemon_watch_state: BTreeMap<String, u64>,
    daemon_idle_backoff_ms: u64,
    host_bridge: Option<Arc<crate::bridge::HostBridge>>,
    /// Provider-reported `stop_reason` from the most recent `llm_call`
    /// in this loop. Used by finalize to detect ACP `max_tokens` (when
    /// the last call truncated due to its `max_tokens` parameter) and
    /// `refusal` (Anthropic refusal stop_reason).
    last_llm_stop_reason: Option<String>,
    /// Untrusted-origin file provenance ledger: workspace paths whose content
    /// came from an untrusted step (fetch/clone/MCP, or a write made while
    /// context was tainted). Owned here so it drops with the session. Read on the
    /// tool-result ingest path so a later read of a tainted file is quarantined.
    file_provenance: crate::security::FileProvenanceLedger,
    /// Pops the per-session capability policy off the execution stack
    /// on drop. Declared last so it Drops last in `AgentHostSession`'s
    /// natural field-order drop, after every other cleanup completes.
    nested_policy_guard: Option<CancelSafeNestedExecutionGuard>,
}

/// Tracks which scoped policy stacks were pushed for a guarded tool
/// dispatch so `Drop` can pop them in reverse order. The agent loop
/// honours per-agent ceilings by intersecting outer policies with the
/// requested per-agent ones before pushing, so child sub-agents never
/// widen permissions beyond their parents.
#[derive(Default)]
struct InstalledPolicies {
    pushed_execution: bool,
    pushed_approval: bool,
    pushed_command: bool,
    pushed_permissions: bool,
    pushed_precheck: bool,
}

pub(crate) struct SessionPolicyGuard {
    installed: InstalledPolicies,
}

impl Drop for SessionPolicyGuard {
    fn drop(&mut self) {
        release_session_policies(&self.installed);
    }
}

struct SharedHostSessions(parking_lot::RwLock<BTreeMap<String, AgentHostSession>>);

impl SharedHostSessions {
    fn borrow(&self) -> parking_lot::RwLockReadGuard<'_, BTreeMap<String, AgentHostSession>> {
        self.0.read()
    }

    fn borrow_mut(&self) -> parking_lot::RwLockWriteGuard<'_, BTreeMap<String, AgentHostSession>> {
        self.0.write()
    }

    fn try_borrow(
        &self,
    ) -> Result<parking_lot::RwLockReadGuard<'_, BTreeMap<String, AgentHostSession>>, ()> {
        self.0.try_read().ok_or(())
    }
}

pub(crate) struct AgentHostSessionRuntime {
    sessions: SharedHostSessions,
}

impl Default for AgentHostSessionRuntime {
    fn default() -> Self {
        Self {
            sessions: SharedHostSessions(parking_lot::RwLock::new(BTreeMap::new())),
        }
    }
}

thread_local! {
    static ACTIVE_AGENT_HOST_SESSION_RUNTIME: RefCell<Arc<AgentHostSessionRuntime>> =
        RefCell::new(fresh_agent_host_session_runtime());
}

pub(crate) fn fresh_agent_host_session_runtime() -> Arc<AgentHostSessionRuntime> {
    Arc::new(AgentHostSessionRuntime::default())
}

pub(crate) fn active_agent_host_session_runtime() -> Arc<AgentHostSessionRuntime> {
    ACTIVE_AGENT_HOST_SESSION_RUNTIME.with(|slot| Arc::clone(&slot.borrow()))
}

pub(crate) fn swap_active_agent_host_session_runtime(
    next: Arc<AgentHostSessionRuntime>,
) -> Arc<AgentHostSessionRuntime> {
    ACTIVE_AGENT_HOST_SESSION_RUNTIME.with(|slot| std::mem::replace(&mut *slot.borrow_mut(), next))
}

struct AgentHostSessionsSlot;
static AGENT_HOST_SESSIONS: AgentHostSessionsSlot = AgentHostSessionsSlot;
impl AgentHostSessionsSlot {
    fn with<T>(&self, use_sessions: impl FnOnce(&SharedHostSessions) -> T) -> T {
        let runtime = active_agent_host_session_runtime();
        use_sessions(&runtime.sessions)
    }
}

pub(crate) fn reset_agent_session_host_state() {
    AGENT_HOST_SESSIONS.with(|sessions| sessions.borrow_mut().clear());
}

/// Seed a minimal host session carrying just the `last_provider`/`last_model`
/// facts that `pair_orphaned_tool_use` reads. Test-only: lets a repro exercise
/// the real production entrypoint (which sources provider/model from the host
/// store) without standing up a full `agent_loop`.
#[cfg(test)]
pub(crate) fn seed_host_session_provider_model(session_id: &str, provider: &str, model: &str) {
    let session = AgentHostSession {
        session_id: session_id.to_string(),
        run_id: format!("agent_run_{}", uuid::Uuid::now_v7()),
        task: String::new(),
        tokens_used: 0,
        cost_used: 0.0,
        unpriced_calls: 0,
        usage_unknown_calls: 0,
        input_tokens: 0,
        output_tokens: 0,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        active_skills: Vec::new(),
        tool_calls: Vec::new(),
        successful_tools: Vec::new(),
        rejected_tools: Vec::new(),
        tool_mode: String::new(),
        last_provider: Some(provider.to_string()),
        last_model: Some(model.to_string()),
        last_tool_format: None,
        pushed_transcript_dir: false,
        started_at: now_id(),
        max_iterations: 0,
        daemon_state: None,
        daemon_snapshot_path: None,
        resumed_iterations: 0,
        daemon_watch_state: std::collections::BTreeMap::new(),
        daemon_idle_backoff_ms: 100,
        host_bridge: None,
        last_llm_stop_reason: None,
        file_provenance: crate::security::FileProvenanceLedger::default(),
        nested_policy_guard: None,
    };
    AGENT_HOST_SESSIONS.with(|sessions| {
        sessions
            .borrow_mut()
            .insert(session_id.to_string(), session);
    });
}

fn with_session<R>(
    session_id: &str,
    label: &str,
    f: impl FnOnce(&mut AgentHostSession) -> Result<R, VmError>,
) -> Result<R, VmError> {
    AGENT_HOST_SESSIONS.with(|sessions| {
        let mut sessions = sessions.borrow_mut();
        let session = sessions.get_mut(session_id).ok_or_else(|| {
            VmError::Runtime(format!("{label}: unknown agent session `{session_id}`"))
        })?;
        f(session)
    })
}

/// Append a taint record to the session's lethal-trifecta ledger. No-op when
/// the session is unknown (e.g. tool results recorded outside a host session).
pub(crate) fn push_session_taint(session_id: &str, record: crate::security::TaintRecord) {
    crate::agent_sessions::push_session_taint(session_id, record);
}

/// Snapshot the session's taint ledger for the dispatch gate. Empty when the
/// session is unknown or no untrusted content has entered context.
pub(crate) fn session_taint_snapshot(session_id: &str) -> Vec<crate::security::TaintRecord> {
    crate::agent_sessions::session_taint_snapshot(session_id)
}

/// Record that `path` now holds untrusted-origin content (taint-on-write). No-op
/// for an unknown session.
pub(crate) fn record_file_provenance(session_id: &str, path: &str, origin: &str) {
    let _ = with_session(session_id, "record_file_provenance", |session| {
        session.file_provenance.record(path, origin);
        Ok(())
    });
}

/// Trust classification for a read of `path` against the session's file
/// provenance ledger (distrust-on-read). `None` for a first-party / unknown path
/// or an unknown session.
pub(crate) fn classify_file_read(
    session_id: &str,
    path: &str,
) -> Option<(crate::security::TrustLevel, String)> {
    with_session(session_id, "classify_file_read", |session| {
        Ok(session.file_provenance.classify(path))
    })
    .ok()
    .flatten()
}

fn opts_dict(value: Option<&VmValue>) -> crate::value::DictMap {
    match value {
        Some(VmValue::Dict(d)) => (**d).clone(),
        _ => crate::value::DictMap::new(),
    }
}

fn json_to_vm(value: &serde_json::Value) -> VmValue {
    crate::stdlib::json_to_vm_value(value)
}

pub(crate) fn vm_to_json(value: &VmValue) -> serde_json::Value {
    crate::llm::vm_value_to_json(value)
}

pub(crate) fn list_items(value: &VmValue) -> Vec<VmValue> {
    match value {
        VmValue::List(items) => (**items).clone(),
        _ => Vec::new(),
    }
}

pub(crate) fn dict_get<'a>(value: &'a VmValue, key: &str) -> Option<&'a VmValue> {
    match value {
        VmValue::Dict(d) => d.get(key),
        VmValue::StructInstance(_) => value.struct_field(key),
        _ => None,
    }
}

fn value_as_i64(value: &VmValue) -> Option<i64> {
    match value {
        VmValue::Int(i) => Some(*i),
        VmValue::Float(f) => Some(*f as i64),
        _ => None,
    }
}

fn first_dict_i64(sources: &[&VmValue], keys: &[&str]) -> i64 {
    for source in sources {
        for key in keys {
            if let Some(value) = dict_get(source, key).and_then(value_as_i64) {
                return value;
            }
        }
    }
    0
}

fn first_provider_cache_usage_i64(
    sources: &[&VmValue],
    extractor: fn(&serde_json::Value) -> i64,
) -> i64 {
    for source in sources {
        let tokens = extractor(&vm_to_json(source));
        if tokens != 0 {
            return tokens;
        }
    }
    0
}

fn opt_str(map: &crate::value::DictMap, key: &str) -> Option<String> {
    map.get(key).and_then(|v| match v {
        VmValue::String(s) => Some(s.to_string()),
        _ => None,
    })
}

fn opt_int(map: &crate::value::DictMap, key: &str) -> Option<i64> {
    map.get(key).and_then(|v| match v {
        VmValue::Int(i) => Some(*i),
        VmValue::Float(f) => Some(*f as i64),
        _ => None,
    })
}

fn opt_json(map: &crate::value::DictMap, key: &str) -> Option<serde_json::Value> {
    map.get(key)
        .filter(|value| !matches!(value, VmValue::Nil))
        .map(vm_to_json)
}

fn initial_user_content(
    opts_map: &crate::value::DictMap,
    fallback_message: &str,
) -> serde_json::Value {
    opt_json(opts_map, "initial_user_content")
        .or_else(|| opt_json(opts_map, "initial_message_content"))
        .unwrap_or_else(|| serde_json::Value::String(fallback_message.to_string()))
}

/// Parse the caller-managed `history` option into validated transcript
/// messages. Each entry must be a dict carrying a string `role`; the remaining
/// fields of the canonical `llm_call` message shape (`content`, `tool_calls`,
/// `tool_call_id`, …) pass through untouched. A malformed seed is a hard error
/// so prior context is never silently dropped.
///
/// This is TRANSIENT seeding: the caller owns the history. The returned turns
/// are injected as ordinary transcript turns by `host_agent_session_init`,
/// visible to the model exactly as `llm_call`'s `messages` array would be, and
/// are otherwise indistinguishable from turns the loop produced itself —
/// done_judge, compaction, and projection all treat them normally.
fn seed_history_messages(opts_map: &crate::value::DictMap) -> Result<Vec<VmValue>, VmError> {
    let Some(value) = opts_map.get("history") else {
        return Ok(Vec::new());
    };
    if matches!(value, VmValue::Nil) {
        return Ok(Vec::new());
    }
    let VmValue::List(list) = value else {
        return Err(VmError::Runtime(format!(
            "agent_loop: `history` must be a list of message dicts; got {}",
            value.type_name()
        )));
    };
    let mut out = Vec::with_capacity(list.len());
    for (index, entry) in list.iter().enumerate() {
        let Some(dict) = entry.as_dict() else {
            return Err(VmError::Runtime(format!(
                "agent_loop: `history[{index}]` must be a message dict; got {}",
                entry.type_name()
            )));
        };
        if !matches!(dict.get("role"), Some(VmValue::String(_))) {
            return Err(VmError::Runtime(format!(
                "agent_loop: `history[{index}]` must carry a string `role` \
                 (user|assistant|tool_result|system)"
            )));
        }
        out.push(entry.clone());
    }
    Ok(out)
}

fn now_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

/// Distrust-on-read: classify a read whose target path is a known
/// untrusted-origin file. Only `Read`-kind tools consume file provenance — a
/// write to a tainted path re-taints it (see [`record_write_provenance`]), it
/// does not surface the content. `None` for a non-read, an unparseable
/// arguments payload, or a first-party path.
fn file_read_provenance(
    session_id: &str,
    tool_name: &str,
    result: &VmValue,
) -> Option<(crate::security::TrustLevel, String)> {
    let annotations = crate::orchestration::current_tool_annotations(tool_name);
    if annotations.map(|a| a.kind) != Some(crate::tool_annotations::ToolKind::Read) {
        return None;
    }
    let arguments = dict_get(result, "arguments").map(vm_to_json)?;
    crate::security::path_arguments(&arguments)
        .into_iter()
        .find_map(|path| classify_file_read(session_id, &path))
}

/// Command-argument provenance (distrust-on-launder): an `Execute`-kind tool
/// whose command string names a path already recorded as an untrusted-origin
/// file re-reads that content into context outside a structured `read_file`
/// call (`cat vendor/dep/README`). Classify it untrusted by the same file origin
/// so the laundered payload arms the taint / trifecta gate, closing the
/// `tool_result` residual. `None` for non-Execute tools, commandless calls, and
/// commands that name no tainted path.
fn command_read_provenance(
    session_id: &str,
    tool_name: &str,
    result: &VmValue,
) -> Option<(crate::security::TrustLevel, String)> {
    let annotations = crate::orchestration::current_tool_annotations(tool_name);
    if annotations.map(|a| a.kind) != Some(crate::tool_annotations::ToolKind::Execute) {
        return None;
    }
    let arguments = dict_get(result, "arguments").map(vm_to_json)?;
    let command = crate::security::command_string(&arguments)?;
    with_session(session_id, "command_read_provenance", |session| {
        Ok(session.file_provenance.references_tainted_path(&command))
    })
    .ok()
    .flatten()
}

/// Taint-on-write propagation: when a tool [`crate::security::mutates_workspace`]
/// and either its own result is untrusted (`result_origin`) or context is
/// already tainted (`context_tainted`), record every path it wrote as
/// untrusted-origin. A specific `result_origin` (e.g. `fetch:web_fetch`) is
/// preferred over the generic propagated `tainted-context` so the provenance
/// chain stays legible. No-op when nothing untrusted is in play.
fn record_write_provenance(
    session_id: &str,
    tool_name: &str,
    result: &VmValue,
    result_origin: Option<&str>,
    context_tainted: bool,
) {
    let annotations = crate::orchestration::current_tool_annotations(tool_name);
    if !crate::security::mutates_workspace(annotations.as_ref()) {
        return;
    }
    let origin = match result_origin {
        Some(origin) => origin.to_string(),
        None if context_tainted => {
            crate::security::file_provenance::TAINTED_CONTEXT_ORIGIN.to_string()
        }
        None => return,
    };
    let Some(arguments) = dict_get(result, "arguments").map(vm_to_json) else {
        return;
    };
    for path in crate::security::path_arguments(&arguments) {
        record_file_provenance(session_id, &path, &origin);
    }
}

/// Append per-tool observation messages from a dispatch result.
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "__host_agent_session_record_tool_results(session_id: string, dispatch: list) -> nil",
    category = "agent.host",
    runtime_only = true
)]
fn host_agent_session_record_tool_results_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let session_id = args.first().map(|v| v.display()).unwrap_or_default();
    let dispatch = args.get(1).cloned().unwrap_or(VmValue::Nil);
    let (provider, model, last_tool_format) =
        assistant_messages::last_dispatch_receipt(&session_id);
    // The session `tool_format` lock is pinned to the PRIMARY model's format at
    // session init and is never re-claimed on escalation. But the trailing
    // assistant turn we are answering may have been produced by an escalated
    // NATIVE model whose calls persisted as structured `tool_use`/`tool_call`
    // blocks. A structured native block MUST get its tool-result on the
    // provider's native role (`tool_result`/`tool`) — routing it through the
    // session-locked text channel (`role:"user"`) would leave the native block
    // orphaned and trip the same non-retryable Anthropic 400 the orphan repair
    // guards against. So derive the format from the turn being answered:
    // `"native"` when the assistant turn carries structured tool-call blocks,
    // otherwise the session lock (homogeneous text-channel runs keep their calls
    // inline in `content` and carry no structured block, so they stay on the
    // text echo — unchanged).
    let session_tool_format = crate::agent_sessions::tool_format(&session_id).unwrap_or_default();
    let tool_format = if message_history::trailing_assistant_has_native_tool_use(&session_id) {
        assistant_messages::effective_session_tool_format(&provider, &model, "native")
    } else if let Some(format) = last_tool_format {
        format
    } else {
        assistant_messages::effective_session_tool_format(&provider, &model, &session_tool_format)
    };
    // A host can record a result before a live dispatch receipt exists (for
    // example, replay and host-driven tools). Resolve that absence through the
    // same provider/model catalog that owns live admission. A non-empty unknown
    // value remains an error: only missing metadata receives a default.
    let default_tool_format = tool_format
        .trim()
        .is_empty()
        .then(|| crate::llm_config::default_tool_format(&model, &provider));
    let resolved_tool_format = default_tool_format.as_deref().unwrap_or(&tool_format);
    let tool_channel =
        crate::llm_config::tool_format_channel(resolved_tool_format).ok_or_else(|| {
            VmError::Runtime(format!(
                "unknown effective tool format `{resolved_tool_format}`"
            ))
        })?;
    let results_value = dispatch;
    let security_policy = crate::security::current_policy();
    let mut successful = Vec::new();
    let mut rejected = Vec::new();
    // Running "is untrusted content already in this session's context" signal for
    // taint-on-write propagation. Seeded from prior batches' taint and flipped
    // true as soon as an untrusted ingress is recorded in this batch, so a
    // fetch-then-write within one batch still taints the written file.
    let mut context_tainted = !session_taint_snapshot(&session_id).is_empty();
    for result in list_items(&results_value).iter() {
        let name = dict_get(result, "tool_name")
            .or_else(|| dict_get(result, "name"))
            .map(|v| v.display())
            .unwrap_or_default();
        let raw_observation = dict_get(result, "observation")
            .or_else(|| dict_get(result, "rendered_result"))
            .or_else(|| dict_get(result, "output"))
            .or_else(|| dict_get(result, "content"))
            .map(|v| v.display())
            .unwrap_or_default();
        // Provenance / spotlighting (Layer 0): tag content that crossed a trust
        // boundary (external MCP server, internet fetch) and frame it as data,
        // not instructions, before it reaches the model's context. Skipped
        // entirely when security is disabled.
        let provenance = if security_policy.is_off() {
            None
        } else {
            crate::security::classify_result_trust(
                dict_get(result, "executor"),
                crate::orchestration::current_tool_annotations(&name).as_ref(),
                &name,
                &security_policy,
            )
            // Origin-authenticated cross-agent directives (Phase 3): when a
            // result was not already tagged untrusted by its executor
            // provenance (e.g. a subagent / worker result that is not
            // MCP-executed), authenticate any embedded orchestration directive.
            // A directive-looking span lacking a valid provenance stamp is
            // forged authority planted in untrusted content, so it is tagged
            // untrusted and quarantined via the same taint/trifecta ledger below
            // — never obeyed as authoritative. Default OFF.
            .or_else(|| {
                if security_policy.authenticate_directives {
                    crate::security::classify_directive_trust(&raw_observation)
                } else {
                    None
                }
            })
            // Untrusted-origin file provenance (distrust-on-read): a read whose
            // target path was recorded as an untrusted-origin file (written by a
            // fetch/clone/MCP step, or under tainted context) is quarantined by
            // the same taint/trifecta gate as a live external ingress. Default
            // OFF.
            .or_else(|| {
                if security_policy.taint_file_provenance {
                    file_read_provenance(&session_id, &name, result)
                } else {
                    None
                }
            })
            // Command-argument provenance (distrust-on-launder): an Execute-kind
            // tool whose command string names an untrusted-origin file re-reads
            // that content outside a structured `read_file` call (`cat <path>`),
            // the laundering path that evades lexical file provenance. Quarantined
            // by the same taint/trifecta gate. Default OFF.
            .or_else(|| {
                if security_policy.taint_command_reads {
                    command_read_provenance(&session_id, &name, result)
                } else {
                    None
                }
            })
        };
        let ingress = provenance.as_ref().map(|(trust, origin)| {
            crate::security::sanitize_ingress(&raw_observation, origin, *trust)
        });
        let observation = ingress
            .as_ref()
            .map(|ingress| ingress.delivered.clone())
            .unwrap_or_else(|| raw_observation.clone());
        let tool_call_id = dict_get(result, "tool_call_id")
            .or_else(|| dict_get(result, "tool_use_id"))
            .map(|v| v.display())
            .unwrap_or_default();
        let ok = match dict_get(result, "ok") {
            Some(VmValue::Bool(value)) => *value,
            _ => match dict_get(result, "success") {
                Some(VmValue::Bool(value)) => *value,
                _ => match dict_get(result, "status") {
                    Some(VmValue::String(s)) => s.as_str() == "ok",
                    _ => true,
                },
            },
        };
        if ok {
            successful.push(name.clone());
        } else {
            rejected.push(name.clone());
        }
        // Lethal-trifecta ledger (Layer 1): note that untrusted content entered
        // this session's context so the dispatch gate can require confirmation
        // before an exfiltration-capable tool runs.
        if let Some((trust, origin)) = &provenance {
            if trust.is_untrusted() && !raw_observation.is_empty() {
                push_session_taint(
                    &session_id,
                    crate::security::TaintRecord {
                        origin: origin.clone(),
                        trust: *trust,
                        introduced_by: if tool_call_id.is_empty() {
                            name.clone()
                        } else {
                            tool_call_id.clone()
                        },
                        // Layer 2: score the untrusted content with the active
                        // injection classifier when detection is enabled
                        // (`local-ml` mode, or an explicit opt-in). The neural
                        // backend (if the host installed a loader) is materialized
                        // lazily on this first scored span; otherwise the
                        // dependency-free heuristic runs.
                        detector: ingress.as_ref().and_then(|value| value.detector.clone()),
                        labels: ingress
                            .as_ref()
                            .map(|value| value.labels.clone())
                            .unwrap_or_default(),
                        endpoints: ingress
                            .as_ref()
                            .map(|value| value.endpoints.clone())
                            .unwrap_or_default(),
                    },
                );
                context_tainted = true;
            }
        }
        // Untrusted-origin file provenance (taint-on-write propagation): a file
        // this tool wrote inherits untrusted origin when the writing result is
        // itself untrusted (fetch-to-disk / clone / MCP write) or context is
        // already tainted, so a later read of it is quarantined by the block
        // above. Only successful writes taint a path — a failed write created no
        // file, so tainting it would gate a later legitimate read for nothing.
        // Default OFF.
        if ok && security_policy.taint_file_provenance {
            let result_origin = provenance
                .as_ref()
                .filter(|(trust, _)| trust.is_untrusted())
                .map(|(_, origin)| origin.as_str());
            record_write_provenance(&session_id, &name, result, result_origin, context_tainted);
        }
        if ok && super::plan::is_plan_tool(&name) {
            if let Some(plan_value) = plan_artifact_from_result(result) {
                let created_at = crate::orchestration::now_unix_seconds_text();
                let event_id = crate::orchestration::new_id("plan_event");
                let document_events = next_plan_document_events(
                    &session_id,
                    &name,
                    result,
                    plan_value,
                    created_at,
                    event_id,
                )
                .map_err(|error| VmError::Runtime(error.to_string()))?;
                for document_event in document_events {
                    super::plan::persist_plan_document_event(&session_id, &document_event)
                        .map_err(|error| VmError::Runtime(error.to_string()))?;
                    super::agent_runtime::emit_agent_event_sync(&AgentEvent::PlanDocumentUpdated {
                        session_id: session_id.clone(),
                        event: Box::new(document_event),
                    });
                }
            }
        }
        // Carry computer-use screenshots back as provider image content blocks.
        let screenshots = screenshots_from_tool_result(result);
        let transcript_data = tool_result_messages::transcript_tool_result_data(result);
        let message_index = crate::agent_sessions::inject_message(
            &session_id,
            tool_result_message(ToolResultMessageInput {
                channel: tool_channel,
                name: &name,
                tool_call_id: &tool_call_id,
                observation: &observation,
                ok,
                screenshots: &screenshots,
                data: transcript_data.as_ref(),
            }),
        )
        .map_err(VmError::Runtime)?;
        crate::llm::pairing_receipts::emit_tool_result_receipt(
            &session_id,
            message_index,
            &tool_call_id,
            &name,
            ok,
            &tool_format,
        );
    }
    let _ = with_session(&session_id, HOST_SESSION_RECORD_TOOL_RESULTS, |session| {
        session.successful_tools.extend(successful);
        session.rejected_tools.extend(rejected);
        Ok(())
    });
    Ok(VmValue::Nil)
}

/// Accumulate token + cost usage from an llm_call result, return totals.
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "__host_agent_session_record_usage(session_id: string, llm_result: dict) -> dict",
    category = "agent.host",
    runtime_only = true
)]
fn host_agent_session_record_usage_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let session_id = args.first().map(|v| v.display()).unwrap_or_default();
    let llm_result = args.get(1).cloned().unwrap_or(VmValue::Nil);
    let llm_block = dict_get(&llm_result, "llm")
        .cloned()
        .unwrap_or(VmValue::Nil);
    let usage_block = dict_get(&llm_result, "usage")
        .cloned()
        .unwrap_or(VmValue::Nil);
    // Probe order: agent-loop result block, canonical envelope `usage`, then
    // top-level for legacy recordings that predate the canonical envelope.
    let input_tokens = first_dict_i64(&[&llm_block, &usage_block, &llm_result], &["input_tokens"]);
    let output_tokens =
        first_dict_i64(&[&llm_block, &usage_block, &llm_result], &["output_tokens"]);
    let usage_sources = [&llm_result, &usage_block, &llm_block];
    let cache_read_tokens =
        first_provider_cache_usage_i64(&usage_sources, super::api::extract_cache_read_tokens);
    let cache_write_tokens =
        first_provider_cache_usage_i64(&usage_sources, super::api::extract_cache_write_tokens);
    let provider = dict_get(&llm_result, "provider")
        .map(|v| v.display())
        .unwrap_or_default();
    let model = dict_get(&llm_result, "model")
        .map(|v| v.display())
        .unwrap_or_default();
    // Canonical LlmResult envelopes already own provider-reported or
    // catalog-derived cost and its certainty. Legacy recordings that predate
    // that envelope get the same catalog projection here. Neither path turns
    // an unknown price into a zero-cost observation.
    let accounting = resolve_call_accounting(
        &usage_block,
        &provider,
        &model,
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_write_tokens,
    );
    // Provider-request accounting, read from the same `usage` block that owns
    // every other accounting fact for this call. Absent for a recording made
    // before the field existed, which is why it is folded in conditionally
    // rather than defaulting to a zero that would read as "no retries".
    let provider_attempts = dict_get(&usage_block, "provider_attempts").cloned();
    let stop_reason = match dict_get(&llm_result, "stop_reason") {
        Some(VmValue::String(s)) if !s.is_empty() => Some(s.to_string()),
        _ => None,
    };

    let totals = with_session(&session_id, HOST_SESSION_RECORD_USAGE, |session| {
        session.tokens_used = session
            .tokens_used
            .saturating_add(input_tokens)
            .saturating_add(output_tokens);
        session.input_tokens = session.input_tokens.saturating_add(input_tokens);
        session.output_tokens = session.output_tokens.saturating_add(output_tokens);
        session.cache_read_tokens = session.cache_read_tokens.saturating_add(cache_read_tokens);
        session.cache_write_tokens = session
            .cache_write_tokens
            .saturating_add(cache_write_tokens);
        if let Some(cost) = accounting.cost_usd {
            session.cost_used += cost;
        } else {
            session.unpriced_calls = session.unpriced_calls.saturating_add(1);
        }
        if accounting.usage_unknown {
            session.usage_unknown_calls = session.usage_unknown_calls.saturating_add(1);
        }
        if stop_reason.is_some() {
            session.last_llm_stop_reason = stop_reason.clone();
        }
        Ok(SessionUsageTotals::from(&*session))
    })?;
    let mut llm_call_metadata = serde_json::json!({
        "input_tokens": input_tokens,
        "output_tokens": output_tokens,
        "cache_read_tokens": cache_read_tokens,
        "cache_write_tokens": cache_write_tokens,
        "provider": provider,
        "model": model,
        "cost_usd": accounting.cost_usd,
        "accounting_status": if accounting.usage_unknown { "unknown" } else { "reported" },
        "provider_stop_reason": stop_reason,
        "canonical_stop_reason": canonical_provider_stop_reason(stop_reason.as_deref()),
    });
    if let (Some(object), Some(attempts)) = (
        llm_call_metadata.as_object_mut(),
        provider_attempts.map(|attempts| crate::stdlib::observability::vm_value_to_json(&attempts)),
    ) {
        object.insert("provider_attempts".to_string(), attempts);
    }
    crate::agent_sessions::append_event(
        &session_id,
        crate::llm::helpers::transcript_event(
            "llm_call",
            "assistant",
            "internal",
            "LLM call completed",
            Some(llm_call_metadata),
        ),
    )
    .map_err(VmError::Runtime)?;
    Ok(totals.to_vm(false))
}

const HOST_SESSION_BUILTINS: &[&VmBuiltinDef] = &[
    &HOST_AGENT_SESSION_RECORD_TOOL_RESULTS_BUILTIN_DEF,
    &HOST_AGENT_SESSION_RECORD_USAGE_BUILTIN_DEF,
];

pub fn register_agent_session_host_primitives(vm: &mut Vm) {
    register_builtin_defs(vm, HOST_SESSION_BUILTINS);
    daemon_bridge::register_daemon_bridge_primitives(vm);
    inbox::register_inbox_primitives(vm);
    lifecycle::register_lifecycle_primitives(vm);
    live_transcript_journal::register_live_transcript_journal_primitives(vm);
    message_history::register_message_history_primitives(vm);
    skill_state::register_skill_state_primitives(vm);
    turn_projection::register_turn_projection_primitives(vm);
    visible_messages::register_visible_message_primitives(vm);
}

#[cfg(test)]
#[path = "agent_session_host_tests.rs"]
mod tests;
