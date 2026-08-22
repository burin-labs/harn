//! Inbox delivery, command updates, feedback, and reminder mutation.

use super::*;

/// Deterministic "should the loop auto-continue this truncated turn?" gate.
///
/// Returns `true` only when the provider cut the response off mid-emit
/// (`stop_reason` is a length truncation), the turn resolved zero usable tool
/// calls, and there is a partial tool-call signal (a parser diagnostic or a
/// tool-call opener in the text). Returns `false` on clean stops — including a
/// cleanly-finished-but-malformed call — so it never overlaps the
/// parse-tolerance (#3137) or reasoning-leak (#3142) paths.
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "__host_agent_truncated_tool_call(stop_reason: string|nil, text: string, tool_call_count: int, has_parse_errors: bool) -> bool",
    category = "agent.host",
    runtime_only = true
)]
fn host_agent_truncated_tool_call_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let stop_reason = match args.first() {
        Some(VmValue::String(s)) if !s.is_empty() => Some(s.to_string()),
        _ => None,
    };
    let text = args.get(1).map(|v| v.display()).unwrap_or_default();
    let tool_call_count = match args.get(2) {
        Some(VmValue::Int(i)) => *i,
        _ => 0,
    };
    let has_parse_errors = matches!(args.get(3), Some(VmValue::Bool(true)));
    Ok(VmValue::Bool(truncated_tool_call_should_continue(
        stop_reason.as_deref(),
        &text,
        tool_call_count,
        has_parse_errors,
    )))
}

/// Drain pending runtime-feedback notes for a session (no-op shim).
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "__host_agent_session_drain_feedback(session_id: string) -> list",
    category = "agent.host",
    runtime_only = true
)]
pub(super) fn host_agent_session_drain_feedback_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let session_id = match args.first() {
        Some(VmValue::String(s)) if !s.is_empty() => s.to_string(),
        _ => {
            return Err(VmError::Runtime(format!(
                "{HOST_SESSION_DRAIN_FEEDBACK}: session_id must be a non-empty string"
            )))
        }
    };
    let drained = crate::orchestration::agent_inbox::drain_where(&session_id, |entry| {
        entry.payload.is_none() && entry.kind != "tool_progress" && entry.kind != "tool_result"
    })
    .into_iter()
    .map(|entry| {
        let mut item = crate::value::DictMap::new();
        item.put_str("kind", entry.kind);
        item.put_str("content", entry.content);
        item.put_str("source", entry.source);
        item.insert(
            crate::value::intern_key("sequence"),
            VmValue::Int(entry.sequence as i64),
        );
        item.insert(crate::value::intern_key("ts_ms"), VmValue::Int(entry.ts_ms));
        VmValue::dict(item)
    })
    .collect::<Vec<_>>();
    Ok(VmValue::List(std::sync::Arc::new(drained)))
}

/// Drain the session's queued long-running-command update entries — the
/// `tool_progress` / `tool_result` pushes the hostlib background waiter emits —
/// and leave every other inbox entry in place for the normal feedback path.
///
/// This is the command ledger's dedicated consumer. Kind-filtering here is the
/// digest-build boundary, NOT a wake decision: `wait_async` still wakes on ANY
/// entry (see `host_agent_session_await_inbox`), so a user interrupt or peer
/// message parked alongside a build still breaks the hold — it simply flows
/// through `drain_feedback`, never through this drain. Each returned entry is
/// `{kind, content, sequence, ts_ms}`; `content` is the JSON snapshot string the
/// ledger parses loop-side (offsets, stderr counts, terminal status), keeping
/// snapshot parsing in one place (Harn).
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "__host_agent_session_drain_command_updates(session_id: string) -> list",
    category = "agent.host",
    runtime_only = true
)]
pub(super) fn host_agent_session_drain_command_updates_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let session_id = match args.first() {
        Some(VmValue::String(s)) if !s.is_empty() => s.to_string(),
        _ => {
            return Err(VmError::Runtime(format!(
                "{HOST_SESSION_DRAIN_COMMAND_UPDATES}: session_id must be a non-empty string"
            )))
        }
    };
    let drained = crate::orchestration::agent_inbox::drain_where(&session_id, |entry| {
        entry.kind == "tool_progress" || entry.kind == "tool_result"
    });
    let drained = drained
        .into_iter()
        .map(|entry| {
            let mut item = crate::value::DictMap::new();
            item.put_str("kind", entry.kind);
            item.put_str("content", entry.content);
            item.insert(
                crate::value::intern_key("sequence"),
                VmValue::Int(entry.sequence as i64),
            );
            item.insert(crate::value::intern_key("ts_ms"), VmValue::Int(entry.ts_ms));
            VmValue::dict(item)
        })
        .collect::<Vec<_>>();
    Ok(VmValue::List(std::sync::Arc::new(drained)))
}

/// Park the calling turn until `session_id` has a queued `agent_inbox` entry OR
/// `timeout_ms` elapses on the harness clock. Returns `true` when woken by an
/// entry, `false` on the deadline. This is the command-hold's re-entry
/// primitive: the loop parks here (zero inference) between decision re-entries.
///
/// Determinism: the timeout sleep uses the SAME harness clock the loop reads for
/// its decision deadlines (a `PausedClock` under `Harness::test`), so one
/// `advance()` drives both the deadline math and this park — deterministic
/// replay with no real sleeps. Do NOT drive command-hold tests with `mock_time`:
/// `MockAwareClock::sleep` returns instantly under a mock, which would make this
/// park time out immediately and silently break the hold. Use `Harness::test` /
/// `PausedClock`.
///
/// Wake set is deliberately INCLUSIVE: it wakes on ANY queued entry (progress,
/// terminal, user interrupt, peer message), never a kind-filtered subset. A
/// future refactor must never narrow the wake condition — kind-filtering belongs
/// only where the loop BUILDS the digest, never at the wake decision.
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "__host_agent_session_await_inbox(session_id: string, timeout_ms: int) -> bool",
    kind = "async",
    category = "agent.host",
    runtime_only = true
)]
async fn host_agent_session_await_inbox(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let session_id = match args.first() {
        Some(VmValue::String(s)) if !s.is_empty() => s.to_string(),
        _ => {
            return Err(VmError::Runtime(format!(
                "{HOST_SESSION_AWAIT_INBOX}: session_id must be a non-empty string"
            )))
        }
    };
    let timeout_ms = args.get(1).and_then(value_as_i64).unwrap_or(0).max(0) as u64;
    // Recover the exact harness clock the loop already reads for deadline math
    // (a PausedClock under Harness::test). Falling back to another clock domain
    // can make a fresh handle appear to have crossed its wall-time ceiling.
    // Globals are shared into the child VM, so absence is an embedder contract
    // violation and must fail at this boundary.
    let clock: Arc<dyn harn_clock::Clock> = {
        let vm = ctx.child_vm();
        let handle = vm.harness().ok_or_else(|| {
            VmError::Runtime(format!(
                "{HOST_SESSION_AWAIT_INBOX}: VM harness is required so command deadlines and inbox waits share one clock"
            ))
        })?;
        handle.inner().clock().clone()
    };
    let woke = crate::orchestration::agent_inbox::wait_async(
        &session_id,
        std::time::Duration::from_millis(timeout_ms),
        &*clock,
    )
    .await;
    Ok(VmValue::Bool(woke))
}

/// Drain queued typed host injections for a delivery seam and append them to the
/// live transcript.
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "__host_agent_session_drain_host_injections(session_id: string, delivery: string, seam: string) -> list",
    category = "agent.host",
    runtime_only = true
)]
fn host_agent_session_drain_host_injections_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let session_id = match args.first() {
        Some(VmValue::String(s)) if !s.is_empty() => s.to_string(),
        _ => {
            return Err(VmError::Runtime(format!(
                "{HOST_SESSION_DRAIN_HOST_INJECTIONS}: session_id must be a non-empty string"
            )))
        }
    };
    let delivery = match args.get(1) {
        Some(VmValue::String(s)) => crate::agent_events::InjectionDelivery::parse(s.as_str())
            .ok_or_else(|| {
                VmError::Runtime(format!(
                    "{HOST_SESSION_DRAIN_HOST_INJECTIONS}: unsupported delivery '{}'",
                    s.as_str()
                ))
            })?,
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "{HOST_SESSION_DRAIN_HOST_INJECTIONS}: unsupported delivery '{}'",
                other.display()
            )))
        }
        None => {
            return Err(VmError::Runtime(format!(
                "{HOST_SESSION_DRAIN_HOST_INJECTIONS}: delivery must be a string"
            )))
        }
    };
    let seam = match args.get(2) {
        Some(VmValue::String(s)) if !s.is_empty() => s.to_string(),
        _ => {
            return Err(VmError::Runtime(format!(
                "{HOST_SESSION_DRAIN_HOST_INJECTIONS}: seam must be a non-empty string"
            )))
        }
    };
    let drained = crate::agent_sessions::drain_queued_host_injections(&session_id, delivery, &seam)
        .map_err(VmError::Runtime)?
        .into_iter()
        .map(|value| json_to_vm(&value))
        .collect::<Vec<_>>();
    Ok(VmValue::List(std::sync::Arc::new(drained)))
}

/// Read accumulated token + cost totals for a session.
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "__host_agent_session_totals(session_id: string) -> dict",
    category = "agent.host",
    runtime_only = true
)]
pub(super) fn host_agent_session_totals_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let session_id = args.first().map(|v| v.display()).unwrap_or_default();
    let totals = with_session(&session_id, HOST_SESSION_TOTALS, |session| {
        Ok(SessionUsageTotals::from(&*session))
    })?;
    // `tokens_used` is cumulative input+output. `input_tokens` / `output_tokens`
    // are the split components — surfaced so budget/detector logic that keys on
    // the re-sent context size (e.g. std/agent/stall's token-runaway guard) can
    // read cumulative INPUT tokens rather than the input+output sum.
    Ok(totals.to_vm(true))
}

/// Persist runtime feedback as a corrective directive in the session envelope.
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "__host_agent_session_inject_feedback(session_id: string, kind: string, content: string) -> nil",
    category = "agent.host",
    runtime_only = true
)]
fn host_agent_session_inject_feedback_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let session_id = args.first().map(|v| v.display()).unwrap_or_default();
    let kind = args.get(1).map(|v| v.display()).unwrap_or_default();
    let content = args.get(2).map(|v| v.display()).unwrap_or_default();
    crate::llm::agent_config::inject_agent_feedback(&session_id, &kind, &content)
        .map_err(VmError::Runtime)?;
    crate::llm::agent_runtime::emit_agent_event_sync(&AgentEvent::feedback_injected(
        session_id, kind, content,
    ));
    Ok(VmValue::Nil)
}

/// Inject a single typed system reminder directly into a live session's
/// transcript event stream, bridge-free. The in-process sibling of the
/// `push_bridge_injection` -> `drain_bridge_injections` reminder path: a host
/// that drives the agent loop in-process (no ACP `HostBridge` attached, e.g.
/// Burin's headless/TUI surfaces) can queue a single-turn ephemeral reminder
/// synchronously, the same way `transcript.inject_reminder` does for a
/// transcript value. `options` mirrors that shape — `body` required; optional
/// `tags`, `dedupe_key`, `ttl_turns`, `preserve_on_compact`, `propagate`,
/// `role_hint`, and `authority`. Same-`dedupe_key` reminders are replaced. The reminder renders
/// into the next model prompt and the loop's existing `apply_reminder_post_turn`
/// pass evicts it once its `ttl_turns` reaches zero. Returns the reminder id.
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "__host_agent_session_inject_reminder(session_id: string, options: dict) -> string",
    category = "agent.host",
    runtime_only = true
)]
fn host_agent_session_inject_reminder_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let session_id = match args.first() {
        Some(VmValue::String(value)) if !value.is_empty() => value.to_string(),
        _ => {
            return Err(VmError::Runtime(format!(
                "{HOST_SESSION_INJECT_REMINDER}: session_id must be a non-empty string"
            )))
        }
    };
    let Some(options) = args.get(1).and_then(|value| value.as_dict()) else {
        return Err(VmError::Runtime(format!(
            "{HOST_SESSION_INJECT_REMINDER}: options must be a dict with a string `body`"
        )));
    };
    crate::llm::conversation::ensure_known_reminder_keys(
        HOST_SESSION_INJECT_REMINDER,
        options,
        crate::llm::conversation::INJECT_REMINDER_KEYS,
    )?;
    let reminder = crate::llm::conversation::parse_inject_reminder_options(
        options,
        HOST_SESSION_INJECT_REMINDER,
    )?;
    let report =
        crate::agent_sessions::inject_reminder(&session_id, reminder).map_err(VmError::Runtime)?;
    Ok(VmValue::String(arcstr::ArcStr::from(report.reminder_id)))
}

/// Post an event into a running session's `agent_inbox`. Surface for
/// triggers, connectors, and external host integrations that want to
/// nudge a session that's already mid-loop without bypassing the
/// canonical drain-at-turn-boundary path (e.g. "the GitHub PR you
/// were waiting on just merged").
/// Post an event into a running session's agent_inbox. Used by triggers,
/// connectors, and external host integrations to nudge a mid-loop session.
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "__host_agent_session_post_event(session_id: string, kind: string, content: string, source?: string|nil) -> nil",
    category = "agent.host",
    runtime_only = true
)]
fn host_agent_session_post_event_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let session_id = match args.first() {
        Some(VmValue::String(s)) if !s.is_empty() => s.to_string(),
        _ => {
            return Err(VmError::Runtime(format!(
                "{HOST_SESSION_POST_EVENT}: session_id must be a non-empty string"
            )))
        }
    };
    let kind = match args.get(1) {
        Some(VmValue::String(s)) if !s.is_empty() => s.to_string(),
        _ => {
            return Err(VmError::Runtime(format!(
                "{HOST_SESSION_POST_EVENT}: kind must be a non-empty string"
            )))
        }
    };
    let content = match args.get(2) {
        Some(VmValue::String(s)) => s.to_string(),
        Some(other) => serde_json::to_string(&vm_to_json(other)).unwrap_or_default(),
        None => String::new(),
    };
    let source = match args.get(3) {
        Some(VmValue::String(s)) if !s.is_empty() => s.to_string(),
        _ => "host.post_event".to_string(),
    };
    crate::orchestration::agent_inbox::push(&session_id, &kind, &content, &source);
    Ok(VmValue::Nil)
}

/// Apply reminder TTL lifecycle after an agent turn.
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "__host_agent_session_apply_reminder_post_turn(session_id: string, turn?: dict|nil) -> dict",
    category = "agent.host",
    runtime_only = true
)]
fn host_agent_session_apply_reminder_post_turn_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let session_id = match args.first() {
        Some(VmValue::String(s)) if !s.is_empty() => s.to_string(),
        _ => {
            return Err(VmError::Runtime(format!(
                "{HOST_SESSION_APPLY_REMINDER_POST_TURN}: session_id must be a non-empty string"
            )))
        }
    };
    let turn = args.get(1).and_then(VmValue::as_int).unwrap_or(0);
    let report = crate::agent_sessions::apply_reminder_post_turn(&session_id, turn)
        .map_err(VmError::Runtime)?;
    Ok(crate::stdlib::json_to_vm_value(&report))
}

const INBOX_BUILTINS: &[&VmBuiltinDef] = &[
    &HOST_AGENT_TRUNCATED_TOOL_CALL_BUILTIN_DEF,
    &HOST_AGENT_SESSION_DRAIN_FEEDBACK_BUILTIN_DEF,
    &HOST_AGENT_SESSION_DRAIN_COMMAND_UPDATES_BUILTIN_DEF,
    &HOST_AGENT_SESSION_DRAIN_HOST_INJECTIONS_BUILTIN_DEF,
    &HOST_AGENT_SESSION_TOTALS_BUILTIN_DEF,
    &HOST_AGENT_SESSION_INJECT_FEEDBACK_BUILTIN_DEF,
    &HOST_AGENT_SESSION_INJECT_REMINDER_BUILTIN_DEF,
    &HOST_AGENT_SESSION_POST_EVENT_BUILTIN_DEF,
    &HOST_AGENT_SESSION_APPLY_REMINDER_POST_TURN_BUILTIN_DEF,
    &HOST_AGENT_SESSION_AWAIT_INBOX_DEF,
];

pub(super) fn register_inbox_primitives(vm: &mut Vm) {
    register_builtin_defs(vm, INBOX_BUILTINS);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn command_wait_without_a_vm_harness_fails_closed() {
        let ctx = crate::vm::AsyncBuiltinCtx::for_test(crate::Vm::new());
        let error = host_agent_session_await_inbox(
            ctx,
            vec![
                VmValue::String("missing-harness-clock".into()),
                VmValue::Int(1),
            ],
        )
        .await
        .expect_err("command waits require the owning VM harness clock");
        let rendered = error.to_string();
        assert!(
            rendered.contains("VM harness is required") && rendered.contains("share one clock"),
            "unexpected error: {rendered}"
        );
    }
}
