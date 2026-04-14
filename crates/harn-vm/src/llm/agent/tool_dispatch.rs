//! Tool-dispatch phase.
//!
//! Runs once per iteration after `run_llm_call` populates
//! `LlmCallResult.tool_calls`, but only when the list is non-empty.
//! The phase:
//!
//!   1. Appends the assistant turn message to the conversation
//!      (native or text-mode shape depending on `tool_format`).
//!   2. Builds a parallel-dispatch cache for the leading read-only
//!      prefix of tool calls via `join_all`, so read/lookup/search
//!      batches finish in parallel-latency time.
//!   3. For each tool call, sequentially:
//!        a. detects `__parse_error` sentinels from malformed provider
//!           arguments and rejects,
//!        b. enforces the current execution policy + arg constraints,
//!        c. runs the declarative approval policy (auto-approve /
//!           auto-deny / `session/request_permission` host bridge),
//!        d. runs in-process PreToolUse hooks (Allow / Deny / Modify),
//!        e. validates required arguments,
//!        f. emits `tool_intent` + `ToolCall` (Pending/InProgress)
//!           events and a `tool_call` tracing span,
//!        g. runs the loop-detect check (skip if stuck),
//!        h. dispatches the tool (replay fixture, parallel cache, or
//!           a fresh `dispatch_tool_execution`),
//!        i. tracks `run` exit codes for the verification gate,
//!        j. microcompacts oversized tool output,
//!        k. runs in-process PostToolUse hooks,
//!        l. emits a final `ToolCallUpdate` event + tracing span close,
//!        m. records to the tool-recording mock when active,
//!        n. runs loop-detect `record()` and optionally appends or
//!           replaces the result with a redirect hint,
//!        o. appends `tool_execution` transcript events and tool-result
//!           messages (or an observation line for text-mode).
//!   4. Returns `ToolDispatchResult` carrying `tools_used_this_iter`,
//!      `tool_results_this_iter`, the accumulated `observations`
//!      string, and the (currently unused) `rejection_followups`
//!      vector — the post-turn phase flushes these into the
//!      conversation before the next LLM call.

use std::rc::Rc;

use crate::agent_events::{AgentEvent, ToolCallStatus};
use crate::bridge::HostBridge;
use crate::value::{ErrorCategory, VmError, VmValue};

use super::super::helpers::transcript_event;
use super::super::tools::{
    build_assistant_tool_message, build_tool_result_message, collect_tool_schemas,
    normalize_tool_args, validate_tool_args,
};
use super::helpers::{append_message_to_contexts, assistant_history_text};
use super::llm_call::LlmCallResult;
use super::state::AgentLoopState;
use super::super::agent_tools::{
    classify_tool_mutation, declared_paths, denied_tool_result, dispatch_tool_execution,
    is_denied_tool_result, loop_intervention_message, render_tool_result, stable_hash,
    stable_hash_str, LoopIntervention,
};

pub(super) struct ToolDispatchContext<'a> {
    pub bridge: &'a Option<Rc<HostBridge>>,
    pub tool_format: &'a str,
    pub tools_val: Option<&'a VmValue>,
    pub tool_retries: usize,
    pub tool_backoff_ms: u64,
    pub loop_detect_enabled: bool,
    pub session_id: &'a str,
    pub iteration: usize,
    pub exit_when_verified: bool,
    pub auto_compact: &'a Option<crate::orchestration::AutoCompactConfig>,
}

pub(super) struct ToolDispatchResult {
    pub tools_used_this_iter: Vec<String>,
    pub tool_results_this_iter: Vec<serde_json::Value>,
    pub observations: String,
}

pub(super) async fn run_tool_dispatch(
    state: &mut AgentLoopState,
    opts: &mut super::super::api::LlmCallOptions,
    ctx: &ToolDispatchContext<'_>,
    call_result: &LlmCallResult,
) -> Result<ToolDispatchResult, VmError> {
    let tool_calls = &call_result.tool_calls;
    let text = &call_result.text;
    let iteration = ctx.iteration;

    state.consecutive_text_only = 0;
    state.idle_backoff_ms = 100;
    if ctx.tool_format == "native" {
        append_message_to_contexts(
            &mut state.visible_messages,
            &mut state.recorded_messages,
            build_assistant_tool_message(text, tool_calls, &opts.provider),
        );
    } else {
        let assistant_content_for_history = assistant_history_text(
            call_result.canonical_history.as_deref(),
            text,
            call_result.tool_parse_errors.len(),
            tool_calls,
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
    let mut tools_used_this_iter: Vec<String> = Vec::new();
    let mut tool_results_this_iter: Vec<serde_json::Value> = Vec::new();
    let tool_schemas = collect_tool_schemas(ctx.tools_val, opts.native_tools.as_deref());

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
    let mut parallel_results: std::collections::HashMap<usize, Result<serde_json::Value, VmError>> =
        std::collections::HashMap::new();
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
            let tool_retries_local = ctx.tool_retries;
            let tool_backoff_ms_local = ctx.tool_backoff_ms;
            let bridge_local = ctx.bridge.clone();
            let tools_val_local = ctx.tools_val.cloned();
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
            if ctx.tool_format == "native" {
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

        let policy_result = crate::orchestration::enforce_current_policy_for_tool(tool_name)
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
            if ctx.tool_format == "native" {
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
            None | Some(crate::orchestration::ToolApprovalDecision::AutoApproved) => Ok(None),
            Some(crate::orchestration::ToolApprovalDecision::AutoDenied { reason }) => {
                Err(("auto_denied", reason))
            }
            Some(crate::orchestration::ToolApprovalDecision::RequiresHostApproval) => {
                // Canonical ACP: request permission via
                // `session/request_permission`. Fail closed: if the
                // host does not implement the method or returns an
                // error, the tool is denied.
                if let Some(bridge) = ctx.bridge.as_ref() {
                    let mutation = crate::orchestration::current_mutation_session();
                    let payload = serde_json::json!({
                        "sessionId": ctx.session_id,
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
                                .or_else(|| response.get("outcome").and_then(|v| v.as_str()))
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
            if ctx.tool_format == "native" {
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
                let result_text = render_tool_result(&denied_tool_result(tool_name, reason));
                if !state.rejected_tools.contains(&tool_name.to_string()) {
                    state.rejected_tools.push(tool_name.to_string());
                }
                state.transcript_events.push(transcript_event(
                    "tool_execution",
                    "tool",
                    "internal",
                    &result_text,
                    Some(
                        serde_json::json!({"tool_name": tool_name, "tool_use_id": tool_id, "rejected": true}),
                    ),
                ));
                if ctx.tool_format == "native" {
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
            if ctx.tool_format == "native" {
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
            Some(serde_json::json!({"arguments": tool_args.clone(), "tool_use_id": tool_id})),
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
        let tool_kind = crate::orchestration::current_tool_annotations(tool_name).map(|a| a.kind);
        super::emit_agent_event(&AgentEvent::ToolCall {
            session_id: ctx.session_id.to_string(),
            tool_call_id: tool_call_id.clone(),
            tool_name: tool_name.to_string(),
            kind: tool_kind,
            status: ToolCallStatus::Pending,
            raw_input: tool_args.clone(),
        })
        .await;
        super::emit_agent_event(&AgentEvent::ToolCallUpdate {
            session_id: ctx.session_id.to_string(),
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
        crate::tracing::span_set_metadata(tool_span_id, "tool_name", serde_json::json!(tool_name));
        crate::tracing::span_set_metadata(tool_span_id, "tool_use_id", serde_json::json!(tool_id));
        crate::tracing::span_set_metadata(
            tool_span_id,
            "call_id",
            serde_json::json!(tool_call_id.clone()),
        );
        crate::tracing::span_set_metadata(tool_span_id, "iteration", serde_json::json!(iteration));
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
        let args_hash = if ctx.loop_detect_enabled {
            stable_hash(&tool_args)
        } else {
            0
        };
        if ctx.loop_detect_enabled {
            if let LoopIntervention::Skip { count } =
                state.loop_tracker.check(tool_name, args_hash)
            {
                // Skip execution entirely — the model is stuck.
                let skip_msg =
                    loop_intervention_message(tool_name, "", &LoopIntervention::Skip { count })
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
                if ctx.tool_format == "native" {
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
                super::emit_agent_event(&AgentEvent::ToolCallUpdate {
                    session_id: ctx.session_id.to_string(),
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
            let exec_result = if let Some(cached) = parallel_results.remove(&tc_index) {
                cached
            } else {
                dispatch_tool_execution(
                    tool_name,
                    &tool_args,
                    ctx.tools_val,
                    ctx.bridge.as_ref(),
                    ctx.tool_retries,
                    ctx.tool_backoff_ms,
                )
                .await
            };

            let rejected = matches!(
                &exec_result,
                Err(VmError::CategorizedError {
                    category: ErrorCategory::ToolRejected,
                    ..
                })
            ) || exec_result.as_ref().ok().is_some_and(is_denied_tool_result);
            let text = match &exec_result {
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
        if ctx.exit_when_verified && tool_name == "run" {
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
        let result_text = if let Some(ref ac) = ctx.auto_compact {
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
        let result_text = crate::orchestration::run_post_tool_hooks(tool_name, &result_text);

        // Emit a final ToolCallUpdate with the execution outcome
        // so pipeline subscribers can lint, audit, or inject
        // feedback. Result mutation is now a pipeline concern.
        super::emit_agent_event(&AgentEvent::ToolCallUpdate {
            session_id: ctx.session_id.to_string(),
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
        if crate::llm::mock::get_tool_recording_mode() == crate::llm::mock::ToolRecordingMode::Record
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
        let result_text = if ctx.loop_detect_enabled && !is_rejected {
            let result_hash = stable_hash_str(&result_text);
            let intervention = state.loop_tracker.record(tool_name, args_hash, result_hash);
            if let Some(msg) = loop_intervention_message(tool_name, &result_text, &intervention) {
                let (kind, count) = match &intervention {
                    LoopIntervention::Warn { count } => ("warn", *count),
                    LoopIntervention::Block { count } => ("block", *count),
                    LoopIntervention::Skip { count } => ("skip", *count),
                    LoopIntervention::Proceed => ("proceed", 0),
                };
                super::super::trace::emit_agent_event(
                    super::super::trace::AgentTraceEvent::LoopIntervention {
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
            super::super::trace::emit_agent_event(super::super::trace::AgentTraceEvent::ToolRejected {
                tool_name: tool_name.to_string(),
                reason: result_text.clone(),
                iteration,
            });
        } else {
            super::super::trace::emit_agent_event(
                super::super::trace::AgentTraceEvent::ToolExecution {
                    tool_name: tool_name.to_string(),
                    tool_use_id: tool_id.to_string(),
                    duration_ms: tool_start.elapsed().as_millis() as u64,
                    status: tool_status.to_string(),
                    classification: classify_tool_mutation(tool_name),
                    iteration,
                },
            );
        }

        if ctx.tool_format == "native" {
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

    Ok(ToolDispatchResult {
        tools_used_this_iter,
        tool_results_this_iter,
        observations,
    })
}
