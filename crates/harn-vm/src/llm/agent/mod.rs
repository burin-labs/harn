use std::rc::Rc;

use crate::agent_events::{self, AgentEvent, ToolCallStatus};
use crate::value::{ErrorCategory, VmError, VmValue};

use super::daemon::detect_watch_changes;
use super::helpers::transcript_event;
use super::tools::{
    build_assistant_tool_message, build_tool_result_message, collect_tool_schemas,
    normalize_tool_args, parse_text_tool_calls_with_tools, validate_tool_args,
};

// Imports from extracted submodules.
use super::agent_config::AgentLoopConfig;
use super::agent_observe::{dump_llm_interpreted_response, observed_llm_call, LlmRetryConfig};
use super::agent_tools::{
    classify_tool_mutation, declared_paths, denied_tool_result, dispatch_tool_execution,
    is_denied_tool_result, loop_intervention_message, render_tool_result, stable_hash,
    stable_hash_str, LoopIntervention,
};

mod finalize;
mod helpers;
mod state;
mod turn_preflight;

use helpers::{
    action_turn_nudge, append_host_messages_to_recorded, append_message_to_contexts,
    assistant_history_text, daemon_snapshot_from_state, has_successful_tools,
    inject_queued_user_messages, loop_state_requests_phase_change,
    maybe_auto_compact_agent_messages, maybe_persist_daemon_snapshot, prose_exceeds_budget,
    runtime_feedback_message, sentinel_without_action_nudge, should_stop_after_successful_tools,
    trim_prose_for_history,
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
        q.borrow_mut()
            .push((session_id.to_string(), kind.to_string(), content.to_string()))
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
        let result = observed_llm_call(
            opts,
            Some(&tool_format),
            bridge.as_ref(),
            &LlmRetryConfig {
                retries: llm_retries,
                backoff_ms: llm_backoff_ms,
            },
            Some(iteration),
            true,
            false, // agent_loop runs on the local set, not offthread
        )
        .await?;

        let text = result.text.clone();
        state.total_text.push_str(&text);
        // `last_iteration_text` is assigned below AFTER the tool-call parser
        // runs, so it holds the prose (calls stripped) rather than the raw
        // text. For the native-tool-call and no-tools branches we fall back
        // to the raw text a few lines down.
        state.transcript_events.push(transcript_event(
            "provider_payload",
            "assistant",
            "internal",
            "",
            Some(serde_json::json!({
                "model": result.model,
                "input_tokens": result.input_tokens,
                "output_tokens": result.output_tokens,
                "tool_calls": result.tool_calls,
                "tool_calling_mode": tool_format.clone(),
            })),
        ));
        if let Some(thinking) = result.thinking.clone() {
            if !thinking.is_empty() {
                state.transcript_events.push(transcript_event(
                    "private_reasoning",
                    "assistant",
                    "private",
                    &thinking,
                    None,
                ));
            }
        }

        let mut tool_parse_errors: Vec<String> = Vec::new();
        // `text_prose` is the content of `<assistant_prose>` blocks under
        // the tagged response protocol (concatenated with blank-line joins).
        // Always run the tagged parser — even with no tools or native-tool
        // provider channel — so `visible_text` is the unwrapped prose and
        // `<done>` / `<assistant_prose>` tags never leak to callers that
        // just want the model's answer. The tool-call gate below controls
        // only whether parsed TS-expression calls are promoted into
        // `tool_calls`, not whether the parser runs.
        let (text_prose, protocol_violations, tagged_done_marker, canonical_history) = {
            let parse_result = parse_text_tool_calls_with_tools(&text, tools_val);
            let prose = if parse_result.prose.is_empty() {
                text.clone()
            } else {
                parse_result.prose.clone()
            };
            let canonical = if parse_result.canonical.is_empty() {
                None
            } else {
                Some(parse_result.canonical)
            };
            (
                prose,
                parse_result.violations,
                parse_result.done_marker,
                canonical,
            )
        };
        let tool_calls = if !result.tool_calls.is_empty() {
            result.tool_calls.clone()
        } else if has_tools {
            let parse_result = parse_text_tool_calls_with_tools(&text, tools_val);
            tool_parse_errors = parse_result.errors;
            // Text-mode tool calls are the universal protocol every model is
            // expected to follow. Honor them regardless of `tool_format` —
            // many models served via Ollama / OpenAI-compat fall back to
            // emitting `<tool_call>name({...})</tool_call>` in text even
            // when the request advertised a native `tools` channel, because
            // the model's chat template doesn't actually route through the
            // host's native function-call surface. Discarding those would
            // strand a legitimate tool call mid-loop. When in native mode
            // and the model still chose text, log a hint so we can spot
            // mis-tuned aliases later, but execute the call.
            if tool_format == "native" && !parse_result.calls.is_empty() {
                crate::events::log_info(
                    "llm.tool",
                    "native-mode stage accepted text-mode tool calls (model fell back to text)",
                );
            }
            {
                let calls = parse_result.calls;

                // When the parser found tool-call-looking text but couldn't
                // parse it, inject the specific parse error into the conversation
                // so the model knows what to fix (e.g. unescaped backtick inside
                // a template literal). Without this, the generic nudge gives the
                // model no signal about what was wrong.
                if calls.is_empty() && !tool_parse_errors.is_empty() {
                    let error_summary = tool_parse_errors
                        .iter()
                        .take(2)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join("; ");
                    crate::events::log_warn(
                        "llm.tool",
                        &format!(
                            "{} tool-call parse error(s): {}",
                            tool_parse_errors.len(),
                            &error_summary[..error_summary.len().min(200)]
                        ),
                    );
                    let feedback = format!(
                        "Your tool call could not be parsed: {error_summary}\n\n\
                         Use heredoc syntax for multiline content — it requires NO escaping:\n\
                         edit({{\n\
                             action: \"create\",\n\
                             path: \"...\",\n\
                             content: <<EOF\n\
                         package main\n\
                         // backticks, quotes, backslashes — all fine inside heredoc\n\
                         EOF\n\
                         }})\n\n\
                         Do NOT use backtick template literals for code that contains \
                         backtick characters (Go raw strings, Rust raw strings, shell). \
                         Heredoc avoids all escaping issues."
                    );
                    append_message_to_contexts(
                        &mut state.visible_messages,
                        &mut state.recorded_messages,
                        runtime_feedback_message("parse_guidance", feedback),
                    );
                }
                calls
            }
        } else {
            Vec::new()
        };
        let prose_too_long = prose_exceeds_budget(&text_prose, turn_policy.as_ref());
        let shaped_text_prose = trim_prose_for_history(&text_prose, turn_policy.as_ref());
        let interpreted_call_id = format!("iteration-{iteration}");
        dump_llm_interpreted_response(
            iteration,
            &interpreted_call_id,
            &tool_format,
            &shaped_text_prose,
            &tool_calls,
            &tool_parse_errors,
        );
        // Surface the prose (not the raw text) to callers that read
        // `last_iteration_text` / `visible_text`. Tool call expressions are
        // structured data in `tool_calls`, not something the user should
        // see as the agent's "answer". When the model emitted a `<done>`
        // block, append its canonical wrapper so post-turn callbacks can
        // substring-match the configured sentinel without the UI showing
        // it (visible-text sanitization strips `<done>` blocks downstream).
        state.last_iteration_text = match tagged_done_marker.as_deref() {
            Some(body) if shaped_text_prose.trim().is_empty() => {
                format!("<done>{body}</done>")
            }
            Some(body) => format!("{shaped_text_prose}\n\n<done>{body}</done>"),
            None => shaped_text_prose.clone(),
        };

        // Inject structured feedback for any tagged-protocol violations.
        // The response-protocol parser enforces top-level grammar; these
        // feedbacks teach the model how to fix its shape before the next
        // turn. Done first so protocol errors surface even when tool-call
        // dispatch still happens (e.g. calls parsed inside tags plus stray
        // prose outside).
        if !protocol_violations.is_empty() && tool_format != "native" {
            let feedback = format!(
                "Your response violated the tagged response protocol. Each issue:\n- {}\n\n\
                 Re-emit using only these top-level tags, separated by whitespace:\n\n\
                 <assistant_prose>short narration (optional)</assistant_prose>\n\
                 <tool_call>\nname({{ key: value }})\n</tool_call>\n\
                 <done>{done_sentinel}</done>\n\n\
                 Nothing outside these tags is accepted. Do not paste source code, \
                 diffs, JSON, or command output as prose — wrap each action in its \
                 own <tool_call> block.",
                protocol_violations.join("\n- "),
            );
            append_message_to_contexts(
                &mut state.visible_messages,
                &mut state.recorded_messages,
                runtime_feedback_message("protocol_violation", feedback),
            );
        }

        // Check done_sentinel on EVERY response, not just text-only ones.
        // If present alongside tool calls, we still process the tools (so their
        // results land in the conversation), but mark the loop to exit afterward.
        let sentinel_in_text = tagged_done_marker
            .as_deref()
            .is_some_and(|body| body == done_sentinel.as_str())
            // Native-format and no-tools paths bypass the tagged parser;
            // fall back to substring match so their transcripts still honour
            // the configured sentinel.
            || (tool_format == "native" && text.contains(done_sentinel.as_str()))
            || (!has_tools && text.contains(done_sentinel.as_str()));
        let phase_change = break_unless_phase
            .as_deref()
            .is_some_and(|phase| loop_state_requests_phase_change(&text, phase));
        if phase_change {
            if let Some(ref phase) = break_unless_phase {
                super::trace::emit_agent_event(super::trace::AgentTraceEvent::PhaseChange {
                    from_phase: phase.clone(),
                    to_phase: text
                        .lines()
                        .rev()
                        .find_map(|l| l.trim().strip_prefix("next_phase:"))
                        .unwrap_or("")
                        .trim()
                        .to_string(),
                    iteration,
                });
            }
        }
        // When exit_when_verified is set, the sentinel is only honoured if the
        // last run() tool call returned exit code 0.  This prevents premature
        // exit when the model claims it's done but verification hasn't passed.
        let verified = !exit_when_verified || state.last_run_exit_code == Some(0);
        // Guard: the model must have made at least one tool call before the
        // done sentinel is honoured.  This prevents premature exits where the
        // model describes a plan and emits ##DONE## without actually acting.
        let has_acted = !state.all_tools_used.is_empty() || !tool_calls.is_empty();
        // Ledger gate: if a task ledger was seeded and still has open or
        // blocked deliverables, the done sentinel is rejected. This is
        // the general-purpose "what does the user call done?" mechanism
        // that replaces ad-hoc task-specific guardrails. See
        // `llm/ledger.rs` for the structured semantics.
        let ledger_blocks_done = state.task_ledger.gates_done();
        let sentinel_hit = persistent
            && ((sentinel_in_text && verified && has_acted && !ledger_blocks_done) || phase_change);

        if sentinel_in_text && ledger_blocks_done && persistent {
            let corrective = state.task_ledger.done_gate_feedback();
            append_message_to_contexts(
                &mut state.visible_messages,
                &mut state.recorded_messages,
                runtime_feedback_message("ledger_not_satisfied", corrective),
            );
            state.ledger_done_rejections += 1;
        }

        // If the model emitted the sentinel but verification hasn't passed,
        // inject a corrective so the model knows it must keep going.
        if sentinel_in_text && !verified && persistent {
            let code_str = state.last_run_exit_code.map_or("none".to_string(), |c| c.to_string());
            let corrective = format!(
                "You emitted the done sentinel but verification has not passed \
                 (last run exit code: {code_str}). The loop will continue. \
                 Run the verification command and fix any failures before finishing."
            );
            append_message_to_contexts(
                &mut state.visible_messages,
                &mut state.recorded_messages,
                runtime_feedback_message("verification_gate", corrective),
            );
        }
        // If the model emitted the sentinel without having made any tool
        // calls, it's trying to declare done without doing any work.
        if sentinel_in_text && !has_acted && persistent && has_tools {
            let corrective =
                sentinel_without_action_nudge(&tool_format, turn_policy.as_ref());
            append_message_to_contexts(
                &mut state.visible_messages,
                &mut state.recorded_messages,
                runtime_feedback_message("sentinel_without_action", corrective),
            );
        }

        // Intercept `ledger(...)` tool calls before the normal dispatch
        // pipeline. The ledger tool has no host executor — it mutates
        // runtime state (task_ledger) and emits a synthetic tool-result
        // message. Filtering here lets the rest of the pipeline stay
        // unaware of ledger bookkeeping.
        let mut tool_calls: Vec<serde_json::Value> = tool_calls;
        let mut ledger_tool_results: Vec<serde_json::Value> = Vec::new();
        tool_calls.retain(|tc| {
            if tc.get("name").and_then(|n| n.as_str()) != Some("ledger") {
                return true;
            }
            let call_id = tc
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("ledger_call")
                .to_string();
            let args = tc
                .get("arguments")
                .cloned()
                .unwrap_or(serde_json::json!({}));
            let result_text = match state.task_ledger.apply(&args) {
                Ok(summary) => {
                    state.all_tools_used.push("ledger".to_string());
                    state.successful_tools_used.push("ledger".to_string());
                    format!("<tool_result>ledger: {summary}</tool_result>")
                }
                Err(err) => format!("<tool_result>ledger error: {err}</tool_result>"),
            };
            ledger_tool_results.push(serde_json::json!({
                "role": "user",
                "content": result_text,
                "metadata": {
                    "tool_call_id": call_id,
                    "tool_name": "ledger",
                },
            }));
            false
        });
        for message in ledger_tool_results.drain(..) {
            append_message_to_contexts(&mut state.visible_messages, &mut state.recorded_messages, message);
        }

        if !tool_calls.is_empty() {
            state.consecutive_text_only = 0;
            state.idle_backoff_ms = 100;
            if tool_format == "native" {
                append_message_to_contexts(
                    &mut state.visible_messages,
                    &mut state.recorded_messages,
                    build_assistant_tool_message(&text, &tool_calls, &opts.provider),
                );
            } else {
                let assistant_content_for_history = assistant_history_text(
                    canonical_history.as_deref(),
                    &text,
                    tool_parse_errors.len(),
                    &tool_calls,
                );
                append_message_to_contexts(
                    &mut state.visible_messages,
                    &mut state.recorded_messages,
                    serde_json::json!({
                        "role": "assistant",
                        "content": assistant_content_for_history,
                    }),
                );
            }

            let mut observations = String::new();
            let mut tools_used_this_iter = Vec::new();
            let mut tool_results_this_iter: Vec<serde_json::Value> = Vec::new();
            let rejection_followups: Vec<String> = Vec::new();
            let tool_schemas = collect_tool_schemas(tools_val, opts.native_tools.as_deref());

            // Parallel dispatch for read-only exploration batches. When the
            // leading run of tool calls in this assistant response are all in
            // the read-only set (read, lookup, search, outline, list_templates,
            // get_template, web_search, web_fetch), we concurrently pre-fetch
            // their execution results via join_all. This covers two cases:
            //   (a) all tools read-only — entire turn runs in parallel latency
            //   (b) mixed turn starting with reads — the read-only prefix
            //       runs in parallel, then sequential dispatch handles the
            //       non-read-only tail (edit, run, etc.) as before.
            //
            // The sequential loop still runs for ALL bookkeeping (policy
            // checks, hooks, transcript events, observation appending,
            // post-hooks, ordering) — only the actual tool-execution step is
            // parallelized, and only for tools whose index is in the cache.
            // Any hook denial or arg mutation falls through to the sequential
            // path, which safely recomputes that single call.
            // A tool is eligible for concurrent dispatch iff its declared
            // ToolKind is read-only. Unannotated tools are conservatively
            // treated as NOT read-only (fail-safe default).
            let ro_prefix_len: usize = tool_calls
                .iter()
                .position(|tc| {
                    let name = tc["name"].as_str().unwrap_or("");
                    !crate::orchestration::current_tool_annotations(name)
                        .map(|a| a.kind.is_read_only())
                        .unwrap_or(false)
                })
                .unwrap_or(tool_calls.len());
            let parallel_indices: Vec<usize> = if ro_prefix_len >= 2 {
                (0..ro_prefix_len).collect()
            } else {
                Vec::new()
            };
            let mut parallel_results: std::collections::HashMap<
                usize,
                Result<serde_json::Value, VmError>,
            > = std::collections::HashMap::new();
            if !parallel_indices.is_empty() {
                // Build futures for each read-only execution. We use the raw
                // tool_args here (pre-hook); if a hook would modify or deny,
                // the sequential loop will still run its full checks and
                // choose to either reuse our result (if hooks are Allow with
                // no modifications) or recompute. This is safe because we
                // only cache results for read-only tools which have no side
                // effects — re-running them is at worst wasted work.
                use futures::future::join_all;
                let futures = parallel_indices.iter().map(|&idx| {
                    let tc = tool_calls[idx].clone();
                    let tool_name = tc["name"].as_str().unwrap_or("").to_string();
                    let tool_args = normalize_tool_args(&tool_name, &tc["arguments"]);
                    let tool_retries_local = tool_retries;
                    let tool_backoff_ms_local = tool_backoff_ms;
                    let bridge_local = bridge.clone();
                    let tools_val_local = tools_val.cloned();
                    async move {
                        dispatch_tool_execution(
                            &tool_name,
                            &tool_args,
                            tools_val_local.as_ref(),
                            bridge_local.as_ref(),
                            tool_retries_local,
                            tool_backoff_ms_local,
                        )
                        .await
                    }
                });
                let joined: Vec<Result<serde_json::Value, VmError>> = join_all(futures).await;
                for (i, idx) in parallel_indices.iter().enumerate() {
                    parallel_results.insert(*idx, joined[i].clone());
                }
            }

            for (tc_index, tc) in tool_calls.iter().enumerate() {
                let tool_id = tc["id"].as_str().unwrap_or("");
                let tool_name = tc["name"].as_str().unwrap_or("");
                let mut tool_args = normalize_tool_args(tool_name, &tc["arguments"]);

                // Detect malformed JSON arguments that the provider returned
                // (marked with __parse_error sentinel during response parsing).
                if let Some(parse_err) = tool_args.get("__parse_error").and_then(|v| v.as_str()) {
                    let result_text = format!("ERROR: {parse_err}");
                    state.transcript_events.push(transcript_event(
                        "tool_execution",
                        "tool",
                        "internal",
                        &result_text,
                        Some(serde_json::json!({
                            "tool_name": tool_name,
                            "tool_use_id": tool_id,
                            "rejected": true,
                        })),
                    ));
                    if tool_format == "native" {
                        append_message_to_contexts(
                            &mut state.visible_messages,
                            &mut state.recorded_messages,
                            build_tool_result_message(tool_id, &result_text, &opts.provider),
                        );
                    } else {
                        observations.push_str(&format!(
                            "[result of {tool_name}]\n{result_text}\n[end of {tool_name} result]\n\n"
                        ));
                    }
                    continue;
                }

                let policy_result = crate::orchestration::enforce_current_policy_for_tool(
                    tool_name,
                )
                .and_then(|_| {
                    crate::orchestration::enforce_tool_arg_constraints(
                        &crate::orchestration::current_execution_policy().unwrap_or_default(),
                        tool_name,
                        &tool_args,
                    )
                });
                if let Err(error) = policy_result {
                    let result_text = render_tool_result(&denied_tool_result(
                        tool_name,
                        format!(
                            "{error}. Use one of the declared tools exactly as named and put extra fields inside that tool's arguments."
                        ),
                    ));
                    if !state.rejected_tools.contains(&tool_name.to_string()) {
                        state.rejected_tools.push(tool_name.to_string());
                    }
                    state.transcript_events.push(transcript_event(
                        "tool_execution",
                        "tool",
                        "internal",
                        &result_text,
                        Some(serde_json::json!({
                            "tool_name": tool_name,
                            "tool_use_id": tool_id,
                            "rejected": true,
                            "arguments": tool_args.clone(),
                        })),
                    ));
                    if tool_format == "native" {
                        append_message_to_contexts(
                            &mut state.visible_messages,
                            &mut state.recorded_messages,
                            build_tool_result_message(tool_id, &result_text, &opts.provider),
                        );
                    } else {
                        observations.push_str(&format!(
                            "[result of {tool_name}]\n{result_text}\n[end of {tool_name} result]\n\n"
                        ));
                    }
                    continue;
                }

                // Declarative approval policy: auto-approve / auto-deny / require host.
                let approval_decision = crate::orchestration::current_approval_policy()
                    .map(|policy| policy.evaluate(tool_name, &tool_args));
                let approval_outcome = match approval_decision {
                    None | Some(crate::orchestration::ToolApprovalDecision::AutoApproved) => {
                        Ok(None)
                    }
                    Some(crate::orchestration::ToolApprovalDecision::AutoDenied { reason }) => {
                        Err(("auto_denied", reason))
                    }
                    Some(crate::orchestration::ToolApprovalDecision::RequiresHostApproval) => {
                        // Canonical ACP: request permission via
                        // `session/request_permission`. Fail closed: if the
                        // host does not implement the method or returns an
                        // error, the tool is denied.
                        if let Some(bridge) = bridge.as_ref() {
                            let mutation = crate::orchestration::current_mutation_session();
                            let payload = serde_json::json!({
                                "sessionId": session_id,
                                "toolCall": {
                                    "toolCallId": tool_id,
                                    "toolName": tool_name,
                                    "rawInput": tool_args,
                                },
                                "mutation": mutation,
                                "declaredPaths": declared_paths(tool_name, &tool_args),
                            });
                            match bridge.call("session/request_permission", payload).await {
                                Ok(response) => {
                                    let outcome = response
                                        .get("outcome")
                                        .and_then(|v| v.get("outcome"))
                                        .and_then(|v| v.as_str())
                                        .or_else(|| {
                                            response.get("outcome").and_then(|v| v.as_str())
                                        })
                                        .unwrap_or("");
                                    let granted = matches!(outcome, "selected" | "allow")
                                        || response
                                            .get("granted")
                                            .and_then(|v| v.as_bool())
                                            .unwrap_or(false);
                                    if granted {
                                        if let Some(new_args) = response.get("args") {
                                            tool_args = new_args.clone();
                                        }
                                        Ok(Some("host_granted"))
                                    } else {
                                        let reason = response
                                            .get("reason")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("host did not grant approval")
                                            .to_string();
                                        Err(("host_denied", reason))
                                    }
                                }
                                Err(_) => Err((
                                    "host_denied",
                                    "approval request failed or host does not implement \
                                     session/request_permission"
                                        .to_string(),
                                )),
                            }
                        } else {
                            Err((
                                "host_denied",
                                "approval required but no host bridge is available".to_string(),
                            ))
                        }
                    }
                };
                if let Err((approval_status, reason)) = approval_outcome {
                    let result_text = render_tool_result(&denied_tool_result(tool_name, reason));
                    if !state.rejected_tools.contains(&tool_name.to_string()) {
                        state.rejected_tools.push(tool_name.to_string());
                    }
                    state.transcript_events.push(transcript_event(
                        "tool_execution",
                        "tool",
                        "internal",
                        &result_text,
                        Some(serde_json::json!({
                            "tool_name": tool_name,
                            "tool_use_id": tool_id,
                            "rejected": true,
                            "arguments": tool_args.clone(),
                            "approval": approval_status,
                        })),
                    ));
                    if tool_format == "native" {
                        append_message_to_contexts(
                            &mut state.visible_messages,
                            &mut state.recorded_messages,
                            build_tool_result_message(tool_id, &result_text, &opts.provider),
                        );
                    } else {
                        observations.push_str(&format!(
                            "[result of {tool_name}]\n{result_text}\n[end of {tool_name} result]\n\n"
                        ));
                    }
                    continue;
                }
                if let Ok(Some(approval_status)) = approval_outcome {
                    state.transcript_events.push(transcript_event(
                        "tool_execution",
                        "tool",
                        "internal",
                        "",
                        Some(serde_json::json!({
                            "tool_name": tool_name,
                            "tool_use_id": tool_id,
                            "approval": approval_status,
                        })),
                    ));
                }

                // PreToolUse hooks: in-process hooks first, then bridge gate
                match crate::orchestration::run_pre_tool_hooks(tool_name, &tool_args) {
                    crate::orchestration::PreToolAction::Allow => {}
                    crate::orchestration::PreToolAction::Deny(reason) => {
                        let result_text =
                            render_tool_result(&denied_tool_result(tool_name, reason));
                        if !state.rejected_tools.contains(&tool_name.to_string()) {
                            state.rejected_tools.push(tool_name.to_string());
                        }
                        state.transcript_events.push(transcript_event(
                            "tool_execution", "tool", "internal", &result_text,
                            Some(serde_json::json!({"tool_name": tool_name, "tool_use_id": tool_id, "rejected": true})),
                        ));
                        if tool_format == "native" {
                            append_message_to_contexts(
                                &mut state.visible_messages,
                                &mut state.recorded_messages,
                                build_tool_result_message(tool_id, &result_text, &opts.provider),
                            );
                        } else {
                            observations.push_str(&format!(
                                "[result of {tool_name}]\n{result_text}\n[end of {tool_name} result]\n\n"
                            ));
                        }
                        continue;
                    }
                    crate::orchestration::PreToolAction::Modify(new_args) => {
                        tool_args = new_args;
                    }
                }

                // Arg rewriting is now a pipeline concern — it happens
                // inside the tool's handler (or via `arg_aliases` in the
                // tool's ToolAnnotations). The VM no longer provides a
                // pre-dispatch mutation hook.

                // Bridge-level PreToolUse gate has been retired. The canonical
                // ACP observation surface is the AgentEvent stream — external
                // sinks (e.g. the harn-cli ACP server) receive `ToolCall`
                // events and translate them into `session/update` with the
                // `tool_call` variant. Hosts that need to block a call use
                // `session/request_permission` (see the declarative approval
                // policy above).
                // Validate required parameters before dispatch so the LLM gets
                // a clear error instead of a cryptic handler failure.
                if let Err(msg) = validate_tool_args(tool_name, &tool_args, &tool_schemas) {
                    let result_text = format!("ERROR: {msg}");
                    state.transcript_events.push(transcript_event(
                        "tool_execution",
                        "tool",
                        "internal",
                        &result_text,
                        Some(serde_json::json!({
                            "tool_name": tool_name,
                            "tool_use_id": tool_id,
                            "rejected": true,
                            "arguments": tool_args.clone(),
                        })),
                    ));
                    if tool_format == "native" {
                        append_message_to_contexts(
                            &mut state.visible_messages,
                            &mut state.recorded_messages,
                            build_tool_result_message(tool_id, &result_text, &opts.provider),
                        );
                    } else {
                        observations.push_str(&format!(
                            "[result of {tool_name}]\n{result_text}\n[end of {tool_name} result]\n\n"
                        ));
                    }
                    continue;
                }

                state.transcript_events.push(transcript_event(
                    "tool_intent",
                    "assistant",
                    "internal",
                    tool_name,
                    Some(
                        serde_json::json!({"arguments": tool_args.clone(), "tool_use_id": tool_id}),
                    ),
                ));
                tools_used_this_iter.push(tool_name.to_string());
                let mutation_classification = classify_tool_mutation(tool_name);
                let declared_paths_current = declared_paths(tool_name, &tool_args);
                let tool_started_at = std::time::Instant::now();
                let tool_call_id = if tool_id.is_empty() {
                    format!("tool-iter-{iteration}-{}", tools_used_this_iter.len())
                } else {
                    format!("tool-{tool_id}")
                };
                // Emit a Pending ToolCall event so pipeline subscribers
                // and the ACP server can observe the dispatch before it
                // starts. Status transitions (InProgress, Completed,
                // Failed) follow via ToolCallUpdate.
                let tool_kind = crate::orchestration::current_tool_annotations(tool_name)
                    .map(|a| a.kind);
                emit_agent_event(&AgentEvent::ToolCall {
                    session_id: session_id.clone(),
                    tool_call_id: tool_call_id.clone(),
                    tool_name: tool_name.to_string(),
                    kind: tool_kind,
                    status: ToolCallStatus::Pending,
                    raw_input: tool_args.clone(),
                })
                .await;
                emit_agent_event(&AgentEvent::ToolCallUpdate {
                    session_id: session_id.clone(),
                    tool_call_id: tool_call_id.clone(),
                    tool_name: tool_name.to_string(),
                    status: ToolCallStatus::InProgress,
                    raw_output: None,
                    error: None,
                })
                .await;
                let tool_span_id = crate::tracing::span_start(
                    crate::tracing::SpanKind::ToolCall,
                    tool_name.to_string(),
                );
                crate::tracing::span_set_metadata(
                    tool_span_id,
                    "tool_name",
                    serde_json::json!(tool_name),
                );
                crate::tracing::span_set_metadata(
                    tool_span_id,
                    "tool_use_id",
                    serde_json::json!(tool_id),
                );
                crate::tracing::span_set_metadata(
                    tool_span_id,
                    "call_id",
                    serde_json::json!(tool_call_id.clone()),
                );
                crate::tracing::span_set_metadata(
                    tool_span_id,
                    "iteration",
                    serde_json::json!(iteration),
                );
                crate::tracing::span_set_metadata(
                    tool_span_id,
                    "classification",
                    serde_json::json!(mutation_classification.clone()),
                );
                crate::tracing::span_set_metadata(
                    tool_span_id,
                    "declared_paths",
                    serde_json::json!(declared_paths_current.clone()),
                );
                // Tool-call observability is now carried by the AgentEvent
                // stream (`ToolCall` + `ToolCallUpdate`); the ACP server
                // consumes those via `AgentEventSink` and emits canonical
                // `session/update` notifications. No direct bridge call here.

                // Tool loop detection: check BEFORE dispatch whether this
                // exact call has been stuck in a loop.
                let args_hash = if loop_detect_enabled {
                    stable_hash(&tool_args)
                } else {
                    0
                };
                if loop_detect_enabled {
                    if let LoopIntervention::Skip { count } =
                        state.loop_tracker.check(tool_name, args_hash)
                    {
                        // Skip execution entirely — the model is stuck.
                        let skip_msg = loop_intervention_message(
                            tool_name,
                            "",
                            &LoopIntervention::Skip { count },
                        )
                        .unwrap_or_default();
                        state.transcript_events.push(transcript_event(
                            "tool_execution",
                            "tool",
                            "internal",
                            &skip_msg,
                            Some(serde_json::json!({
                                "tool_name": tool_name,
                                "tool_use_id": tool_id,
                                "loop_skipped": true,
                                "repeat_count": count,
                            })),
                        ));
                        if tool_format == "native" {
                            append_message_to_contexts(
                                &mut state.visible_messages,
                                &mut state.recorded_messages,
                                build_tool_result_message(tool_id, &skip_msg, &opts.provider),
                            );
                        } else {
                            observations.push_str(&format!(
                                "[result of {tool_name}]\n{skip_msg}\n[end of {tool_name} result]\n\n"
                            ));
                        }
                        crate::tracing::span_end(tool_span_id);
                        // Surface the loop-skip as a ToolCallUpdate so
                        // external sinks still see a completion event.
                        emit_agent_event(&AgentEvent::ToolCallUpdate {
                            session_id: session_id.clone(),
                            tool_call_id: tool_call_id.clone(),
                            tool_name: tool_name.to_string(),
                            status: ToolCallStatus::Failed,
                            raw_output: Some(serde_json::json!({
                                "loop_skipped": true,
                                "repeat_count": count,
                            })),
                            error: Some(format!("tool loop detected (skipped after {count} repeats)")),
                        })
                        .await;
                        continue;
                    }
                }

                // Tool replay: if replay mode is active, try to use a
                // recorded fixture instead of executing the tool.
                let replay_hit = if crate::llm::mock::get_tool_recording_mode()
                    == crate::llm::mock::ToolRecordingMode::Replay
                {
                    crate::llm::mock::find_tool_replay_fixture(tool_name, &tool_args)
                } else {
                    None
                };

                let tool_start = std::time::Instant::now();
                let (is_rejected, result_text) = if let Some(fixture) = replay_hit {
                    (fixture.is_rejected, fixture.result.clone())
                } else {
                    // Prefer a pre-computed result from the parallel pre-fetch
                    // pass above, when available.
                    let call_result = if let Some(cached) = parallel_results.remove(&tc_index) {
                        cached
                    } else {
                        dispatch_tool_execution(
                            tool_name,
                            &tool_args,
                            tools_val,
                            bridge.as_ref(),
                            tool_retries,
                            tool_backoff_ms,
                        )
                        .await
                    };

                    let rejected =
                        matches!(
                            &call_result,
                            Err(VmError::CategorizedError {
                                category: ErrorCategory::ToolRejected,
                                ..
                            })
                        ) || call_result.as_ref().ok().is_some_and(is_denied_tool_result);
                    let text = match &call_result {
                        Ok(val) => render_tool_result(val),
                        Err(VmError::CategorizedError {
                            message,
                            category: ErrorCategory::ToolRejected,
                        }) => render_tool_result(&denied_tool_result(
                            tool_name,
                            format!("{message} Do not retry this tool."),
                        )),
                        Err(error) => format!("Error: {error}"),
                    };
                    (rejected, text)
                };

                if is_rejected && !state.rejected_tools.contains(&tool_name.to_string()) {
                    state.rejected_tools.push(tool_name.to_string());
                }

                // Track run() exit codes for verification-gated exit.
                // The host bridge formats run results with "exit_code=N" or
                // "Command succeeded"/"Command failed" markers.
                if exit_when_verified && tool_name == "run" {
                    if result_text.contains("exit_code=0")
                        || result_text.contains("Command succeeded")
                        || result_text.contains("success=true")
                    {
                        state.last_run_exit_code = Some(0);
                    } else if result_text.contains("Command failed")
                        || result_text.contains("success=false")
                        || result_text.contains("exit_code=")
                    {
                        state.last_run_exit_code = Some(1);
                    }
                }

                // Microcompaction: compress oversized tool outputs
                let result_text = if let Some(ref ac) = auto_compact {
                    if result_text.len() > ac.tool_output_max_chars {
                        if let Some(ref cb) = ac.compress_callback {
                            crate::orchestration::invoke_compress_callback(
                                cb,
                                tool_name,
                                &result_text,
                                ac.tool_output_max_chars,
                            )
                            .await
                        } else {
                            crate::orchestration::microcompact_tool_output(
                                &result_text,
                                ac.tool_output_max_chars,
                            )
                        }
                    } else {
                        result_text
                    }
                } else {
                    result_text
                };
                crate::tracing::span_set_metadata(
                    tool_span_id,
                    "status",
                    serde_json::json!(if is_rejected { "rejected" } else { "ok" }),
                );
                crate::tracing::span_set_metadata(
                    tool_span_id,
                    "result_chars",
                    serde_json::json!(result_text.len()),
                );

                // PostToolUse hooks (in-process)
                let result_text =
                    crate::orchestration::run_post_tool_hooks(tool_name, &result_text);

                // Emit a final ToolCallUpdate with the execution outcome
                // so pipeline subscribers can lint, audit, or inject
                // feedback. Result mutation is now a pipeline concern.
                emit_agent_event(&AgentEvent::ToolCallUpdate {
                    session_id: session_id.clone(),
                    tool_call_id: tool_call_id.clone(),
                    tool_name: tool_name.to_string(),
                    status: if is_rejected {
                        ToolCallStatus::Failed
                    } else {
                        ToolCallStatus::Completed
                    },
                    raw_output: Some(serde_json::json!({
                        "text": result_text,
                        "tool_use_id": tool_id,
                    })),
                    error: if is_rejected {
                        Some(result_text.clone())
                    } else {
                        None
                    },
                })
                .await;

                // Bridge-level PostToolUse gate has been retired. Hosts that
                // need to observe or mutate a tool result subscribe to the
                // AgentEvent stream — `ToolCallUpdate` events carry the
                // outcome. Pipeline-side closure subscribers can still
                // inject feedback via `agent_inject_feedback`.
                // The terminal ToolCallUpdate event above already carries
                // the final status; no direct bridge.send_call_end is
                // needed. Sinks consume the event stream instead.
                crate::tracing::span_end(tool_span_id);

                // Record tool call result if recording mode is active.
                if crate::llm::mock::get_tool_recording_mode()
                    == crate::llm::mock::ToolRecordingMode::Record
                {
                    crate::llm::mock::record_tool_call(crate::orchestration::ToolCallRecord {
                        tool_name: tool_name.to_string(),
                        tool_use_id: tool_call_id.clone(),
                        args_hash: crate::orchestration::tool_fixture_hash(tool_name, &tool_args),
                        result: result_text.clone(),
                        is_rejected,
                        duration_ms: tool_started_at.elapsed().as_millis() as u64,
                        iteration,
                        timestamp: crate::orchestration::now_rfc3339(),
                    });
                }

                // Tool loop detection: record the result and check for
                // repeated identical outcomes.  If we detect a loop,
                // append a redirection hint or replace the result.
                let result_text = if loop_detect_enabled && !is_rejected {
                    let result_hash = stable_hash_str(&result_text);
                    let intervention = state.loop_tracker.record(tool_name, args_hash, result_hash);
                    if let Some(msg) =
                        loop_intervention_message(tool_name, &result_text, &intervention)
                    {
                        let (kind, count) = match &intervention {
                            LoopIntervention::Warn { count } => ("warn", *count),
                            LoopIntervention::Block { count } => ("block", *count),
                            LoopIntervention::Skip { count } => ("skip", *count),
                            LoopIntervention::Proceed => ("proceed", 0),
                        };
                        super::trace::emit_agent_event(
                            super::trace::AgentTraceEvent::LoopIntervention {
                                tool_name: tool_name.to_string(),
                                kind: kind.to_string(),
                                count,
                                iteration,
                            },
                        );
                        match intervention {
                            LoopIntervention::Warn { .. } => {
                                // Append hint after the real result
                                format!("{result_text}{msg}")
                            }
                            LoopIntervention::Block { .. } => {
                                // Replace the result entirely with the redirect
                                msg
                            }
                            _ => result_text,
                        }
                    } else {
                        result_text
                    }
                } else {
                    result_text
                };
                let tool_status = if is_rejected {
                    "rejected"
                } else if result_text.starts_with("Error:") || result_text.starts_with("ERROR:") {
                    "error"
                } else {
                    "ok"
                };

                tool_results_this_iter.push(serde_json::json!({
                    "tool_name": tool_name,
                    "status": tool_status,
                    "rejected": is_rejected,
                }));

                state.transcript_events.push(transcript_event(
                    "tool_execution",
                    "tool",
                    "internal",
                    &result_text,
                    Some(serde_json::json!({
                        "tool_name": tool_name,
                        "tool_use_id": tool_id,
                        "rejected": is_rejected,
                    })),
                ));

                if is_rejected {
                    super::trace::emit_agent_event(super::trace::AgentTraceEvent::ToolRejected {
                        tool_name: tool_name.to_string(),
                        reason: result_text.clone(),
                        iteration,
                    });
                } else {
                    super::trace::emit_agent_event(super::trace::AgentTraceEvent::ToolExecution {
                        tool_name: tool_name.to_string(),
                        tool_use_id: tool_id.to_string(),
                        duration_ms: tool_start.elapsed().as_millis() as u64,
                        status: tool_status.to_string(),
                        classification: classify_tool_mutation(tool_name),
                        iteration,
                    });
                }

                if tool_format == "native" {
                    append_message_to_contexts(
                        &mut state.visible_messages,
                        &mut state.recorded_messages,
                        build_tool_result_message(tool_id, &result_text, &opts.provider),
                    );
                } else {
                    observations.push_str(&format!(
                        "[result of {tool_name}]\n{result_text}\n[end of {tool_name} result]\n\n"
                    ));
                }
            }

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
            if tool_calls.len() == 1 {
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
                if !state.successful_tools_used
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
            {
                let tool_names: Vec<&str> = tool_calls
                    .iter()
                    .filter_map(|tc| tc["name"].as_str())
                    .collect();
                let turn_info = serde_json::json!({
                    "tool_names": tool_names,
                    "tool_results": tool_results_this_iter,
                    "successful_tool_names": successful_tool_names,
                    "tool_count": tool_calls.len(),
                    "iteration": iteration,
                    "consecutive_single_tool_turns": state.consecutive_single_tool_turns,
                    "session_tools_used": state.all_tools_used,
                    "session_successful_tools": state.successful_tools_used,
                });
                emit_agent_event(&AgentEvent::TurnEnd {
                    session_id: session_id.clone(),
                    iteration,
                    turn_info,
                })
                .await;
            }
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

            // Auto-compaction check after tool processing.
            // Include the system prompt + tool definitions in the estimate
            // since they consume context window alongside messages.
            if let Some(ref ac) = auto_compact {
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
            if !tool_parse_errors.is_empty() {
                let error_msg = tool_parse_errors.join("\n\n");
                append_message_to_contexts(
                    &mut state.visible_messages,
                    &mut state.recorded_messages,
                    runtime_feedback_message("parse_error", error_msg),
                );
            }
            if sentinel_hit {
                if !tool_parse_errors.is_empty() {
                    crate::events::log_warn(
                        "llm.tool",
                        &format!(
                            "{} tool-call parse error(s) suppressed by sentinel: {}",
                            tool_parse_errors.len(),
                            tool_parse_errors.join("; ")
                        ),
                    );
                }
                break;
            }
            continue;
        }

        let assistant_content_for_history = assistant_history_text(
            canonical_history.as_deref(),
            &text,
            tool_parse_errors.len(),
            &tool_calls,
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
        if sentinel_hit {
            break;
        }

        // If the model attempted tool calls but parsing failed, send diagnostics
        // back so it can fix its syntax instead of being silently nudged.
        if !tool_parse_errors.is_empty() {
            let error_msg = tool_parse_errors.join("\n\n");
            append_message_to_contexts(
                &mut state.visible_messages,
                &mut state.recorded_messages,
                runtime_feedback_message("parse_error", error_msg),
            );
            tool_parse_errors.clear();
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
            state.daemon_snapshot_path = maybe_persist_daemon_snapshot(&daemon_config, &idle_snapshot)?
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
        let nudge = action_turn_nudge(&tool_format, turn_policy.as_ref(), prose_too_long)
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
