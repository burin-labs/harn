//! Daemon snapshots, bridge injection queues, and idle wake checkpoints.

use super::*;

use crate::llm::helpers::{DirectiveAuthority, ReminderSource, SystemReminder};

/// Persist a daemon snapshot for a Harn-driven agent session.
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "__host_agent_daemon_snapshot(session_id: string, options: dict) -> dict",
    category = "agent.host",
    runtime_only = true
)]
fn host_agent_daemon_snapshot_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let session_id = args.first().map(|v| v.display()).unwrap_or_default();
    let opts_map = opts_dict(args.get(1));
    let config = crate::llm::daemon::parse_daemon_loop_config(Some(&opts_map));
    let daemon_state = opt_str(&opts_map, "daemon_state").unwrap_or_else(|| "idle".to_string());
    let total_iterations = opt_int(&opts_map, "total_iterations").unwrap_or(0).max(0) as usize;
    let transcript_summary_override = opt_str(&opts_map, "transcript_summary");
    let transcript = crate::agent_sessions::transcript(&session_id).unwrap_or(VmValue::Nil);
    let transcript_json = vm_to_json(&transcript);
    let visible_messages = transcript_json
        .get("messages")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    let recorded_messages = visible_messages.clone();
    let transcript_events = transcript_json
        .get("events")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    let transcript_summary = transcript_summary_override.or_else(|| {
        transcript_json
            .get("summary")
            .and_then(|value| value.as_str())
            .map(ToString::to_string)
    });
    let total_text = visible_messages
        .iter()
        .filter_map(|message| message.get("content").and_then(|value| value.as_str()))
        .collect::<Vec<_>>()
        .join("\n");
    let last_iteration_text = visible_messages
        .iter()
        .rev()
        .find(|message| message.get("role").and_then(|value| value.as_str()) == Some("assistant"))
        .and_then(|message| message.get("content").and_then(|value| value.as_str()))
        .unwrap_or_default()
        .to_string();

    let mut snapshot = crate::llm::daemon::DaemonSnapshot {
        daemon_state: daemon_state.clone(),
        visible_messages,
        recorded_messages,
        transcript_summary,
        transcript_events,
        total_text,
        last_iteration_text,
        total_iterations,
        ..Default::default()
    }
    .normalize();

    let (snapshot_path, idle_backoff_ms) =
        with_session(&session_id, HOST_DAEMON_SNAPSHOT, |session| {
            if session.daemon_watch_state.is_empty() && !config.watch_paths.is_empty() {
                session.daemon_watch_state = crate::llm::daemon::watch_state(
                    &crate::llm::daemon::RealMtimeProvider,
                    &config.watch_paths,
                );
            }
            snapshot.all_tools_used = session.successful_tools.clone();
            snapshot.rejected_tools = session.rejected_tools.clone();
            snapshot.total_iterations = snapshot
                .total_iterations
                .saturating_add(session.resumed_iterations);
            snapshot.idle_backoff_ms = session.daemon_idle_backoff_ms;
            snapshot.watch_state = session.daemon_watch_state.clone();
            let snapshot_path = if let Some(path) = config.effective_persist_path() {
                Some(crate::llm::daemon::persist_snapshot(path, &snapshot)?)
            } else {
                None
            };
            session.daemon_state = Some(daemon_state.clone());
            if let Some(path) = snapshot_path.as_ref() {
                session.daemon_snapshot_path = Some(path.clone());
            }
            Ok((snapshot_path, session.daemon_idle_backoff_ms))
        })?;

    let mut value = serde_json::to_value(&snapshot).unwrap_or_default();
    value["daemon_snapshot_path"] = snapshot_path
        .as_ref()
        .map(|path| serde_json::json!(path))
        .unwrap_or(serde_json::Value::Null);
    value["idle_backoff_ms"] = serde_json::json!(idle_backoff_ms);
    Ok(json_to_vm(&value))
}

fn host_bridge_for_session(
    session_id: &str,
    builtin_name: &str,
) -> Option<Arc<crate::bridge::HostBridge>> {
    with_session(session_id, builtin_name, |session| {
        Ok(session.host_bridge.clone())
    })
    .ok()
    .flatten()
    .or_else(crate::llm::agent_runtime::current_host_bridge)
}

fn bridge_delivery_checkpoint(value: &str) -> Result<crate::bridge::DeliveryCheckpoint, VmError> {
    match value {
        "interrupt_immediate" | "interrupt" => {
            Ok(crate::bridge::DeliveryCheckpoint::InterruptImmediate)
        }
        "finish_step" | "after_current_operation" => {
            Ok(crate::bridge::DeliveryCheckpoint::AfterCurrentOperation)
        }
        "audit_only" | "end_of_interaction" => {
            Ok(crate::bridge::DeliveryCheckpoint::EndOfInteraction)
        }
        other => Err(VmError::Runtime(format!(
            "{HOST_SESSION_DRAIN_BRIDGE_INJECTIONS}: unsupported checkpoint `{other}`"
        ))),
    }
}

/// Tag carried by the standing directive a delivered steer registers.
pub(crate) const OPERATOR_STEER_TAG: &str = "operator_steer";

/// A steer is an operator control event, not a comment.
///
/// `session/inject` with `mode: "steer"` (and its `interrupt` sibling)
/// splices the operator's instruction into the transcript as a plain
/// `role: user` message, which carries no directive authority at all. The
/// turn-end judge's veto feedback, by contrast, is stamped
/// `DirectiveAuthority::Corrective` in
/// [`crate::llm::agent_config::inject_agent_feedback`], and the rendered
/// envelope tells the model that "contract directives override corrective
/// directives; corrective directives override advisory directives"
/// (`crates/harn-stdlib/src/stdlib/llm/prompts/directive_envelope_instructions.harn.prompt`).
///
/// So a judge re-deriving acceptance from the ORIGINAL task outranked the
/// operator's live steer: traced on a served session, the model complied
/// with the steer on the next turn, the judge vetoed, five corrective
/// directives restated the original task, and the model reverted and ran a
/// tool the steer had forbidden.
///
/// Delivering a steer therefore also registers it as a standing directive at
/// `contract` authority — the level the operator actually holds — so a later
/// corrective cannot contradict an accepted steer. Notes:
///
///   * `ttl_turns: None`. The judge re-injects its corrective on every
///     subsequent iteration, so a steer that expired after one turn would
///     simply lose the same argument later.
///   * `preserve_on_compact: true`. An operator redirect that a compaction
///     silently dropped would revert the run for the same reason.
///   * A dedupe key per message id. Two steers both stand; deduping them
///     against a shared key would let a second steer erase the first, and a
///     dropped steer reads exactly like an obeyed one.
///   * `audit_only` is excluded. It is the one mode whose contract is "lands
///     in the transcript, never rendered into a model prompt" (harn#2212), so
///     minting a directive from it would put text in front of a model that
///     was promised not to see it. This reads the mode the queue entry
///     carries rather than the checkpoint it drained at: the two are a
///     bijection today, but the checkpoint is a delivery detail and a future
///     mapping change would silently stop arming steers.
fn operator_steer_directive(message: &crate::bridge::QueuedUserMessage) -> Option<SystemReminder> {
    if message.mode == crate::bridge::QueuedUserMessageMode::AuditOnly {
        return None;
    }
    let mut reminder = SystemReminder::new(
        format!(
            "The operator redirected this run mid-turn. Follow this instruction for the \
             remainder of the run, in preference to any earlier instruction it contradicts:\n\
             {}",
            message.content
        ),
        ReminderSource::Bridge,
        0,
    );
    reminder.tags = vec![OPERATOR_STEER_TAG.to_string()];
    reminder.dedupe_key = Some(format!("{OPERATOR_STEER_TAG}/{}", message.message_id));
    reminder.authority = DirectiveAuthority::Contract;
    reminder.ttl_turns = None;
    reminder.preserve_on_compact = true;
    Some(reminder)
}

async fn drain_bridge_injections_for_checkpoint(
    session_id: &str,
    bridge: &crate::bridge::HostBridge,
    checkpoint: crate::bridge::DeliveryCheckpoint,
) -> Result<(usize, Option<&'static str>), VmError> {
    let queued = bridge
        .take_queued_transcript_injections_for(checkpoint)
        .await;
    if queued.is_empty() {
        return Ok((0, None));
    }
    let mut saw_user_message = false;
    let mut saw_reminder = false;
    let mut delivered = 0;
    for injection in queued {
        match injection {
            crate::bridge::QueuedTranscriptInjection::User(message) => {
                crate::agent_sessions::inject_message(
                    session_id,
                    json_to_vm(&serde_json::json!({
                        "role": "user",
                        "content": message.transcript_content,
                        "messageId": message.message_id,
                        // The delivery mode is what separates a mid-run user
                        // directive the model actually saw from an audit-only
                        // note it never did. Completion obligations are derived
                        // from this field, so it has to travel with the message
                        // rather than be re-inferred from transcript position.
                        "injectedMode": message.mode.as_str(),
                    })),
                )
                .map_err(VmError::Runtime)?;
                if let Some(directive) = operator_steer_directive(&message) {
                    crate::agent_sessions::inject_reminder(session_id, directive)
                        .map_err(VmError::Runtime)?;
                }
                saw_user_message = true;
                delivered += 1;
            }
            crate::bridge::QueuedTranscriptInjection::Reminder(reminder) => {
                crate::agent_sessions::inject_reminder(session_id, reminder.reminder)
                    .map_err(VmError::Runtime)?;
                saw_reminder = true;
                delivered += 1;
            }
        }
    }
    if saw_user_message {
        Ok((delivered, Some("message")))
    } else if saw_reminder {
        Ok((delivered, Some("reminder")))
    } else {
        Ok((delivered, None))
    }
}

/// Drain `interrupt_immediate` injections on behalf of the daemon idle
/// path and emit a `LoopCheckpoint` so the rest of the seam catalog
/// (Harn-side `agent_stage`, ACP `loop_checkpoint`
/// notifications, debugger views) sees daemon-side activity through the
/// same surface. The actual drain reuses the bridge primitive; this
/// wrapper adds daemon-specific observability while preserving the
/// caller's Harn callback context for live subscribers.
async fn daemon_checkpoint_drain(
    ctx: &crate::vm::AsyncBuiltinCtx,
    session_id: &str,
    bridge: &crate::bridge::HostBridge,
    kind: &'static str,
) -> Result<(usize, Option<&'static str>), VmError> {
    let (delivered, reason) = drain_bridge_injections_for_checkpoint(
        session_id,
        bridge,
        crate::bridge::DeliveryCheckpoint::InterruptImmediate,
    )
    .await?;
    let event = crate::agent_events::AgentEvent::LoopCheckpoint {
        session_id: session_id.to_string(),
        iteration: 0,
        kind: kind.to_string(),
        delivered,
        inbox_delivered: 0,
        typed_delivered: 0,
        dispatch_skipped: false,
    };
    crate::llm::agent_runtime::emit_agent_event_with_ctx(Some(ctx), &event).await;
    Ok((delivered, reason))
}

/// Push a system-reminder onto the session's host bridge queue. The
/// inverse of `__host_agent_session_drain_bridge_injections` — exposed
/// so a Harn script driving the loop (custom CLI host, conformance
/// test, etc.) can queue an injection that lands at the next eligible
/// checkpoint without going through ACP.
///
/// Expects `options` shaped like the `session/remind` JSON-RPC params
/// (`body`, `mode`, optional `tags`, `dedupe_key`, `ttl_turns`,
/// `role_hint`, `authority`, `propagate`, `preserve_on_compact`). Returns the
/// reminder id so callers can correlate with later
/// `ReminderEmitted` events.
/// Push a system-reminder onto the session's host bridge queue;
/// returns the reminder id. Inverse of drain_bridge_injections.
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "__host_agent_session_push_bridge_injection(session_id: string, options: dict) -> string",
    kind = "async",
    category = "agent.host",
    runtime_only = true
)]
async fn host_agent_session_push_bridge_injection(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let session_id = args.first().map(|v| v.display()).unwrap_or_default();
    if session_id.trim().is_empty() {
        return Err(VmError::Runtime(format!(
            "{HOST_SESSION_PUSH_BRIDGE_INJECTION}: session_id must be a non-empty string"
        )));
    }
    let options = args.get(1).cloned().unwrap_or(VmValue::Nil);
    let params = vm_to_json(&options);
    if !params.is_object() {
        return Err(VmError::Runtime(format!(
            "{HOST_SESSION_PUSH_BRIDGE_INJECTION}: options must be a dict"
        )));
    }
    let Some(bridge) = host_bridge_for_session(&session_id, HOST_SESSION_PUSH_BRIDGE_INJECTION)
    else {
        return Err(VmError::Runtime(format!(
            "{HOST_SESSION_PUSH_BRIDGE_INJECTION}: no host bridge attached to session `{session_id}`"
        )));
    };
    let reminder_id = bridge
        .push_queued_session_remind_from_params(&params)
        .await
        .map_err(|message| {
            VmError::Runtime(format!("{HOST_SESSION_PUSH_BRIDGE_INJECTION}: {message}"))
        })?;
    Ok(VmValue::String(arcstr::ArcStr::from(reminder_id)))
}

/// Push a user-role message onto the session's host bridge queue. The
/// in-VM equivalent of the ACP `session/inject` JSON-RPC method (the
/// user-message sibling of `__host_agent_session_push_bridge_injection`,
/// which is the in-VM equivalent of `session/remind`). Exposed so a Harn
/// script driving the loop (custom CLI host, conformance test, etc.) can
/// enqueue a steer mid-turn without going through ACP.
///
/// Expects `options.content` (string, required, non-empty) and an
/// optional `options.mode` (string, default `"finish_step"`). The mode
/// follows `QueuedUserMessageMode::from_str`: `"finish_step"` /
/// `"after_current_operation"` / `"steer"` deliver at the next loop
/// checkpoint (tool boundary / iteration boundary); `"interrupt_immediate"`
/// / `"interrupt"` preempt the next tool batch; `"audit_only"` / `"queue"`
/// land in the transcript at `loop_exit` only. Returns the message id so
/// callers can correlate with later events / revoke the message.
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "__host_agent_session_push_user_message(session_id: string, options: dict) -> string",
    kind = "async",
    category = "agent.host",
    runtime_only = true
)]
async fn host_agent_session_push_user_message(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let session_id = args.first().map(|v| v.display()).unwrap_or_default();
    if session_id.trim().is_empty() {
        return Err(VmError::Runtime(format!(
            "{HOST_SESSION_PUSH_USER_MESSAGE}: session_id must be a non-empty string"
        )));
    }
    let options = args.get(1).cloned().unwrap_or(VmValue::Nil);
    let params = vm_to_json(&options);
    if !params.is_object() {
        return Err(VmError::Runtime(format!(
            "{HOST_SESSION_PUSH_USER_MESSAGE}: options must be a dict"
        )));
    }
    let content = params
        .get("content")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    if content.trim().is_empty() {
        return Err(VmError::Runtime(format!(
            "{HOST_SESSION_PUSH_USER_MESSAGE}: options.content must be a non-empty string"
        )));
    }
    let mode = params
        .get("mode")
        .and_then(|value| value.as_str())
        .unwrap_or("finish_step")
        .to_string();
    let Some(bridge) = host_bridge_for_session(&session_id, HOST_SESSION_PUSH_USER_MESSAGE) else {
        return Err(VmError::Runtime(format!(
            "{HOST_SESSION_PUSH_USER_MESSAGE}: no host bridge attached to session `{session_id}`"
        )));
    };
    let message_id = bridge.push_queued_user_message(content, &mode).await;
    Ok(VmValue::String(arcstr::ArcStr::from(message_id)))
}

/// Return a FIFO snapshot of pending bridge user-message and reminder injections.
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "__host_agent_session_pending_injections(session_id: string) -> list",
    kind = "async",
    category = "agent.host",
    runtime_only = true
)]
async fn host_agent_session_pending_injections(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let session_id = args.first().map(|v| v.display()).unwrap_or_default();
    if session_id.trim().is_empty() {
        return Err(VmError::Runtime(format!(
            "{HOST_SESSION_PENDING_INJECTIONS}: session_id must be a non-empty string"
        )));
    }
    let Some(bridge) = host_bridge_for_session(&session_id, HOST_SESSION_PENDING_INJECTIONS) else {
        return Ok(json_to_vm(&serde_json::json!({
            "pendingCount": 0,
            "injections": [],
        })));
    };
    Ok(json_to_vm(&bridge.pending_injections_json().await))
}

/// Revoke a queued bridge reminder before an agent checkpoint drains it.
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "__host_agent_session_revoke_reminder(session_id: string, reminder_id: string) -> bool",
    kind = "async",
    category = "agent.host",
    runtime_only = true
)]
async fn host_agent_session_revoke_reminder(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let session_id = args.first().map(|v| v.display()).unwrap_or_default();
    if session_id.trim().is_empty() {
        return Err(VmError::Runtime(format!(
            "{HOST_SESSION_REVOKE_REMINDER}: session_id must be a non-empty string"
        )));
    }
    let reminder_id = args.get(1).map(|v| v.display()).unwrap_or_default();
    if reminder_id.trim().is_empty() {
        return Err(VmError::Runtime(format!(
            "{HOST_SESSION_REVOKE_REMINDER}: reminder_id must be a non-empty string"
        )));
    }
    let session_status = match crate::agent_sessions::revoke_reminder(&session_id, &reminder_id) {
        Ok(status) => status,
        Err(error) => return Err(VmError::Runtime(error)),
    };
    if matches!(session_status, "revoked" | "already_revoked") {
        if let Some(bridge) = host_bridge_for_session(&session_id, HOST_SESSION_REVOKE_REMINDER) {
            let _ = bridge.revoke_pending_reminder(&reminder_id).await;
        }
        return Ok(json_to_vm(&serde_json::json!({
            "status": session_status,
            "reminderId": reminder_id,
        })));
    }
    if let Some(bridge) = host_bridge_for_session(&session_id, HOST_SESSION_REVOKE_REMINDER) {
        let status = match bridge.revoke_pending_reminder(&reminder_id).await {
            crate::bridge::PendingReminderMutationResult::Mutated => "revoked",
            crate::bridge::PendingReminderMutationResult::AlreadyRevoked => "already_revoked",
            crate::bridge::PendingReminderMutationResult::AlreadyDelivered => "already_delivered",
            crate::bridge::PendingReminderMutationResult::UnknownReminderId => session_status,
        };
        return Ok(json_to_vm(&serde_json::json!({
            "status": status,
            "reminderId": reminder_id,
        })));
    }
    Ok(json_to_vm(&serde_json::json!({
        "status": session_status,
        "reminderId": reminder_id,
    })))
}

/// Drain queued bridge transcript injections for a delivery checkpoint.
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "__host_agent_session_drain_bridge_injections(session_id: string, checkpoint: dict) -> list",
    kind = "async",
    category = "agent.host",
    runtime_only = true
)]
async fn host_agent_session_drain_bridge_injections(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let session_id = args.first().map(|v| v.display()).unwrap_or_default();
    if session_id.trim().is_empty() {
        return Err(VmError::Runtime(format!(
            "{HOST_SESSION_DRAIN_BRIDGE_INJECTIONS}: session_id must be a non-empty string"
        )));
    }
    let checkpoint_arg = args
        .get(1)
        .map(|value| value.display())
        .unwrap_or_else(|| "finish_step".to_string());
    let checkpoint = bridge_delivery_checkpoint(&checkpoint_arg)?;
    let Some(bridge) = host_bridge_for_session(&session_id, HOST_SESSION_DRAIN_BRIDGE_INJECTIONS)
    else {
        return Ok(json_to_vm(&serde_json::json!({
            "delivered": 0,
            "reason": "none",
        })));
    };
    let (delivered, reason) =
        drain_bridge_injections_for_checkpoint(&session_id, &bridge, checkpoint).await?;
    Ok(json_to_vm(&serde_json::json!({
        "delivered": delivered,
        "reason": reason.unwrap_or("none"),
    })))
}

/// Wait for daemon wake input or a timeout.
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "__host_agent_daemon_wait(session_id: string, timeout_ms: int) -> dict",
    kind = "async",
    category = "agent.host",
    runtime_only = true
)]
async fn host_agent_daemon_wait(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let session_id = args.first().map(|v| v.display()).unwrap_or_default();
    let timeout_ms = args
        .get(1)
        .and_then(VmValue::as_int)
        .map(|value| value.max(0) as u64)
        .unwrap_or(0);
    let bridge = host_bridge_for_session(&session_id, HOST_DAEMON_WAIT);
    let has_bridge = bridge.is_some();
    if let Some(bridge) = bridge.as_ref() {
        bridge.set_daemon_idle(true);
        bridge.notify(
            "agent/idle",
            serde_json::json!({"session_id": session_id, "timeout_ms": timeout_ms}),
        );
        if bridge.take_resume_signal() {
            bridge.set_daemon_idle(false);
            return Ok(json_to_vm(&serde_json::json!({"reason": "resume"})));
        }
        let (_, reason) =
            daemon_checkpoint_drain(&ctx, &session_id, bridge, "daemon_idle_pre").await?;
        if let Some(reason) = reason {
            bridge.set_daemon_idle(false);
            return Ok(json_to_vm(&serde_json::json!({"reason": reason})));
        }
    }

    if timeout_ms > 0 {
        tokio::time::sleep(std::time::Duration::from_millis(timeout_ms)).await;
    }

    if let Some(bridge) = bridge.as_ref() {
        let (_, reason) =
            daemon_checkpoint_drain(&ctx, &session_id, bridge, "daemon_idle_post").await?;
        if let Some(reason) = reason {
            bridge.set_daemon_idle(false);
            return Ok(json_to_vm(&serde_json::json!({"reason": reason})));
        }
        bridge.set_daemon_idle(false);
    }

    if timeout_ms > 0 && !has_bridge {
        Ok(json_to_vm(&serde_json::json!({
            "reason": "timer",
            "feedback_kind": "timer",
            "feedback": "Daemon wake interval elapsed.",
        })))
    } else {
        Ok(json_to_vm(&serde_json::json!({"reason": nil_json()})))
    }
}

fn nil_json() -> serde_json::Value {
    serde_json::Value::Null
}

const DAEMON_BRIDGE_BUILTINS: &[&VmBuiltinDef] = &[
    &HOST_AGENT_DAEMON_SNAPSHOT_BUILTIN_DEF,
    &HOST_AGENT_SESSION_DRAIN_BRIDGE_INJECTIONS_DEF,
    &HOST_AGENT_SESSION_PUSH_BRIDGE_INJECTION_DEF,
    &HOST_AGENT_SESSION_PUSH_USER_MESSAGE_DEF,
    &HOST_AGENT_SESSION_PENDING_INJECTIONS_DEF,
    &HOST_AGENT_SESSION_REVOKE_REMINDER_DEF,
    &HOST_AGENT_DAEMON_WAIT_DEF,
];

pub(super) fn register_daemon_bridge_primitives(vm: &mut Vm) {
    register_builtin_defs(vm, DAEMON_BRIDGE_BUILTINS);
}

#[cfg(test)]
mod operator_steer_tests {
    use super::*;
    use crate::bridge::{QueuedUserMessage, QueuedUserMessageMode};

    fn queued(mode: QueuedUserMessageMode) -> QueuedUserMessage {
        QueuedUserMessage {
            message_id: "msg_inj_0199".to_string(),
            content: "do not call look again; final reply must be exactly BRAVO".to_string(),
            transcript_content: serde_json::json!(
                "do not call look again; final reply must be exactly BRAVO"
            ),
            mode,
        }
    }

    /// A delivered steer must outrank the turn-end judge's `corrective`
    /// feedback, so it is registered at the authority the operator holds.
    #[test]
    fn a_delivered_steer_becomes_a_standing_contract_directive() {
        let directive = operator_steer_directive(&queued(QueuedUserMessageMode::FinishStep))
            .expect("a steer delivered mid-turn registers a directive");

        assert_eq!(directive.authority, DirectiveAuthority::Contract);
        assert!(directive.body.contains("BRAVO"));
        assert_eq!(directive.tags, vec![OPERATOR_STEER_TAG.to_string()]);
        assert_eq!(
            directive.dedupe_key.as_deref(),
            Some("operator_steer/msg_inj_0199"),
            "each steer keeps its own key: a shared key would let a second steer \
             silently erase the first, and a dropped steer reads like an obeyed one"
        );
        assert_eq!(
            directive.ttl_turns, None,
            "the judge re-injects its corrective every iteration, so an expiring \
             steer would simply lose the same argument one turn later"
        );
        assert!(
            directive.preserve_on_compact,
            "a compaction that dropped the operator's redirect would revert the run"
        );
    }

    /// The interrupt sibling is the same control event delivered sooner.
    #[test]
    fn an_interrupt_carries_the_same_authority_as_a_steer() {
        let directive =
            operator_steer_directive(&queued(QueuedUserMessageMode::InterruptImmediate))
                .expect("an interrupt delivered mid-turn registers a directive");
        assert_eq!(directive.authority, DirectiveAuthority::Contract);
    }

    /// Negative control. `audit_only` is the one mode whose contract is
    /// "lands in the transcript, never rendered into a model prompt"
    /// (harn#2212). Minting a directive from it would put text in front of a
    /// model that was explicitly promised not to see it.
    #[test]
    fn an_audit_only_message_never_becomes_a_directive() {
        assert!(operator_steer_directive(&queued(QueuedUserMessageMode::AuditOnly)).is_none());
    }
}
