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

use crate::agent_events::{AgentEvent, ToolCallStatus, ToolExecutor};
use crate::bridge::HostBridge;
use crate::tool_annotations::ToolKind;
use crate::value::VmError;

use crate::orchestration::TurnPolicy;

use super::super::agent_observe::{
    dump_llm_interpreted_response, observed_llm_call, LlmRetryConfig, StreamingDetectorContext,
};
use super::super::helpers::{expects_structured_output, transcript_event};
use super::super::tools::{collect_tool_schemas, parse_text_tool_calls_with_tools};
use super::helpers::{
    append_message_to_contexts, loop_state_requests_phase_change, prose_exceeds_budget,
    runtime_feedback_message, sentinel_without_action_nudge, trim_prose_for_history,
};
use super::state::AgentLoopState;

/// Phase-local handles the LLM-call step reads from the outer scope.
pub(super) struct LlmCallContext<'a> {
    pub bridge: &'a Option<Rc<HostBridge>>,
    pub tool_format: &'a str,
    pub native_tool_fallback: crate::orchestration::NativeToolFallbackPolicy,
    pub done_sentinel: &'a str,
    pub break_unless_phase: Option<&'a str>,
    pub exit_when_verified: bool,
    pub persistent: bool,
    pub has_tools: bool,
    pub turn_policy: Option<&'a TurnPolicy>,
    pub llm_retries: usize,
    pub llm_backoff_ms: u64,
    pub schema_retries: usize,
    pub schema_retry_nudge: &'a super::super::SchemaNudge,
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
    pub input_tokens: i64,
    pub output_tokens: i64,
}

pub(super) async fn run_llm_call(
    state: &mut AgentLoopState,
    opts: &mut super::super::api::LlmCallOptions,
    ctx: &LlmCallContext<'_>,
    iteration: usize,
) -> Result<LlmCallResult, VmError> {
    // Streaming text-mode candidate detector (harn#692). Only relevant
    // when this stage actually parses text-mode tool calls — native
    // tool-channel calls have their own server-side streaming path
    // (harn#693 / H4 in the parent epic) so spinning up a text
    // detector for them would surface false-positive candidates from
    // any tool-shaped narration the model emits before its native
    // tool block.
    let mut streaming_detector = if ctx.has_tools && ctx.tool_format != "native" {
        let mut known: std::collections::BTreeSet<String> =
            collect_tool_schemas(ctx.tools_val, None)
                .into_iter()
                .map(|schema| schema.name)
                .collect();
        // Mirror the post-stream parsers' pseudo-tool registration.
        known.insert("ledger".to_string());
        known.insert("load_skill".to_string());
        Some(StreamingDetectorContext {
            session_id: state.session_id.clone(),
            known_tools: known,
        })
    } else {
        None
    };
    // Forward the agent session id into the LLM-call options so the SSE
    // transport can emit `AgentEvent::ToolCall` / `AgentEvent::ToolCallUpdate`
    // for streaming native tool-call deltas (#693). Without this, the
    // transport has no session to attribute streaming partial-arg events
    // to and falls back to the dispatch-time lifecycle only.
    opts.session_id = Some(state.session_id.clone());
    let retry_config = LlmRetryConfig {
        retries: ctx.llm_retries,
        backoff_ms: ctx.llm_backoff_ms,
    };
    let mut schema_attempt = 0usize;
    let result = loop {
        let detector = if schema_attempt == 0 {
            streaming_detector.take()
        } else {
            None
        };
        let result = observed_llm_call(
            opts,
            Some(ctx.tool_format),
            ctx.bridge.as_ref(),
            &retry_config,
            Some(iteration),
            true,
            false, // agent_loop runs on the local set, not offthread
            detector,
        )
        .await?;

        if schema_attempt >= ctx.schema_retries || !expects_structured_output(opts) {
            break result;
        }

        let vm_result = super::super::agent_config::build_llm_call_result(&result, opts);
        let errors = super::super::structured_output_errors(&vm_result, opts);
        if errors.is_empty() {
            break result;
        }

        schema_attempt += 1;
        let nudge = super::super::build_schema_nudge(
            &errors,
            opts.output_schema.as_ref(),
            ctx.schema_retry_nudge,
        );
        super::super::trace::emit_agent_event(super::super::trace::AgentTraceEvent::SchemaRetry {
            attempt: schema_attempt,
            errors: errors.clone(),
            nudge_used: !nudge.is_empty(),
            correction_prompt: nudge.clone(),
        });
        if !nudge.is_empty() {
            append_message_to_contexts(
                &mut state.visible_messages,
                &mut state.recorded_messages,
                runtime_feedback_message("schema_retry", nudge),
            );
        }
        opts.messages = state.visible_messages.clone();
    };

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
            "thinking_summary": result.thinking_summary,
            "tool_calling_mode": ctx.tool_format,
            "structural_experiment": opts.applied_structural_experiment.as_ref(),
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
    if let Some(summary) = result.thinking_summary.clone() {
        if !summary.is_empty() {
            state.transcript_events.push(transcript_event(
                "thinking_summary",
                "assistant",
                "private",
                &summary,
                None,
            ));
        }
    }

    // Surface provider-native `tool_search` events (harn#71 Anthropic +
    // OpenAI Responses). The response parser records these as blocks on
    // `result.blocks`, but ACP sinks and `transcript_events(...)` live
    // on the AgentEvent + state.transcript_events paths — mirror the
    // client-path emission shape so downstream consumers can't tell the
    // two apart. `mode` is set to the provider family so IDEs rendering
    // a Tool Vault chip can distinguish server-hosted search from the
    // in-process fallback.
    let native_search_mode =
        if crate::llm::helpers::ResolvedProvider::resolve(&result.provider).is_anthropic_style {
            "anthropic"
        } else {
            "openai"
        };
    let native_emissions =
        provider_native_search_emissions(&result.blocks, &state.session_id, native_search_mode);
    for transcript in native_emissions.transcript_events {
        state.transcript_events.push(transcript);
    }
    for event in native_emissions.agent_events {
        super::emit_agent_event(&event).await;
    }

    let mut tool_parse_errors: Vec<String> = Vec::new();
    // Always run the tagged parser so `<assistant_prose>`/`<done>` tags
    // never leak to callers. The tool-call gate below controls only
    // whether parsed TS-expression calls are promoted into `tool_calls`.
    let (text_prose, user_response, protocol_violations, tagged_done_marker, canonical_history) = {
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
            parse_result.user_response,
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
        {
            let mut calls = parse_result.calls;
            let parsed_call_count = calls.len();
            if ctx.tool_format == "native" && !calls.is_empty() {
                state.native_text_tool_fallbacks += 1;
                let fallback_index = state.native_text_tool_fallbacks;
                let accepted = match ctx.native_tool_fallback {
                    crate::orchestration::NativeToolFallbackPolicy::Allow => true,
                    crate::orchestration::NativeToolFallbackPolicy::AllowOnce => {
                        fallback_index == 1
                    }
                    crate::orchestration::NativeToolFallbackPolicy::Reject => false,
                };
                if accepted {
                    crate::events::log_info(
                        "llm.tool",
                        "native-mode stage accepted text-mode tool calls (model fell back to text)",
                    );
                } else {
                    state.native_text_tool_fallback_rejections += 1;
                    crate::events::log_warn(
                        "llm.tool",
                        &format!(
                            "native-mode stage rejected text-mode tool calls (policy={}, fallback_index={fallback_index})",
                            ctx.native_tool_fallback.as_str(),
                        ),
                    );
                    let feedback = format!(
                        "This stage is running in native tool mode. Your last response emitted text-mode tool calls instead of provider-native tool calls.\n\n\
                         Re-issue the same action using ONLY the native tool channel. Do not write `<tool_call>` tags, bare `name({{ ... }})` calls, Markdown fences, or JSON tool-call envelopes in assistant text.\n\n\
                         Policy: `{}`. Observed fallback turn: {}.",
                        ctx.native_tool_fallback.as_str(),
                        fallback_index,
                    );
                    append_message_to_contexts(
                        &mut state.visible_messages,
                        &mut state.recorded_messages,
                        runtime_feedback_message("native_tool_contract", feedback),
                    );
                    calls.clear();
                }
                state.transcript_events.push(transcript_event(
                    "native_tool_fallback",
                    "assistant",
                    "internal",
                    "",
                    Some(serde_json::json!({
                        "accepted": accepted,
                        "policy": ctx.native_tool_fallback.as_str(),
                        "fallback_index": fallback_index,
                        "tool_call_count": parsed_call_count,
                        "tool_parse_error_count": tool_parse_errors.len(),
                    })),
                ));
                super::super::trace::emit_agent_event(
                    super::super::trace::AgentTraceEvent::NativeToolFallback {
                        iteration,
                        accepted,
                        policy: ctx.native_tool_fallback.as_str().to_string(),
                        fallback_index,
                        tool_call_count: parsed_call_count,
                    },
                );
            }

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
    if !protocol_violations.is_empty() && ctx.has_tools && ctx.tool_format != "native" {
        let feedback = format!(
            "Your response violated the tagged response protocol. Each issue:\n- {}\n\n\
             Re-emit using only these top-level tags, separated by whitespace:\n\n\
             <assistant_prose>short narration (optional)</assistant_prose>\n\
             <user_response>final user-facing answer (optional)</user_response>\n\
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
    let tagged_done_hit = tagged_done_marker
        .as_deref()
        .is_some_and(|body| body == ctx.done_sentinel);
    let plain_done_hit = if ctx.tool_format == "native" {
        // Native-mode providers may surface the sentinel in visible prose
        // while tool calls travel separately via the provider channel.
        text.contains(ctx.done_sentinel)
    } else if !ctx.has_tools {
        // No-tool loops advertise the plain sentinel form, not a tagged
        // `<done>` block. Honor only visible prose so tagged control
        // blocks do not terminate a text-only loop early.
        text_prose.contains(ctx.done_sentinel)
    } else {
        false
    };
    let sentinel_in_text = (ctx.has_tools && tagged_done_hit) || plain_done_hit;
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
    let allow_done_sentinel = ctx
        .turn_policy
        .map(|policy| policy.allow_done_sentinel)
        .unwrap_or(true);
    // exit_when_verified: honor sentinel only if the last run() exit 0.
    let verified = !ctx.exit_when_verified || state.last_run_exit_code == Some(0);
    // Guard against premature exit where the model emits done without acting.
    let has_acted = !state.all_tools_used.is_empty() || !tool_calls.is_empty();
    let completion_ready = has_acted || !ctx.has_tools;
    // Ledger gate: reject done while open/blocked deliverables remain.
    let ledger_blocks_done = state.task_ledger.gates_done();
    let completion_requested =
        sentinel_in_text || (allow_done_sentinel && user_response.as_deref().is_some());
    let sentinel_hit = ctx.persistent
        && ((completion_requested && verified && completion_ready && !ledger_blocks_done)
            || phase_change);

    if completion_requested && ledger_blocks_done && ctx.persistent {
        let corrective = state.task_ledger.done_gate_feedback();
        append_message_to_contexts(
            &mut state.visible_messages,
            &mut state.recorded_messages,
            runtime_feedback_message("ledger_not_satisfied", corrective),
        );
        state.ledger_done_rejections += 1;
    }

    if completion_requested && !verified && ctx.persistent {
        let code_str = state
            .last_run_exit_code
            .map_or("none".to_string(), |c| c.to_string());
        let corrective = format!(
            "You emitted a completion signal but verification has not passed \
             (last run exit code: {code_str}). The loop will continue. \
             Run the verification command and fix any failures before finishing."
        );
        append_message_to_contexts(
            &mut state.visible_messages,
            &mut state.recorded_messages,
            runtime_feedback_message("verification_gate", corrective),
        );
    }
    if completion_requested && !has_acted && ctx.persistent && ctx.has_tools {
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
        let result_text = if state.task_ledger.is_empty() {
            if !state.rejected_tools.contains(&"ledger".to_string()) {
                state.rejected_tools.push("ledger".to_string());
            }
            "<tool_result>ledger unavailable: no task ledger is active in this turn</tool_result>"
                .to_string()
        } else {
            match state.task_ledger.apply(&args) {
                Ok(summary) => {
                    state.all_tools_used.push("ledger".to_string());
                    state.successful_tools_used.push("ledger".to_string());
                    format!("<tool_result>ledger: {summary}</tool_result>")
                }
                Err(err) => format!("<tool_result>ledger error: {err}</tool_result>"),
            }
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
        input_tokens: result.input_tokens,
        output_tokens: result.output_tokens,
    })
}

/// Per-block translation of provider-native `tool_search_*` content into
/// transcript + agent events. Pure: no global state, no I/O — the
/// caller dispatches the produced events. Pulled out so harn#691 can
/// unit-test the `ProviderNative` executor tagging without standing up
/// an entire agent loop.
pub(super) struct ProviderNativeSearchEmissions {
    pub agent_events: Vec<AgentEvent>,
    pub transcript_events: Vec<crate::value::VmValue>,
}

pub(super) fn provider_native_search_emissions(
    blocks: &[serde_json::Value],
    session_id: &str,
    native_search_mode: &str,
) -> ProviderNativeSearchEmissions {
    let mut agent_events: Vec<AgentEvent> = Vec::new();
    let mut transcript_events: Vec<crate::value::VmValue> = Vec::new();
    for block in blocks {
        match block.get("type").and_then(|v| v.as_str()) {
            Some("tool_search_query") => {
                let tool_use_id = block
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let name = block
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let query = block
                    .get("query")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                transcript_events.push(transcript_event(
                    "tool_search_query",
                    "assistant",
                    "internal",
                    "",
                    Some(serde_json::json!({
                        "id": tool_use_id,
                        "name": name,
                        "query": query,
                        "mode": native_search_mode,
                    })),
                ));
                agent_events.push(AgentEvent::ToolSearchQuery {
                    session_id: session_id.to_string(),
                    tool_use_id: tool_use_id.clone(),
                    name: name.clone(),
                    query: query.clone(),
                    // Native paths don't expose a strategy knob — the
                    // provider chooses. Use an empty string so replays
                    // can still match against `ev.metadata?.strategy`
                    // without a nil guard.
                    strategy: String::new(),
                    mode: native_search_mode.to_string(),
                });
                // Mirror the search-specific emission as a generic
                // tool-call pair tagged `ProviderNative` (harn#691).
                // Clients keying off `ToolCall`/`ToolCallUpdate` to
                // render badges can attribute the run to the provider
                // without having to special-case `tool_search` blocks.
                agent_events.push(AgentEvent::ToolCall {
                    session_id: session_id.to_string(),
                    tool_call_id: format!("provider-{tool_use_id}"),
                    tool_name: name,
                    kind: Some(ToolKind::Search),
                    status: ToolCallStatus::InProgress,
                    raw_input: query,
                    parsing: None,
                    audit: crate::orchestration::current_mutation_session(),
                });
            }
            Some("tool_search_result") => {
                let tool_use_id = block
                    .get("tool_use_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let promoted: Vec<String> = block
                    .get("tool_references")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|r| {
                                r.get("tool_name")
                                    .and_then(|n| n.as_str())
                                    .map(String::from)
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let tool_references = block
                    .get("tool_references")
                    .cloned()
                    .unwrap_or(serde_json::Value::Array(Vec::new()));
                transcript_events.push(transcript_event(
                    "tool_search_result",
                    "tool",
                    "internal",
                    "",
                    Some(serde_json::json!({
                        "tool_use_id": tool_use_id,
                        "tool_references": tool_references.clone(),
                        "promoted": promoted,
                        "mode": native_search_mode,
                    })),
                ));
                agent_events.push(AgentEvent::ToolSearchResult {
                    session_id: session_id.to_string(),
                    tool_use_id: tool_use_id.clone(),
                    promoted: promoted.clone(),
                    strategy: String::new(),
                    mode: native_search_mode.to_string(),
                });
                // Pair the ToolCall emitted on the corresponding query
                // block. The `provider-` prefix matches above so a UI
                // can correlate the two events.
                agent_events.push(AgentEvent::ToolCallUpdate {
                    session_id: session_id.to_string(),
                    tool_call_id: format!("provider-{tool_use_id}"),
                    tool_name: "tool_search".to_string(),
                    status: ToolCallStatus::Completed,
                    raw_output: Some(serde_json::json!({
                        "tool_references": tool_references,
                        "promoted": promoted,
                        "mode": native_search_mode,
                    })),
                    error: None,
                    duration_ms: None,
                    execution_duration_ms: None,
                    error_category: None,
                    executor: Some(ToolExecutor::ProviderNative),
                    parsing: None,

                    raw_input: None,
                    raw_input_partial: None,
                    audit: crate::orchestration::current_mutation_session(),
                });
            }
            _ => {}
        }
    }
    ProviderNativeSearchEmissions {
        agent_events,
        transcript_events,
    }
}

#[cfg(test)]
mod tests {
    //! Harn#691: assert the `ProviderNative` executor variant is
    //! emitted when the response carries provider-native server-tool
    //! result blocks (currently `tool_search_result`).

    use super::*;

    #[test]
    fn tool_search_result_block_emits_provider_native_tool_call_update() {
        let blocks = vec![
            serde_json::json!({
                "type": "tool_search_query",
                "id": "tsq-1",
                "name": "tool_search",
                "query": {"q": "github"},
            }),
            serde_json::json!({
                "type": "tool_search_result",
                "tool_use_id": "tsq-1",
                "tool_references": [{"tool_name": "create_issue"}],
            }),
        ];
        let emissions = provider_native_search_emissions(&blocks, "session-1", "anthropic");
        let updates: Vec<&AgentEvent> = emissions
            .agent_events
            .iter()
            .filter(|e| matches!(e, AgentEvent::ToolCallUpdate { .. }))
            .collect();
        assert_eq!(updates.len(), 1, "expected exactly one ToolCallUpdate");
        match updates[0] {
            AgentEvent::ToolCallUpdate {
                executor,
                tool_call_id,
                status,
                ..
            } => {
                assert_eq!(*status, ToolCallStatus::Completed);
                assert_eq!(tool_call_id, "provider-tsq-1");
                assert_eq!(*executor, Some(ToolExecutor::ProviderNative));
            }
            _ => unreachable!(),
        }
        // The corresponding query block should have produced an
        // in-progress ToolCall paired by the same `provider-` prefix.
        let calls: Vec<&AgentEvent> = emissions
            .agent_events
            .iter()
            .filter(|e| matches!(e, AgentEvent::ToolCall { .. }))
            .collect();
        assert_eq!(calls.len(), 1);
        match calls[0] {
            AgentEvent::ToolCall { tool_call_id, .. } => {
                assert_eq!(tool_call_id, "provider-tsq-1");
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn unrelated_blocks_do_not_emit_provider_native_events() {
        let blocks = vec![
            serde_json::json!({"type": "output_text", "text": "hi"}),
            serde_json::json!({"type": "reasoning", "text": "thinking"}),
        ];
        let emissions = provider_native_search_emissions(&blocks, "session-1", "openai");
        assert!(emissions.agent_events.is_empty());
        assert!(emissions.transcript_events.is_empty());
    }
}
