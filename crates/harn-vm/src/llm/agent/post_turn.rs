//! Post-turn phase.
//!
//! Runs after every iteration, once the LLM call (and, when the
//! response carried tool calls, the dispatch phase) has produced its
//! results. Handles all the bookkeeping and control-flow decisions
//! that follow a turn — this is where the iteration loop actually
//! decides whether to continue or break.
//!
//! The phase branches on whether the turn produced tool calls:
//!
//! **Tool-call branch** (`dispatch.is_some()`):
//!   - extend `all_tools_used` with the per-turn tool names
//!   - append the aggregated `observations` string as a
//!     `tool_results` runtime-feedback message (text-mode only)
//!   - drain `AfterCurrentOperation` host messages and append
//!   - update `consecutive_single_tool_turns` + `successful_tools_used`
//!   - emit `AgentEvent::TurnEnd` so pipeline subscribers can react
//!   - `should_stop_after_successful_tools` → Break
//!   - optional `post_turn_callback` VM closure → Break when the
//!     callback signals stop, else inject a runtime message
//!   - auto-compaction when the message estimate exceeds the threshold
//!   - `parse_error` feedback for the mixed native+text case
//!   - sentinel_hit → Break (logs suppressed parse errors first),
//!     else Continue
//!
//! **Text-only branch** (`dispatch.is_none()`):
//!   - append the assistant turn to history
//!   - sentinel_hit → Break
//!   - parse errors → inject + Continue (also clears the errors so
//!     the next iteration's LLM call starts clean)
//!   - `!persistent && !daemon` → Break
//!   - daemon mode: snapshot idle state, wait on wake sources
//!     (bridge messages, watch paths, timer), inject the wake-reason
//!     feedback message, Continue
//!   - `AfterCurrentOperation` host messages → Continue
//!   - `consecutive_text_only > max_nudges` → final_status=stuck,
//!     Break
//!   - action-turn nudge (or custom nudge) injection, Continue

use std::rc::Rc;

use crate::agent_events::AgentEvent;
use crate::bridge::HostBridge;
use crate::orchestration::{AutoCompactConfig, TurnPolicy};
use crate::value::{VmError, VmValue};

use super::super::daemon::{detect_watch_changes, DaemonLoopConfig};
use super::super::helpers::transcript_event;
use super::helpers::{
    action_turn_nudge, append_host_messages_to_recorded, append_message_to_contexts,
    assistant_history_text, daemon_snapshot_from_state, inject_queued_user_messages,
    interpret_post_turn_callback_result, maybe_auto_compact_agent_messages,
    maybe_persist_daemon_snapshot, runtime_feedback_message, should_stop_after_successful_tools,
};
use super::llm_call::LlmCallResult;
use super::state::AgentLoopState;
use super::tool_dispatch::ToolDispatchResult;

pub(super) enum IterationOutcome {
    Continue,
    Break,
}

pub(super) struct PostTurnContext<'a> {
    pub bridge: &'a Option<Rc<HostBridge>>,
    pub session_id: &'a str,
    pub tool_format: &'a str,
    pub max_nudges: usize,
    pub persistent: bool,
    pub daemon: bool,
    pub turn_policy: Option<&'a TurnPolicy>,
    pub stop_after_successful_tools: &'a Option<Vec<String>>,
    pub post_turn_callback: &'a Option<VmValue>,
    pub auto_compact: &'a Option<AutoCompactConfig>,
    pub daemon_config: &'a DaemonLoopConfig,
    pub custom_nudge: &'a Option<String>,
    pub iteration: usize,
}

pub(super) async fn run_post_turn(
    state: &mut AgentLoopState,
    opts: &mut super::super::api::LlmCallOptions,
    ctx: &PostTurnContext<'_>,
    call_result: &mut LlmCallResult,
    dispatch: Option<ToolDispatchResult>,
) -> Result<IterationOutcome, VmError> {
    let iteration = ctx.iteration;

    if let Some(dispatch) = dispatch {
        state.all_tools_used.extend(dispatch.tools_used_this_iter);
        if ctx.tool_format != "native" && !dispatch.observations.is_empty() {
            append_message_to_contexts(
                &mut state.visible_messages,
                &mut state.recorded_messages,
                runtime_feedback_message("tool_results", dispatch.observations.trim_end()),
            );
        }
        let finish_step_messages = inject_queued_user_messages(
            ctx.bridge.as_ref(),
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

        if call_result.tool_calls.len() == 1 {
            state.consecutive_single_tool_turns += 1;
        } else {
            state.consecutive_single_tool_turns = 0;
        }
        let successful_tool_names: Vec<&str> = dispatch
            .tool_results_this_iter
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
        let tool_names: Vec<&str> = call_result
            .tool_calls
            .iter()
            .filter_map(|tc| tc["name"].as_str())
            .collect();
        let turn_info = serde_json::json!({
            "tool_names": tool_names,
            "tool_results": dispatch.tool_results_this_iter,
            "successful_tool_names": successful_tool_names,
            "tool_count": call_result.tool_calls.len(),
            "iteration": iteration,
            "consecutive_single_tool_turns": state.consecutive_single_tool_turns,
            "session_tools_used": state.all_tools_used,
            "session_successful_tools": state.successful_tools_used,
        });
        super::emit_agent_event(&AgentEvent::TurnEnd {
            session_id: ctx.session_id.to_string(),
            iteration,
            turn_info: turn_info.clone(),
        })
        .await;
        if let Some(stop_tools) = ctx.stop_after_successful_tools.as_ref() {
            if should_stop_after_successful_tools(&dispatch.tool_results_this_iter, stop_tools) {
                crate::events::log_debug(
                    "agent.stop_after_successful_tools",
                    &format!("iter={iteration} requested stage stop after successful tool turn"),
                );
                return Ok(IterationOutcome::Break);
            }
        }
        // post_turn_callback returns: ""/nil (no-op), string (inject as
        // feedback), true (stop), or {message, stop} (dict for both).
        if let Some(VmValue::Closure(closure)) = ctx.post_turn_callback.as_ref() {
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
                    let feedback = runtime_feedback_message("post_turn_callback", msg.as_str());
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
                return Ok(IterationOutcome::Break);
            }
        }

        // Include system prompt + tool defs in the estimate since they
        // consume context window alongside messages.
        if let Some(ref ac) = ctx.auto_compact {
            let mut est = crate::orchestration::estimate_message_tokens(&state.visible_messages);
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
                    super::super::trace::emit_agent_event(
                        super::super::trace::AgentTraceEvent::ContextCompaction {
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
                            if !existing.trim().is_empty() && existing.trim() != summary.trim() =>
                        {
                            format!("{existing}\n\n{summary}")
                        }
                        Some(_) | None => summary,
                    };
                    state.transcript_summary = Some(merged);
                }
            }
        }

        // Surface parse errors in mixed turns too; otherwise rejected
        // calls silently vanish from the conversation.
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
            return Ok(IterationOutcome::Break);
        }
        return Ok(IterationOutcome::Continue);
    }

    if call_result.sentinel_hit {
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
        return Ok(IterationOutcome::Break);
    }

    // Send parse diagnostics so the model fixes its syntax instead of
    // being silently nudged.
    if !call_result.tool_parse_errors.is_empty() {
        let error_msg = call_result.tool_parse_errors.join("\n\n");
        append_message_to_contexts(
            &mut state.visible_messages,
            &mut state.recorded_messages,
            runtime_feedback_message("parse_error", error_msg),
        );
        call_result.tool_parse_errors.clear();
        state.consecutive_text_only = 0;
        return Ok(IterationOutcome::Continue);
    }

    let action_required_before_answer = ctx.tool_format == "native"
        && ctx
            .turn_policy
            .is_some_and(|policy| policy.require_action_or_yield)
        && state.all_tools_used.is_empty();
    if action_required_before_answer {
        state.consecutive_text_only += 1;
        if state.consecutive_text_only > ctx.max_nudges {
            state.final_status = "stuck";
            let tail_excerpt = {
                let raw = call_result.text.trim();
                if raw.chars().count() > 240 {
                    let truncated: String = raw.chars().take(240).collect();
                    format!("{truncated}…")
                } else {
                    raw.to_string()
                }
            };
            super::emit_agent_event(&AgentEvent::LoopStuck {
                session_id: ctx.session_id.to_string(),
                max_nudges: ctx.max_nudges,
                last_iteration: iteration,
                tail_excerpt,
            })
            .await;
            return Ok(IterationOutcome::Break);
        }
        let guidance =
            action_turn_nudge(ctx.tool_format, ctx.turn_policy, call_result.prose_too_long)
                .unwrap_or_else(|| "Use a tool call to make progress.".to_string());
        append_message_to_contexts(
            &mut state.visible_messages,
            &mut state.recorded_messages,
            runtime_feedback_message(
                "action_required",
                format!(
                    "You returned assistant text/JSON before using any tool. \
                     This stage requires at least one tool action before an answer counts. \
                     That response was not accepted. {guidance}"
                ),
            ),
        );
        return Ok(IterationOutcome::Continue);
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

    if !ctx.persistent && !ctx.daemon {
        return Ok(IterationOutcome::Break);
    }

    // Daemon idle: notify host and wait briefly for user messages.
    if ctx.daemon && !ctx.persistent {
        state.daemon_state = "idle".to_string();
        if ctx.daemon_config.consolidate_on_idle {
            maybe_auto_compact_agent_messages(
                opts,
                ctx.auto_compact,
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
            maybe_persist_daemon_snapshot(ctx.daemon_config, &idle_snapshot)?
                .or(state.daemon_snapshot_path.take());
        if !ctx.daemon_config.has_wake_source(ctx.bridge.is_some()) {
            state.final_status = "idle";
            return Ok(IterationOutcome::Break);
        }
        let watchdog_limit = ctx.daemon_config.idle_watchdog_attempts;
        let watchdog_started = std::time::Instant::now();
        let mut idle_null_attempts: usize = 0;
        loop {
            if let Some(bridge) = ctx.bridge.as_ref() {
                bridge.notify(
                    "agent/idle",
                    serde_json::json!({
                        "iteration": state.total_iterations,
                        "backoff_ms": state.idle_backoff_ms,
                        "persist_path": state.daemon_snapshot_path,
                        "watch_paths": ctx.daemon_config.watch_paths,
                    }),
                );
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(
                ctx.daemon_config.idle_wait_ms(state.idle_backoff_ms),
            ))
            .await;
            let resumed = ctx
                .bridge
                .as_ref()
                .is_some_and(|bridge| bridge.take_resume_signal());
            let idle_messages = inject_queued_user_messages(
                ctx.bridge.as_ref(),
                &mut state.visible_messages,
                crate::bridge::DeliveryCheckpoint::InterruptImmediate,
            )
            .await?;
            append_host_messages_to_recorded(&mut state.recorded_messages, &idle_messages);
            let changed_paths = if ctx.daemon_config.watch_paths.is_empty() {
                Vec::new()
            } else {
                detect_watch_changes(
                    &ctx.daemon_config.watch_paths,
                    &mut state.daemon_watch_state,
                )
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
            } else if ctx.daemon_config.wake_interval_ms.is_some() {
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
            idle_null_attempts += 1;
            if let Some(limit) = watchdog_limit {
                if idle_null_attempts >= limit {
                    let elapsed_ms = watchdog_started.elapsed().as_millis() as u64;
                    super::emit_agent_event(&AgentEvent::DaemonWatchdogTripped {
                        session_id: ctx.session_id.to_string(),
                        attempts: idle_null_attempts,
                        elapsed_ms,
                    })
                    .await;
                    state.final_status = "watchdog";
                    return Ok(IterationOutcome::Break);
                }
            }
            ctx.daemon_config
                .update_idle_backoff(&mut state.idle_backoff_ms);
        }
        return Ok(IterationOutcome::Continue);
    }

    let finish_step_messages = inject_queued_user_messages(
        ctx.bridge.as_ref(),
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
        return Ok(IterationOutcome::Continue);
    }

    state.consecutive_text_only += 1;
    if state.consecutive_text_only > ctx.max_nudges {
        state.final_status = "stuck";
        let tail_excerpt = {
            let raw = call_result.text.trim();
            if raw.chars().count() > 240 {
                let truncated: String = raw.chars().take(240).collect();
                format!("{truncated}…")
            } else {
                raw.to_string()
            }
        };
        super::emit_agent_event(&AgentEvent::LoopStuck {
            session_id: ctx.session_id.to_string(),
            max_nudges: ctx.max_nudges,
            last_iteration: iteration,
            tail_excerpt,
        })
        .await;
        return Ok(IterationOutcome::Break);
    }

    let nudge = action_turn_nudge(ctx.tool_format, ctx.turn_policy, call_result.prose_too_long)
        .or_else(|| ctx.custom_nudge.clone())
        .unwrap_or_else(|| "Continue — use a tool call to make progress.".to_string());
    append_message_to_contexts(
        &mut state.visible_messages,
        &mut state.recorded_messages,
        runtime_feedback_message("nudge", nudge),
    );
    Ok(IterationOutcome::Continue)
}
