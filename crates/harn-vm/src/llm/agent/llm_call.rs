//! LLM-call phase.
//!
//! Runs once per iteration, between `turn_preflight` and
//! `tool_dispatch`:
//!
//!   1. Invoke the provider via `observed_llm_call` (retries + tracing).
//!   2. Append `provider_payload` + optional `private_reasoning`
//!      transcript events.
//!   3. Parse the response through the tagged-prose / text-tool-call
//!      parser. Extract `text_prose`, protocol violations, the
//!      `<done>` marker body, and the canonical tool-call history
//!      string.
//!   4. Promote parsed text-mode tool calls into `tool_calls` (even
//!      in native-tool-call mode, when the provider fell back to
//!      text-mode calls).
//!   5. Inject `parse_guidance` feedback when tool-call-shaped text
//!      failed to parse.
//!   6. Inject `protocol_violation` feedback for tagged-response
//!      grammar errors.
//!   7. Compute `sentinel_hit` (done sentinel + verification-gate +
//!      ledger-gate + has_acted), injecting corrective feedback for
//!      each gate the model violated.
//!   8. Intercept `ledger(...)` tool calls — apply to the task ledger,
//!      emit synthetic tool-result messages, drop them from the
//!      dispatch list.
//!
//! Produces `LlmCallResult`, carrying every value the later phases
//! need (tool_calls, sentinel_hit, shaped prose, parse errors, etc.).
//! The phase never breaks the outer iteration loop directly — all
//! control flow decisions live in `post_turn`.

use std::rc::Rc;

use crate::bridge::HostBridge;
use crate::value::VmError;

use crate::orchestration::TurnPolicy;

use super::super::agent_observe::{
    dump_llm_interpreted_response, observed_llm_call, LlmRetryConfig,
};
use super::super::helpers::transcript_event;
use super::super::tools::parse_text_tool_calls_with_tools;
use super::helpers::{
    append_message_to_contexts, loop_state_requests_phase_change, prose_exceeds_budget,
    runtime_feedback_message, sentinel_without_action_nudge, trim_prose_for_history,
};
use super::state::AgentLoopState;

/// Phase-local handles the LLM-call step reads from the outer scope.
pub(super) struct LlmCallContext<'a> {
    pub bridge: &'a Option<Rc<HostBridge>>,
    pub tool_format: &'a str,
    pub done_sentinel: &'a str,
    pub break_unless_phase: Option<&'a str>,
    pub exit_when_verified: bool,
    pub persistent: bool,
    pub has_tools: bool,
    pub turn_policy: Option<&'a TurnPolicy>,
    pub llm_retries: usize,
    pub llm_backoff_ms: u64,
    pub tools_val: Option<&'a crate::value::VmValue>,
}

/// Values produced by the LLM-call phase that later phases read.
pub(super) struct LlmCallResult {
    pub text: String,
    pub tool_calls: Vec<serde_json::Value>,
    pub tool_parse_errors: Vec<String>,
    pub canonical_history: Option<String>,
    pub prose_too_long: bool,
    pub sentinel_hit: bool,
}

pub(super) async fn run_llm_call(
    state: &mut AgentLoopState,
    opts: &mut super::super::api::LlmCallOptions,
    ctx: &LlmCallContext<'_>,
    iteration: usize,
) -> Result<LlmCallResult, VmError> {
    let result = observed_llm_call(
        opts,
        Some(ctx.tool_format),
        ctx.bridge.as_ref(),
        &LlmRetryConfig {
            retries: ctx.llm_retries,
            backoff_ms: ctx.llm_backoff_ms,
        },
        Some(iteration),
        true,
        false, // agent_loop runs on the local set, not offthread
    )
    .await?;

    // Consume the prefill after the call. The provider appended it to
    // the outgoing request as a final `role: "assistant"` message; the
    // returned `text` is only the model's continuation, so we prepend
    // the prefill here before downstream parsing so the logical
    // assistant turn is whole. Clearing `opts.prefill` after the call
    // keeps the injection scoped to exactly one turn.
    let prefill = opts.prefill.take();
    let text = match prefill.as_ref() {
        Some(prefix) if !result.text.starts_with(prefix.as_str()) => {
            format!("{prefix}{}", result.text)
        }
        _ => result.text.clone(),
    };
    state.total_text.push_str(&text);
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
            "tool_calling_mode": ctx.tool_format,
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
    // Always run the tagged parser so `<assistant_prose>`/`<done>` tags
    // never leak to callers. The tool-call gate below controls only
    // whether parsed TS-expression calls are promoted into `tool_calls`.
    let (text_prose, protocol_violations, tagged_done_marker, canonical_history) = {
        let parse_result = parse_text_tool_calls_with_tools(&text, ctx.tools_val);
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
    let tool_calls: Vec<serde_json::Value> = if !result.tool_calls.is_empty() {
        result.tool_calls.clone()
    } else if ctx.has_tools {
        let parse_result = parse_text_tool_calls_with_tools(&text, ctx.tools_val);
        tool_parse_errors = parse_result.errors;
        // Honor text-mode tool calls regardless of tool_format: models
        // served via Ollama/OpenAI-compat often fall back to emitting
        // `<tool_call>...</tool_call>` even when the request advertised
        // a native `tools` channel (the model's chat template doesn't
        // actually route through the native function-call surface).
        // Discarding them would strand a legitimate call mid-loop.
        if ctx.tool_format == "native" && !parse_result.calls.is_empty() {
            crate::events::log_info(
                "llm.tool",
                "native-mode stage accepted text-mode tool calls (model fell back to text)",
            );
        }
        {
            let calls = parse_result.calls;

            // Emit feedback on ANY parse error so mixed-batch cases
            // (some calls parsed, some didn't) still signal the model.
            if !tool_parse_errors.is_empty() {
                let error_summary = tool_parse_errors
                    .iter()
                    .take(2)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("; ");
                crate::events::log_warn(
                    "llm.tool",
                    &format!(
                        "{} tool-call parse error(s): {} (parsed_calls={})",
                        tool_parse_errors.len(),
                        &error_summary[..error_summary.len().min(200)],
                        calls.len(),
                    ),
                );
                let partial_note = if calls.is_empty() {
                    String::new()
                } else {
                    format!(
                        "\n\n(The other {} tool call(s) in this turn parsed \
                         successfully and were dispatched; the errors above \
                         describe only the malformed ones, which were dropped.)",
                        calls.len()
                    )
                };
                let feedback = format!(
                    "Your tool call could not be parsed: {error_summary}{partial_note}\n\n\
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
    let prose_too_long = prose_exceeds_budget(&text_prose, ctx.turn_policy);
    let shaped_text_prose = trim_prose_for_history(&text_prose, ctx.turn_policy);
    let interpreted_call_id = format!("iteration-{iteration}");
    dump_llm_interpreted_response(
        iteration,
        &interpreted_call_id,
        ctx.tool_format,
        &shaped_text_prose,
        &tool_calls,
        &tool_parse_errors,
    );
    // Append the `<done>` wrapper so post-turn callbacks can substring-
    // match the sentinel; visible-text sanitization strips it downstream.
    state.last_iteration_text = match tagged_done_marker.as_deref() {
        Some(body) if shaped_text_prose.trim().is_empty() => {
            format!("<done>{body}</done>")
        }
        Some(body) => format!("{shaped_text_prose}\n\n<done>{body}</done>"),
        None => shaped_text_prose.clone(),
    };

    // Teach the model to fix grammar violations. Done before tool-call
    // dispatch so protocol errors surface even in mixed turns.
    if !protocol_violations.is_empty() && ctx.tool_format != "native" {
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
            done_sentinel = ctx.done_sentinel,
        );
        append_message_to_contexts(
            &mut state.visible_messages,
            &mut state.recorded_messages,
            runtime_feedback_message("protocol_violation", feedback),
        );
    }

    // Check sentinel on every response; when it coexists with tool calls
    // we still process the tools, then exit.
    let sentinel_in_text = tagged_done_marker
        .as_deref()
        .is_some_and(|body| body == ctx.done_sentinel)
        // Native-format and no-tools paths bypass the tagged parser —
        // fall back to substring match.
        || (ctx.tool_format == "native" && text.contains(ctx.done_sentinel))
        || (!ctx.has_tools && text.contains(ctx.done_sentinel));
    let phase_change = ctx
        .break_unless_phase
        .is_some_and(|phase| loop_state_requests_phase_change(&text, phase));
    if phase_change {
        if let Some(phase) = ctx.break_unless_phase {
            super::super::trace::emit_agent_event(
                super::super::trace::AgentTraceEvent::PhaseChange {
                    from_phase: phase.to_string(),
                    to_phase: text
                        .lines()
                        .rev()
                        .find_map(|l| l.trim().strip_prefix("next_phase:"))
                        .unwrap_or("")
                        .trim()
                        .to_string(),
                    iteration,
                },
            );
        }
    }
    // exit_when_verified: honor sentinel only if the last run() exit 0.
    let verified = !ctx.exit_when_verified || state.last_run_exit_code == Some(0);
    // Guard against premature exit where the model emits done without acting.
    let has_acted = !state.all_tools_used.is_empty() || !tool_calls.is_empty();
    // Ledger gate: reject done while open/blocked deliverables remain.
    let ledger_blocks_done = state.task_ledger.gates_done();
    let sentinel_hit = ctx.persistent
        && ((sentinel_in_text && verified && has_acted && !ledger_blocks_done) || phase_change);

    if sentinel_in_text && ledger_blocks_done && ctx.persistent {
        let corrective = state.task_ledger.done_gate_feedback();
        append_message_to_contexts(
            &mut state.visible_messages,
            &mut state.recorded_messages,
            runtime_feedback_message("ledger_not_satisfied", corrective),
        );
        state.ledger_done_rejections += 1;
    }

    if sentinel_in_text && !verified && ctx.persistent {
        let code_str = state
            .last_run_exit_code
            .map_or("none".to_string(), |c| c.to_string());
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
    if sentinel_in_text && !has_acted && ctx.persistent && ctx.has_tools {
        let corrective = sentinel_without_action_nudge(ctx.tool_format, ctx.turn_policy);
        append_message_to_contexts(
            &mut state.visible_messages,
            &mut state.recorded_messages,
            runtime_feedback_message("sentinel_without_action", corrective),
        );
    }

    // Intercept `ledger(...)` before normal dispatch — it mutates
    // task_ledger state and has no host executor.
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
        append_message_to_contexts(
            &mut state.visible_messages,
            &mut state.recorded_messages,
            message,
        );
    }

    Ok(LlmCallResult {
        text,
        tool_calls,
        tool_parse_errors,
        canonical_history,
        prose_too_long,
        sentinel_hit,
    })
}
