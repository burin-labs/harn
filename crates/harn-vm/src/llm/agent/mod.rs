use std::rc::Rc;

use crate::agent_events::{self, AgentEvent};
use crate::value::{VmError, VmValue};

use super::daemon::detect_watch_changes;
use super::helpers::transcript_event;

// Imports from extracted submodules.
use super::agent_config::AgentLoopConfig;

mod finalize;
mod helpers;
mod llm_call;
mod state;
mod tool_dispatch;
mod turn_preflight;

use helpers::{
    action_turn_nudge, append_host_messages_to_recorded, append_message_to_contexts,
    assistant_history_text, daemon_snapshot_from_state, inject_queued_user_messages,
    interpret_post_turn_callback_result, maybe_auto_compact_agent_messages,
    maybe_persist_daemon_snapshot, runtime_feedback_message, should_stop_after_successful_tools,
};

thread_local! {
    static CURRENT_HOST_BRIDGE: std::cell::RefCell<Option<Rc<crate::bridge::HostBridge>>> = const { std::cell::RefCell::new(None) };
    /// Queue of feedback items pushed via `agent_inject_feedback(session_id, kind, content)`
    /// from inside a pipeline event handler. The turn loop drains this
    /// queue at safe boundaries (before each LLM call) and appends each
    /// entry as a runtime-feedback message.
    static PENDING_FEEDBACK: std::cell::RefCell<Vec<(String, String, String)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Emit an event through both external sinks (sync) and closure
/// subscribers (async, via the agent-loop's VM context). Called by the
/// turn loop at every phase.
///
/// **Thread-local invariant.** Pipeline closure subscribers are stored
/// in a `thread_local!` registry in `agent_events.rs` because
/// `VmValue` wraps `Rc` and can't cross threads. The agent loop itself
/// runs on a tokio `LocalSet`-pinned task, and `agent_subscribe`
/// (the host builtin that populates the registry) runs on that same
/// task, so the invariant holds. If a future VM embedder runs the
/// loop from a multi-thread runtime without a `LocalSet`, closure
/// subscribers will silently decouple from their emit site. The
/// `debug_assert!` below catches that invariant violation in debug
/// builds; release builds tolerate the divergence rather than panic
/// on a misconfigured embedding.
async fn emit_agent_event(event: &AgentEvent) {
    // External (Rust-side) sinks first — they're always sync.
    agent_events::emit_event(event);

    // Pipeline closure subscribers — invoke via the async VM API.
    let subscribers = agent_events::closure_subscribers_for(event.session_id());
    if subscribers.is_empty() {
        return;
    }
    let payload = serde_json::to_value(event).unwrap_or(serde_json::Value::Null);
    for closure in subscribers {
        let VmValue::Closure(closure) = closure else {
            continue;
        };
        let Some(mut vm) = crate::vm::clone_async_builtin_child_vm() else {
            continue;
        };
        let arg = crate::stdlib::json_to_vm_value(&payload);
        // Log but do not propagate subscriber errors — one misbehaving
        // subscriber (e.g. a pipeline grounding handler with a type
        // error) must not tear down the agent loop. Silent drops hid
        // pipeline bugs; logging surfaces them without escalating.
        if let Err(err) = vm.call_closure_pub(&closure, &[arg], &[]).await {
            crate::events::log_warn(
                "agent.subscriber",
                &format!(
                    "session={} event={:?} subscriber error: {}",
                    event.session_id(),
                    std::mem::discriminant(event),
                    err
                ),
            );
        }
    }
}

/// Push a pending-feedback item. Called by the `agent_inject_feedback`
/// host builtin; drained by the turn loop.
pub(crate) fn push_pending_feedback(session_id: &str, kind: &str, content: &str) {
    PENDING_FEEDBACK.with(|q| {
        q.borrow_mut().push((
            session_id.to_string(),
            kind.to_string(),
            content.to_string(),
        ))
    });
}

/// Drain every pending-feedback item for a session. Called by the turn
/// loop at injection boundaries.
pub(super) fn drain_pending_feedback(session_id: &str) -> Vec<(String, String)> {
    PENDING_FEEDBACK.with(|q| {
        let mut queue = q.borrow_mut();
        let mut drained: Vec<(String, String)> = Vec::new();
        let mut kept: Vec<(String, String, String)> = Vec::new();
        for (sid, kind, content) in queue.drain(..) {
            if sid == session_id {
                drained.push((kind, content));
            } else {
                kept.push((sid, kind, content));
            }
        }
        *queue = kept;
        drained
    })
}

pub(crate) fn install_current_host_bridge(bridge: Rc<crate::bridge::HostBridge>) {
    CURRENT_HOST_BRIDGE.with(|slot| {
        *slot.borrow_mut() = Some(bridge);
    });
}

pub(crate) fn current_host_bridge() -> Option<Rc<crate::bridge::HostBridge>> {
    CURRENT_HOST_BRIDGE.with(|slot| slot.borrow().clone())
}

pub async fn run_agent_loop_internal(
    opts: &mut super::api::LlmCallOptions,
    config: AgentLoopConfig,
) -> Result<serde_json::Value, VmError> {
    // Build the long-lived loop state (drop guards, prelude computations,
    // daemon snapshot resume). The original inline prelude now lives on
    // `AgentLoopState::new` — behavior must be identical.
    let mut state = state::AgentLoopState::new(opts, config)?;

    // Rebuild the `tools` borrow the loop body reads. `AgentLoopState::new`
    // already mutated `opts.native_tools` and `opts.tool_choice` so these
    // views are stable for the rest of the run.
    let tools_owned = opts.tools.clone();
    let tools_val = tools_owned.as_ref();

    // Snapshot the config fields the iteration loop reads as locals so
    // we don't hold an immutable borrow on `state.config` across the
    // loop body (which would conflict with the `&mut state` phase
    // methods take). `config.turn_policy` is `Option<TurnPolicy>`;
    // clone it once here rather than `.as_ref()`-ing through a borrow.
    let llm_retries: usize = state.config.llm_retries;
    let llm_backoff_ms: u64 = state.config.llm_backoff_ms;
    let turn_policy = state.config.turn_policy.clone();
    let stop_after_successful_tools = state.config.stop_after_successful_tools.clone();
    let post_turn_callback = state.config.post_turn_callback.clone();

    // Copy/clone bindings for identifiers that collide with argument
    // names, module paths, or pattern bindings (so renaming `state.foo`
    // at every callsite would be brittle). `bridge` is an `Option<Rc>`,
    // cheap to clone; the rest are small scalars or already-cloned
    // owned values.
    let bridge = state.bridge.clone();
    let max_iterations: usize = state.max_iterations;
    let max_nudges: usize = state.max_nudges;
    let tool_retries: usize = state.tool_retries;
    let tool_backoff_ms: u64 = state.tool_backoff_ms;
    let exit_when_verified: bool = state.exit_when_verified;
    let persistent: bool = state.persistent;
    let daemon: bool = state.daemon;
    let has_tools: bool = state.has_tools;
    let loop_detect_enabled: bool = state.loop_detect_enabled;
    let resumed_iterations: usize = state.resumed_iterations;
    let tool_format = state.tool_format.clone();
    let done_sentinel = state.done_sentinel.clone();
    let break_unless_phase = state.break_unless_phase.clone();
    let loop_start = state.loop_start;
    let tool_contract_prompt = state.tool_contract_prompt.clone();
    let base_system = state.base_system.clone();
    let persistent_system_prompt = state.persistent_system_prompt.clone();
    let auto_compact = state.auto_compact.clone();
    let daemon_config = state.daemon_config.clone();
    let custom_nudge = state.custom_nudge.clone();
    let session_id = state.session_id.clone();

    for iteration in 0..max_iterations {
        turn_preflight::run_turn_preflight(
            &mut state,
            opts,
            turn_preflight::PreflightContext {
                bridge: &bridge,
                session_id: &session_id,
                resumed_iterations,
                iteration,
                base_system: base_system.as_deref(),
                tool_contract_prompt: tool_contract_prompt.as_deref(),
                persistent_system_prompt: persistent_system_prompt.as_deref(),
            },
        )
        .await?;

        let mut call_result = llm_call::run_llm_call(
            &mut state,
            opts,
            &llm_call::LlmCallContext {
                bridge: &bridge,
                tool_format: &tool_format,
                done_sentinel: &done_sentinel,
                break_unless_phase: break_unless_phase.as_deref(),
                exit_when_verified,
                persistent,
                has_tools,
                turn_policy: turn_policy.as_ref(),
                llm_retries,
                llm_backoff_ms,
                tools_val,
            },
            iteration,
        )
        .await?;

        if !call_result.tool_calls.is_empty() {
            let tool_dispatch::ToolDispatchResult {
                tools_used_this_iter,
                tool_results_this_iter,
                observations,
                rejection_followups,
            } = tool_dispatch::run_tool_dispatch(
                &mut state,
                opts,
                &tool_dispatch::ToolDispatchContext {
                    bridge: &bridge,
                    tool_format: &tool_format,
                    tools_val,
                    tool_retries,
                    tool_backoff_ms,
                    loop_detect_enabled,
                    session_id: &session_id,
                    iteration,
                    exit_when_verified,
                    auto_compact: &auto_compact,
                },
                &call_result,
            )
            .await?;

            state.all_tools_used.extend(tools_used_this_iter);
            if tool_format != "native" && !observations.is_empty() {
                append_message_to_contexts(
                    &mut state.visible_messages,
                    &mut state.recorded_messages,
                    runtime_feedback_message("tool_results", observations.trim_end()),
                );
            }
            if !rejection_followups.is_empty() {
                append_message_to_contexts(
                    &mut state.visible_messages,
                    &mut state.recorded_messages,
                    runtime_feedback_message("tool_rejection", rejection_followups.join("\n\n")),
                );
            }
            let finish_step_messages = inject_queued_user_messages(
                bridge.as_ref(),
                &mut state.visible_messages,
                crate::bridge::DeliveryCheckpoint::AfterCurrentOperation,
            )
            .await?;
            append_host_messages_to_recorded(&mut state.recorded_messages, &finish_step_messages);
            for message in &finish_step_messages {
                state.transcript_events.push(transcript_event(
                    "host_input",
                    "user",
                    "public",
                    &message.content,
                    Some(serde_json::json!({"delivery": format!("{:?}", message.mode)})),
                ));
            }
            if !finish_step_messages.is_empty() {
                state.consecutive_text_only = 0;
            }

            // Post-turn callback: let the pipeline inspect each tool turn
            // and optionally inject a user message (e.g. batching hints,
            // progress tracking, adaptive instructions).
            if call_result.tool_calls.len() == 1 {
                state.consecutive_single_tool_turns += 1;
            } else {
                state.consecutive_single_tool_turns = 0;
            }
            let successful_tool_names: Vec<&str> = tool_results_this_iter
                .iter()
                .filter(|result| result["status"].as_str() == Some("ok"))
                .filter_map(|result| result["tool_name"].as_str())
                .collect();
            for tool_name in &successful_tool_names {
                if !state
                    .successful_tools_used
                    .iter()
                    .any(|existing| existing == tool_name)
                {
                    state.successful_tools_used.push((*tool_name).to_string());
                }
            }
            // Emit TurnEnd. Pipeline subscribers may react by pushing
            // pending-feedback messages via `agent_inject_feedback`;
            // those are drained at the top of the next iteration before
            // the LLM is called again.
            let turn_info = {
                let tool_names: Vec<&str> = call_result
                    .tool_calls
                    .iter()
                    .filter_map(|tc| tc["name"].as_str())
                    .collect();
                let turn_info = serde_json::json!({
                    "tool_names": tool_names,
                    "tool_results": tool_results_this_iter,
                    "successful_tool_names": successful_tool_names,
                    "tool_count": call_result.tool_calls.len(),
                    "iteration": iteration,
                    "consecutive_single_tool_turns": state.consecutive_single_tool_turns,
                    "session_tools_used": state.all_tools_used,
                    "session_successful_tools": state.successful_tools_used,
                });
                emit_agent_event(&AgentEvent::TurnEnd {
                    session_id: session_id.clone(),
                    iteration,
                    turn_info: turn_info.clone(),
                })
                .await;
                turn_info
            };
            if let Some(stop_tools) = stop_after_successful_tools.as_ref() {
                if should_stop_after_successful_tools(&tool_results_this_iter, stop_tools) {
                    crate::events::log_debug(
                        "agent.stop_after_successful_tools",
                        &format!(
                            "iter={iteration} requested stage stop after successful tool turn"
                        ),
                    );
                    break;
                }
            }
            // Invoke the optional post_turn_callback. Accepts:
            //   ""/nil: no-op
            //   string: inject as runtime-feedback user message
            //   bool true: stop the stage immediately
            //   dict {message, stop}: both (optional fields)
            if let Some(VmValue::Closure(closure)) = post_turn_callback.as_ref() {
                {
                    let mut cb_vm = crate::vm::clone_async_builtin_child_vm().ok_or_else(|| {
                        VmError::Runtime(
                            "post_turn_callback requires an async builtin VM context".to_string(),
                        )
                    })?;
                    let info_vm = crate::stdlib::json_to_vm_value(&turn_info);
                    let cb_result = cb_vm.call_closure_pub(closure, &[info_vm], &[]).await?;
                    let (message, stop) = interpret_post_turn_callback_result(&cb_result);
                    if let Some(msg) = message {
                        if !msg.trim().is_empty() {
                            let feedback =
                                runtime_feedback_message("post_turn_callback", msg.as_str());
                            append_message_to_contexts(
                                &mut state.visible_messages,
                                &mut state.recorded_messages,
                                feedback,
                            );
                        }
                    }
                    if stop {
                        crate::events::log_debug(
                            "agent.post_turn_callback",
                            &format!("iter={iteration} post_turn_callback requested stage stop"),
                        );
                        break;
                    }
                }
            }

            // Auto-compaction check after tool processing.
            // Include the system prompt + tool definitions in the estimate
            // since they consume context window alongside messages.
            if let Some(ref ac) = auto_compact {
                let mut est =
                    crate::orchestration::estimate_message_tokens(&state.visible_messages);
                if let Some(ref sys) = opts.system {
                    est += sys.len() / 4;
                }
                if est > ac.token_threshold {
                    let mut compact_opts = opts.clone();
                    compact_opts.messages = state.visible_messages.clone();
                    if let Some(summary) = crate::orchestration::auto_compact_messages(
                        &mut state.visible_messages,
                        ac,
                        Some(&compact_opts),
                    )
                    .await?
                    {
                        super::trace::emit_agent_event(
                            super::trace::AgentTraceEvent::ContextCompaction {
                                archived_messages: est.saturating_sub(
                                    crate::orchestration::estimate_message_tokens(
                                        &state.visible_messages,
                                    ),
                                ),
                                new_summary_len: summary.len(),
                                iteration,
                            },
                        );
                        let merged = match state.transcript_summary.take() {
                            Some(existing)
                                if !existing.trim().is_empty()
                                    && existing.trim() != summary.trim() =>
                            {
                                format!("{existing}\n\n{summary}")
                            }
                            Some(_) | None => summary,
                        };
                        state.transcript_summary = Some(merged);
                    }
                }
            }

            // Feed parse-error diagnostics back in the mixed case too, so the
            // model can correct its syntax in the next turn (mirrors the
            // text-only branch below). Without this, rejected calls would
            // silently disappear from the conversation.
            if !call_result.tool_parse_errors.is_empty() {
                let error_msg = call_result.tool_parse_errors.join("\n\n");
                append_message_to_contexts(
                    &mut state.visible_messages,
                    &mut state.recorded_messages,
                    runtime_feedback_message("parse_error", error_msg),
                );
            }
            if call_result.sentinel_hit {
                if !call_result.tool_parse_errors.is_empty() {
                    crate::events::log_warn(
                        "llm.tool",
                        &format!(
                            "{} tool-call parse error(s) suppressed by sentinel: {}",
                            call_result.tool_parse_errors.len(),
                            call_result.tool_parse_errors.join("; ")
                        ),
                    );
                }
                break;
            }
            continue;
        }

        let assistant_content_for_history = assistant_history_text(
            call_result.canonical_history.as_deref(),
            &call_result.text,
            call_result.tool_parse_errors.len(),
            &call_result.tool_calls,
        );
        append_message_to_contexts(
            &mut state.visible_messages,
            &mut state.recorded_messages,
            serde_json::json!({
                "role": "assistant",
                "content": assistant_content_for_history,
            }),
        );

        // Sentinel check for text-only responses (no tool calls).
        if call_result.sentinel_hit {
            break;
        }

        // If the model attempted tool calls but parsing failed, send diagnostics
        // back so it can fix its syntax instead of being silently nudged.
        if !call_result.tool_parse_errors.is_empty() {
            let error_msg = call_result.tool_parse_errors.join("\n\n");
            append_message_to_contexts(
                &mut state.visible_messages,
                &mut state.recorded_messages,
                runtime_feedback_message("parse_error", error_msg),
            );
            call_result.tool_parse_errors.clear();
            state.consecutive_text_only = 0;
            continue;
        }

        // done_sentinel already checked before tool dispatch above;
        // this path only reached for text-only responses without sentinel.
        if !persistent && !daemon {
            break;
        }

        // Daemon mode: if no tool calls and agent is idle, notify host and
        // wait briefly for user messages before deciding to continue/exit.
        if daemon && !persistent {
            state.daemon_state = "idle".to_string();
            if daemon_config.consolidate_on_idle {
                maybe_auto_compact_agent_messages(
                    opts,
                    &auto_compact,
                    &mut state.visible_messages,
                    &mut state.transcript_summary,
                )
                .await?;
            }
            let idle_snapshot = daemon_snapshot_from_state(
                &state.daemon_state,
                &state.visible_messages,
                &state.recorded_messages,
                &state.transcript_summary,
                &state.transcript_events,
                &state.total_text,
                &state.last_iteration_text,
                &state.all_tools_used,
                &state.rejected_tools,
                &state.deferred_user_messages,
                state.total_iterations,
                state.idle_backoff_ms,
                state.last_run_exit_code,
                &state.daemon_watch_state,
            );
            state.daemon_snapshot_path =
                maybe_persist_daemon_snapshot(&daemon_config, &idle_snapshot)?
                    .or(state.daemon_snapshot_path);
            if !daemon_config.has_wake_source(bridge.is_some()) {
                state.final_status = "idle";
                break;
            }
            loop {
                if let Some(bridge) = bridge.as_ref() {
                    bridge.notify(
                        "agent/idle",
                        serde_json::json!({
                            "iteration": state.total_iterations,
                            "backoff_ms": state.idle_backoff_ms,
                            "persist_path": state.daemon_snapshot_path,
                            "watch_paths": daemon_config.watch_paths,
                        }),
                    );
                }
                tokio::time::sleep(tokio::time::Duration::from_millis(
                    daemon_config.idle_wait_ms(state.idle_backoff_ms),
                ))
                .await;
                let resumed = bridge
                    .as_ref()
                    .is_some_and(|bridge| bridge.take_resume_signal());
                let idle_messages = inject_queued_user_messages(
                    bridge.as_ref(),
                    &mut state.visible_messages,
                    crate::bridge::DeliveryCheckpoint::InterruptImmediate,
                )
                .await?;
                append_host_messages_to_recorded(&mut state.recorded_messages, &idle_messages);
                let changed_paths = if daemon_config.watch_paths.is_empty() {
                    Vec::new()
                } else {
                    detect_watch_changes(&daemon_config.watch_paths, &mut state.daemon_watch_state)
                };
                for message in &idle_messages {
                    state.transcript_events.push(transcript_event(
                        "host_input",
                        "user",
                        "public",
                        &message.content,
                        Some(serde_json::json!({"delivery": format!("{:?}", message.mode)})),
                    ));
                }
                let wake_reason = if !idle_messages.is_empty() {
                    Some(("message", None))
                } else if resumed {
                    Some(("resume", None))
                } else if !changed_paths.is_empty() {
                    Some((
                        "watch",
                        Some(format!(
                            "Daemon wake: watched paths changed: {}. Re-check the task state and act only if something actually changed.",
                            changed_paths.join(", ")
                        )),
                    ))
                } else if daemon_config.wake_interval_ms.is_some() {
                    Some((
                        "timer",
                        Some(
                            "Daemon timer wake fired. Re-check for background work and only act when there is new information or a pending follow-up."
                                .to_string(),
                        ),
                    ))
                } else {
                    None
                };
                if let Some((reason, wake_message)) = wake_reason {
                    if let Some(message) = wake_message {
                        append_message_to_contexts(
                            &mut state.visible_messages,
                            &mut state.recorded_messages,
                            runtime_feedback_message(reason, message),
                        );
                    }
                    state.transcript_events.push(transcript_event(
                        "daemon_wake",
                        "system",
                        "internal",
                        reason,
                        Some(serde_json::json!({
                            "reason": reason,
                            "watch_paths": changed_paths,
                            "resumed": resumed,
                        })),
                    ));
                    state.daemon_state = "active".to_string();
                    state.consecutive_text_only = 0;
                    state.idle_backoff_ms = 100;
                    break;
                }
                daemon_config.update_idle_backoff(&mut state.idle_backoff_ms);
            }
            continue;
        }

        let finish_step_messages = inject_queued_user_messages(
            bridge.as_ref(),
            &mut state.visible_messages,
            crate::bridge::DeliveryCheckpoint::AfterCurrentOperation,
        )
        .await?;
        append_host_messages_to_recorded(&mut state.recorded_messages, &finish_step_messages);
        for message in &finish_step_messages {
            state.transcript_events.push(transcript_event(
                "host_input",
                "user",
                "public",
                &message.content,
                Some(serde_json::json!({"delivery": format!("{:?}", message.mode)})),
            ));
        }
        if !finish_step_messages.is_empty() {
            state.consecutive_text_only = 0;
            state.idle_backoff_ms = 100;
            continue;
        }

        state.consecutive_text_only += 1;
        if state.consecutive_text_only > max_nudges {
            state.final_status = "stuck";
            break;
        }

        // Silent continuation for short prose: when the model emits a
        // short text-only response (< 150 tokens, typically "thinking"
        // statements like "Let me check..."), don't inject a nudge. Just
        // loop back — the model sees its own text as the last assistant
        // message and naturally continues to act. This avoids polluting
        // context with nudge messages and the "nudge → rephrase → nudge"
        // loop seen with chatty models.
        //
        let nudge = action_turn_nudge(&tool_format, turn_policy.as_ref(), call_result.prose_too_long)
            .or_else(|| custom_nudge.clone())
            .unwrap_or_else(|| "Continue — use a tool call to make progress.".to_string());
        append_message_to_contexts(
            &mut state.visible_messages,
            &mut state.recorded_messages,
            runtime_feedback_message("nudge", nudge),
        );
    }

    finalize::run_finalize(
        &mut state,
        opts,
        bridge,
        daemon,
        &daemon_config,
        &tool_format,
        loop_start,
    )
    .await
}

#[cfg(test)]
mod tests;
