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
//!   3. For each tool call, sequentially: detect `__parse_error`
//!      sentinels from malformed provider arguments and reject;
//!      enforce the current execution policy + arg constraints; run
//!      the declarative approval policy (auto-approve / auto-deny /
//!      `session/request_permission` host bridge); run in-process
//!      PreToolUse hooks (Allow / Deny / Modify); validate required
//!      arguments; emit `tool_intent` + `ToolCall` (Pending /
//!      InProgress) events and a `tool_call` tracing span; run the
//!      loop-detect check (skip if stuck); dispatch the tool (replay
//!      fixture, parallel cache, or a fresh `dispatch_tool_execution`);
//!      track `run` exit codes for the verification gate; microcompact
//!      oversized tool output; run in-process PostToolUse hooks; emit
//!      a final `ToolCallUpdate` event and close the tracing span;
//!      record to the tool-recording mock when active; run loop-detect
//!      `record()` and optionally append or replace the result with a
//!      redirect hint; append the `tool_execution` transcript event
//!      and tool-result message (or an observation line for text-mode).
//!   4. Returns `ToolDispatchResult` carrying `tools_used_this_iter`,
//!      `tool_results_this_iter`, and the accumulated `observations`
//!      string — the post-turn phase flushes these into the
//!      conversation before the next LLM call.

use std::rc::Rc;

use crate::agent_events::{AgentEvent, ToolCallErrorCategory, ToolCallStatus, ToolExecutor};
use crate::bridge::HostBridge;
use crate::value::{ErrorCategory, VmError, VmValue};

use super::super::agent_tools::{
    classify_tool_mutation, declared_paths, denied_tool_result, dispatch_tool_execution,
    is_denied_tool_result, loop_intervention_message, render_tool_result, stable_hash,
    stable_hash_str, LoopIntervention, ToolDispatchOutcome,
};
use super::super::helpers::transcript_event;
use super::super::tools::{
    build_assistant_tool_message, build_tool_result_message, collect_tool_schemas,
    normalize_tool_args, validate_tool_args,
};
use super::helpers::{append_message_to_contexts, assistant_history_text};
use super::llm_call::LlmCallResult;
use super::state::AgentLoopState;

const REQUIRE_SIGNED_SKILLS_ENV: &str = "HARN_REQUIRE_SIGNED_SKILLS";

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

fn runtime_tool_error(error: &str, skill: &str, message: impl Into<String>) -> String {
    serde_json::to_string_pretty(&serde_json::json!({
        "error": error,
        "skill": skill,
        "message": message.into(),
    }))
    .unwrap_or_else(|_| format!("{{\"error\":\"{error}\",\"skill\":\"{skill}\"}}"))
}

#[derive(Default)]
struct RuntimeSkillProvenance {
    signed: bool,
    trusted: bool,
    signer_fingerprint: Option<String>,
    require_signature: bool,
    trusted_signers: Vec<String>,
    error: Option<String>,
}

fn env_requires_signed_skills() -> bool {
    matches!(
        std::env::var(REQUIRE_SIGNED_SKILLS_ENV)
            .ok()
            .map(|value| value.trim().to_ascii_lowercase()),
        Some(value) if value == "1" || value == "true" || value == "yes"
    )
}

fn runtime_provenance(
    entry: &std::collections::BTreeMap<String, VmValue>,
) -> RuntimeSkillProvenance {
    let mut provenance = RuntimeSkillProvenance {
        require_signature: entry
            .get("require_signature")
            .and_then(|value| match value {
                VmValue::Bool(value) => Some(*value),
                _ => None,
            })
            .unwrap_or(false),
        trusted_signers: entry
            .get("trusted_signers")
            .and_then(|value| match value {
                VmValue::List(values) => Some(
                    values
                        .iter()
                        .filter_map(|value| match value {
                            VmValue::String(value) => Some(value.to_string()),
                            _ => None,
                        })
                        .collect(),
                ),
                _ => None,
            })
            .unwrap_or_default(),
        ..RuntimeSkillProvenance::default()
    };
    let Some(inner) = entry.get("provenance").and_then(VmValue::as_dict) else {
        return provenance;
    };
    provenance.signed = inner
        .get("signed")
        .and_then(|value| match value {
            VmValue::Bool(value) => Some(*value),
            _ => None,
        })
        .unwrap_or(false);
    provenance.trusted = inner
        .get("trusted")
        .and_then(|value| match value {
            VmValue::Bool(value) => Some(*value),
            _ => None,
        })
        .unwrap_or(false);
    provenance.signer_fingerprint = inner
        .get("signer_fingerprint")
        .and_then(|value| match value {
            VmValue::String(value) => Some(value.to_string()),
            _ => None,
        });
    provenance.error = inner.get("error").and_then(|value| match value {
        VmValue::String(value) => Some(value.to_string()),
        _ => None,
    });
    provenance
}

fn emit_skill_loaded_record(
    state: &mut AgentLoopState,
    skill_id: &str,
    provenance: &RuntimeSkillProvenance,
) {
    state.transcript_events.push(transcript_event(
        "skill.loaded",
        "system",
        "internal",
        skill_id,
        Some(serde_json::json!({
            "skill_id": skill_id,
            "signer_fingerprint": provenance.signer_fingerprint,
            "signed": provenance.signed,
            "trusted": provenance.trusted,
        })),
    ));
}

fn apply_loaded_skill_prompt(state: &mut AgentLoopState, entry: &VmValue, prompt: String) {
    let mut active = super::state::ActiveSkill::from_entry(entry);
    active.prompt = if prompt.trim().is_empty() {
        None
    } else {
        Some(prompt)
    };

    if let Some(existing) = state
        .active_skills
        .iter_mut()
        .find(|skill| skill.name == active.name)
    {
        *existing = active.clone();
    }
    if let Some(existing) = state
        .loaded_skills
        .iter_mut()
        .find(|skill| skill.name == active.name)
    {
        *existing = active;
    } else {
        state.loaded_skills.push(active);
    }
}

fn execute_runtime_load_skill(
    state: &mut AgentLoopState,
    requested: &str,
    require_signature: bool,
    session_id: &str,
) -> String {
    let registry = match state.skill_registry.as_ref() {
        Some(registry) => registry.clone(),
        None => {
            return runtime_tool_error(
                "skill_registry_unavailable",
                requested,
                "load_skill requires agent_loop to receive a `skills:` registry",
            )
        }
    };

    let entry = match crate::skills::resolve_skill_entry(&registry, requested, "load_skill") {
        Ok(entry) => entry,
        Err(message) => return runtime_tool_error("skill_not_found", requested, message),
    };
    let entry_value = VmValue::Dict(Rc::new(entry.clone()));
    let active = super::state::ActiveSkill::from_entry(&entry_value);
    let skill_id = crate::skills::skill_entry_id(&entry);
    let provenance = runtime_provenance(&entry);
    emit_skill_loaded_record(state, &skill_id, &provenance);

    if active.disable_model_invocation {
        return runtime_tool_error(
            "skill_model_invocation_disabled",
            &skill_id,
            format!("skill '{skill_id}' is gated to explicit user invocation"),
        );
    }
    let signature_required = require_signature
        || env_requires_signed_skills()
        || provenance.require_signature
        || !provenance.trusted_signers.is_empty();
    if signature_required && !provenance.signed {
        return runtime_tool_error(
            "UnsignedSkillError",
            &skill_id,
            provenance
                .error
                .unwrap_or_else(|| format!("skill '{skill_id}' is missing a valid signature")),
        );
    }
    if signature_required && !provenance.trusted {
        let signer = provenance
            .signer_fingerprint
            .unwrap_or_else(|| "unknown".to_string());
        return runtime_tool_error(
            "UntrustedSignerError",
            &skill_id,
            provenance.error.unwrap_or_else(|| {
                format!("skill '{skill_id}' was signed by untrusted signer {signer}")
            }),
        );
    }

    let binding = crate::skills::current_skill_registry();
    let loaded = match crate::skills::load_skill_from_registry(
        &registry,
        binding.as_ref().map(|bound| &bound.fetcher),
        requested,
        Some(session_id),
        "load_skill",
    ) {
        Ok(loaded) => loaded,
        Err(message) => return runtime_tool_error("skill_not_found", requested, message),
    };
    let entry_value = VmValue::Dict(Rc::new(loaded.entry));
    apply_loaded_skill_prompt(state, &entry_value, loaded.rendered_body.clone());
    loaded.rendered_body
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

    // Parallel pre-fetch for a leading run of read-only tools. Sequential
    // dispatch still runs all bookkeeping (policy, hooks, transcript,
    // ordering) and only reuses the cached result when the hook path
    // would have called the tool anyway. Unannotated tools are treated
    // as NOT read-only (fail-safe).
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
        (Result<serde_json::Value, VmError>, Option<ToolExecutor>),
    > = std::collections::HashMap::new();
    if !parallel_indices.is_empty() {
        // Use raw pre-hook tool_args; re-running a read-only tool when
        // a hook modifies/denies is at worst wasted work.
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
        let joined: Vec<ToolDispatchOutcome> = join_all(futures).await;
        for (i, idx) in parallel_indices.iter().enumerate() {
            let outcome = &joined[i];
            parallel_results.insert(*idx, (outcome.result.clone(), outcome.executor.clone()));
        }
    }

    for (tc_index, tc) in tool_calls.iter().enumerate() {
        let tool_id = tc["id"].as_str().unwrap_or("");
        let tool_name = tc["name"].as_str().unwrap_or("");
        let mut tool_args = normalize_tool_args(tool_name, &tc["arguments"]);

        // Client-mode tool_search (harn#70): intercept the synthetic
        // `__harn_tool_search` call before any normal policy / hook /
        // dispatch machinery runs. Its handler runs the configured
        // strategy against the deferred-tool index, promotes the
        // matching tools onto opts.native_tools for the next turn, and
        // emits the `tool_search_query` / `tool_search_result`
        // transcript events that replay treats as indistinguishable
        // from the Anthropic native path.
        let is_client_search = state
            .tool_search_client
            .as_ref()
            .is_some_and(|c| c.synthetic_name == tool_name);
        if is_client_search {
            let result_text = super::tool_search_client::handle_client_tool_search(
                state, opts, ctx.bridge, tool_id, &tool_args,
            )
            .await?;
            tools_used_this_iter.push(tool_name.to_string());
            tool_results_this_iter.push(serde_json::json!({
                "tool_name": tool_name,
                "status": "ok",
                "rejected": false,
            }));
            if ctx.tool_format == "native" {
                append_message_to_contexts(
                    &mut state.visible_messages,
                    &mut state.recorded_messages,
                    build_tool_result_message(tool_id, tool_name, &result_text, &opts.provider),
                );
            } else {
                observations.push_str(&format!(
                    "[result of {tool_name}]\n{result_text}\n[end of {tool_name} result]\n\n"
                ));
            }
            continue;
        }

        if tool_name == "load_skill" {
            let requested = tool_args
                .get("name")
                .and_then(|value| value.as_str())
                .map(str::trim)
                .unwrap_or("");
            let require_signature = match tool_args.get("require_signature") {
                Some(serde_json::Value::Bool(value)) => *value,
                Some(_) => {
                    let result_text = runtime_tool_error(
                        "invalid_arguments",
                        requested,
                        "load_skill `require_signature` must be a boolean",
                    );
                    tools_used_this_iter.push(tool_name.to_string());
                    tool_results_this_iter.push(serde_json::json!({
                        "tool_name": tool_name,
                        "status": "error",
                        "rejected": false,
                    }));
                    state.transcript_events.push(transcript_event(
                        "tool_execution",
                        "tool",
                        "internal",
                        &result_text,
                        Some(serde_json::json!({
                            "tool_name": tool_name,
                            "tool_use_id": tool_id,
                            "rejected": false,
                        })),
                    ));
                    if ctx.tool_format == "native" {
                        append_message_to_contexts(
                            &mut state.visible_messages,
                            &mut state.recorded_messages,
                            build_tool_result_message(
                                tool_id,
                                tool_name,
                                &result_text,
                                &opts.provider,
                            ),
                        );
                    } else {
                        observations.push_str(&format!(
                            "[result of {tool_name}]\n{result_text}\n[end of {tool_name} result]\n\n"
                        ));
                    }
                    continue;
                }
                None => false,
            };
            let result_text = if requested.is_empty() {
                runtime_tool_error(
                    "invalid_arguments",
                    "",
                    "load_skill requires a non-empty `name` argument",
                )
            } else {
                execute_runtime_load_skill(state, requested, require_signature, ctx.session_id)
            };
            let status = if result_text.starts_with('{') && result_text.contains("\"error\"") {
                "error"
            } else {
                "ok"
            };
            tools_used_this_iter.push(tool_name.to_string());
            tool_results_this_iter.push(serde_json::json!({
                "tool_name": tool_name,
                "status": status,
                "rejected": false,
            }));
            state.transcript_events.push(transcript_event(
                "tool_execution",
                "tool",
                "internal",
                &result_text,
                Some(serde_json::json!({
                    "tool_name": tool_name,
                    "tool_use_id": tool_id,
                    "rejected": false,
                })),
            ));
            if ctx.tool_format == "native" {
                append_message_to_contexts(
                    &mut state.visible_messages,
                    &mut state.recorded_messages,
                    build_tool_result_message(tool_id, tool_name, &result_text, &opts.provider),
                );
            } else {
                observations.push_str(&format!(
                    "[result of {tool_name}]\n{result_text}\n[end of {tool_name} result]\n\n"
                ));
            }
            continue;
        }

        // Hoisted before any failure check so early-exit paths (parse
        // error, policy denial, schema validation, permission denial,
        // hook deny) can emit a `ToolCall(Pending)` + `ToolCallUpdate(
        // Failed, error_category=...)` pair. Clients that today saw
        // nothing for these failures now get a structured failure event.
        // Synthetic dispatchers above this point (`is_client_search`,
        // `load_skill`) intentionally bypass this — they have their
        // own observation paths.
        let tool_call_id = if tool_id.is_empty() {
            format!("tool-iter-{iteration}-{tc_index}")
        } else {
            format!("tool-{tool_id}")
        };
        let tool_kind = crate::orchestration::current_tool_annotations(tool_name).map(|a| a.kind);
        super::emit_agent_event(&AgentEvent::ToolCall {
            session_id: ctx.session_id.to_string(),
            tool_call_id: tool_call_id.clone(),
            tool_name: tool_name.to_string(),
            kind: tool_kind,
            status: ToolCallStatus::Pending,
            raw_input: tool_args.clone(),
            parsing: None,
        })
        .await;

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
                    "error_category": ToolCallErrorCategory::SchemaValidation.as_str(),
                })),
            ));
            super::emit_agent_event(&AgentEvent::ToolCallUpdate {
                session_id: ctx.session_id.to_string(),
                tool_call_id: tool_call_id.clone(),
                tool_name: tool_name.to_string(),
                status: ToolCallStatus::Failed,
                raw_output: None,
                error: Some(parse_err.to_string()),
                duration_ms: None,
                execution_duration_ms: None,
                error_category: Some(ToolCallErrorCategory::SchemaValidation),
                executor: None,
                parsing: None,

                raw_input: None,
                raw_input_partial: None,
            })
            .await;
            if ctx.tool_format == "native" {
                append_message_to_contexts(
                    &mut state.visible_messages,
                    &mut state.recorded_messages,
                    build_tool_result_message(tool_id, tool_name, &result_text, &opts.provider),
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
            let error_message = error.to_string();
            let result_text = render_tool_result(&denied_tool_result(
                tool_name,
                format!(
                    "{error}. Use one of the declared tools exactly as named and put extra fields inside that tool's arguments."
                ),
            ));
            if !state.rejected_tools.contains(&tool_name.to_string()) {
                state.rejected_tools.push(tool_name.to_string());
            }
            state
                .transcript_events
                .push(crate::llm::permissions::permission_transcript_event(
                    "PermissionDeny",
                    tool_name,
                    &tool_args,
                    &error_message,
                    false,
                ));
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
                    "error_category": ToolCallErrorCategory::PermissionDenied.as_str(),
                })),
            ));
            super::emit_agent_event(&AgentEvent::ToolCallUpdate {
                session_id: ctx.session_id.to_string(),
                tool_call_id: tool_call_id.clone(),
                tool_name: tool_name.to_string(),
                status: ToolCallStatus::Failed,
                raw_output: None,
                error: Some(error_message),
                duration_ms: None,
                execution_duration_ms: None,
                error_category: Some(ToolCallErrorCategory::PermissionDenied),
                executor: None,
                parsing: None,

                raw_input: None,
                raw_input_partial: None,
            })
            .await;
            if ctx.tool_format == "native" {
                append_message_to_contexts(
                    &mut state.visible_messages,
                    &mut state.recorded_messages,
                    build_tool_result_message(tool_id, tool_name, &result_text, &opts.provider),
                );
            } else {
                observations.push_str(&format!(
                    "[result of {tool_name}]\n{result_text}\n[end of {tool_name} result]\n\n"
                ));
            }
            continue;
        }

        if let Some(permission) = crate::llm::permissions::check_dynamic_permission(
            &mut state.permission_session_grants,
            tool_name,
            &tool_args,
            ctx.session_id,
        )
        .await?
        {
            match permission {
                crate::llm::permissions::PermissionCheck::Granted { reason, escalated } => {
                    if escalated {
                        state.transcript_events.push(
                            crate::llm::permissions::permission_transcript_event(
                                "PermissionEscalation",
                                tool_name,
                                &tool_args,
                                &reason,
                                true,
                            ),
                        );
                    }
                    state.transcript_events.push(
                        crate::llm::permissions::permission_transcript_event(
                            "PermissionGrant",
                            tool_name,
                            &tool_args,
                            &reason,
                            escalated,
                        ),
                    );
                }
                crate::llm::permissions::PermissionCheck::Denied { reason, escalated } => {
                    if escalated {
                        state.transcript_events.push(
                            crate::llm::permissions::permission_transcript_event(
                                "PermissionEscalation",
                                tool_name,
                                &tool_args,
                                &reason,
                                true,
                            ),
                        );
                    }
                    state.transcript_events.push(
                        crate::llm::permissions::permission_transcript_event(
                            "PermissionDeny",
                            tool_name,
                            &tool_args,
                            &reason,
                            escalated,
                        ),
                    );
                    let denial_reason = reason.clone();
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
                            "permission": "denied",
                            "escalated": escalated,
                            "error_category": ToolCallErrorCategory::PermissionDenied.as_str(),
                        })),
                    ));
                    super::emit_agent_event(&AgentEvent::ToolCallUpdate {
                        session_id: ctx.session_id.to_string(),
                        tool_call_id: tool_call_id.clone(),
                        tool_name: tool_name.to_string(),
                        status: ToolCallStatus::Failed,
                        raw_output: None,
                        error: Some(denial_reason),
                        duration_ms: None,
                        execution_duration_ms: None,
                        error_category: Some(ToolCallErrorCategory::PermissionDenied),
                        executor: None,
                        parsing: None,

                        raw_input: None,
                        raw_input_partial: None,
                    })
                    .await;
                    if ctx.tool_format == "native" {
                        append_message_to_contexts(
                            &mut state.visible_messages,
                            &mut state.recorded_messages,
                            build_tool_result_message(
                                tool_id,
                                tool_name,
                                &result_text,
                                &opts.provider,
                            ),
                        );
                    } else {
                        observations.push_str(&format!(
                            "[result of {tool_name}]\n{result_text}\n[end of {tool_name} result]\n\n"
                        ));
                    }
                    continue;
                }
            }
        }

        let approval_decision = crate::orchestration::current_approval_policy()
            .map(|policy| policy.evaluate(tool_name, &tool_args));
        let approval_outcome = match approval_decision {
            None | Some(crate::orchestration::ToolApprovalDecision::AutoApproved) => Ok(None),
            Some(crate::orchestration::ToolApprovalDecision::AutoDenied { reason }) => {
                Err(("auto_denied", reason))
            }
            Some(crate::orchestration::ToolApprovalDecision::RequiresHostApproval) => {
                // ACP `session/request_permission`. Fail closed: host
                // errors / missing method → deny.
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
                        "declaredPathEntries": crate::orchestration::current_tool_declared_path_entries(tool_name, &tool_args),
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
            let result_text = render_tool_result(&denied_tool_result(tool_name, reason.clone()));
            if !state.rejected_tools.contains(&tool_name.to_string()) {
                state.rejected_tools.push(tool_name.to_string());
            }
            state
                .transcript_events
                .push(crate::llm::permissions::permission_transcript_event(
                    "PermissionDeny",
                    tool_name,
                    &tool_args,
                    &reason,
                    false,
                ));
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
                    "error_category": ToolCallErrorCategory::PermissionDenied.as_str(),
                })),
            ));
            super::emit_agent_event(&AgentEvent::ToolCallUpdate {
                session_id: ctx.session_id.to_string(),
                tool_call_id: tool_call_id.clone(),
                tool_name: tool_name.to_string(),
                status: ToolCallStatus::Failed,
                raw_output: None,
                error: Some(reason),
                duration_ms: None,
                execution_duration_ms: None,
                error_category: Some(ToolCallErrorCategory::PermissionDenied),
                executor: None,
                parsing: None,

                raw_input: None,
                raw_input_partial: None,
            })
            .await;
            if ctx.tool_format == "native" {
                append_message_to_contexts(
                    &mut state.visible_messages,
                    &mut state.recorded_messages,
                    build_tool_result_message(tool_id, tool_name, &result_text, &opts.provider),
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
        match crate::orchestration::run_pre_tool_hooks(tool_name, &tool_args).await? {
            crate::orchestration::PreToolAction::Allow => {}
            crate::orchestration::PreToolAction::Deny(reason) => {
                let denial_reason = reason.clone();
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
                        "error_category": ToolCallErrorCategory::PermissionDenied.as_str(),
                    })),
                ));
                super::emit_agent_event(&AgentEvent::ToolCallUpdate {
                    session_id: ctx.session_id.to_string(),
                    tool_call_id: tool_call_id.clone(),
                    tool_name: tool_name.to_string(),
                    status: ToolCallStatus::Failed,
                    raw_output: None,
                    error: Some(denial_reason),
                    duration_ms: None,
                    execution_duration_ms: None,
                    error_category: Some(ToolCallErrorCategory::PermissionDenied),
                    executor: None,
                    parsing: None,

                    raw_input: None,
                    raw_input_partial: None,
                })
                .await;
                if ctx.tool_format == "native" {
                    append_message_to_contexts(
                        &mut state.visible_messages,
                        &mut state.recorded_messages,
                        build_tool_result_message(tool_id, tool_name, &result_text, &opts.provider),
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

        if let Err(msg) = validate_tool_args(tool_name, &tool_args, &tool_schemas) {
            let validation_message = msg.clone();
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
                    "error_category": ToolCallErrorCategory::SchemaValidation.as_str(),
                })),
            ));
            super::emit_agent_event(&AgentEvent::ToolCallUpdate {
                session_id: ctx.session_id.to_string(),
                tool_call_id: tool_call_id.clone(),
                tool_name: tool_name.to_string(),
                status: ToolCallStatus::Failed,
                raw_output: None,
                error: Some(validation_message),
                duration_ms: None,
                execution_duration_ms: None,
                error_category: Some(ToolCallErrorCategory::SchemaValidation),
                executor: None,
                parsing: None,

                raw_input: None,
                raw_input_partial: None,
            })
            .await;
            if ctx.tool_format == "native" {
                append_message_to_contexts(
                    &mut state.visible_messages,
                    &mut state.recorded_messages,
                    build_tool_result_message(tool_id, tool_name, &result_text, &opts.provider),
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
        // `ToolCall(Pending)` was already emitted at the top of the loop
        // body so early-failure paths can pair it with a failed update.
        // The InProgress transition runs only once we've cleared every
        // pre-flight check and are about to dispatch.
        super::emit_agent_event(&AgentEvent::ToolCallUpdate {
            session_id: ctx.session_id.to_string(),
            tool_call_id: tool_call_id.clone(),
            tool_name: tool_name.to_string(),
            status: ToolCallStatus::InProgress,
            raw_output: None,
            error: None,
            duration_ms: None,
            execution_duration_ms: None,
            error_category: None,
            // The dispatcher picks the backend below; the in-progress
            // emission is unconditional and runs before that choice.
            executor: None,
            parsing: None,

            raw_input: None,
            raw_input_partial: None,
        })
        .await;
        let tool_span_id =
            crate::tracing::span_start(crate::tracing::SpanKind::ToolCall, tool_name.to_string());
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
        // Check BEFORE dispatch whether this call is stuck in a loop.
        let args_hash = if ctx.loop_detect_enabled {
            stable_hash(&tool_args)
        } else {
            0
        };
        if ctx.loop_detect_enabled {
            if let LoopIntervention::Skip { count } = state.loop_tracker.check(tool_name, args_hash)
            {
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
                        "rejected": true,
                        "error_category": ToolCallErrorCategory::RejectedLoop.as_str(),
                    })),
                ));
                if ctx.tool_format == "native" {
                    append_message_to_contexts(
                        &mut state.visible_messages,
                        &mut state.recorded_messages,
                        build_tool_result_message(tool_id, tool_name, &skip_msg, &opts.provider),
                    );
                } else {
                    observations.push_str(&format!(
                        "[result of {tool_name}]\n{skip_msg}\n[end of {tool_name} result]\n\n"
                    ));
                }
                crate::tracing::span_end(tool_span_id);
                super::emit_agent_event(&AgentEvent::ToolCallUpdate {
                    session_id: ctx.session_id.to_string(),
                    tool_call_id: tool_call_id.clone(),
                    tool_name: tool_name.to_string(),
                    status: ToolCallStatus::Failed,
                    raw_output: Some(serde_json::json!({
                        "loop_skipped": true,
                        "repeat_count": count,
                    })),
                    error: Some(format!(
                        "tool loop detected (skipped after {count} repeats)"
                    )),
                    duration_ms: Some(tool_started_at.elapsed().as_millis() as u64),
                    execution_duration_ms: None,
                    error_category: Some(ToolCallErrorCategory::RejectedLoop),
                    // Loop intervention preempts the backend choice —
                    // the tool never ran.
                    executor: None,
                    parsing: None,

                    raw_input: None,
                    raw_input_partial: None,
                })
                .await;
                continue;
            }
        }

        let replay_hit = if crate::llm::mock::get_tool_recording_mode()
            == crate::llm::mock::ToolRecordingMode::Replay
        {
            crate::llm::mock::find_tool_replay_fixture(tool_name, &tool_args)
        } else {
            None
        };

        let tool_start = std::time::Instant::now();
        let mut tool_executor: Option<ToolExecutor> = None;
        let (is_rejected, result_text, dispatch_error_category) =
            if let Some(fixture) = replay_hit {
                let category = fixture
                    .is_rejected
                    .then_some(ToolCallErrorCategory::PermissionDenied);
                // Replay fixtures pre-date the dispatch decision; the
                // recording captured the result, not where it ran.
                (fixture.is_rejected, fixture.result.clone(), category)
            } else {
                // Reuse the parallel pre-fetch result when present.
                let (exec_result, executor) = if let Some((cached_result, cached_executor)) =
                    parallel_results.remove(&tc_index)
                {
                    (cached_result, cached_executor)
                } else {
                    let outcome = dispatch_tool_execution(
                        tool_name,
                        &tool_args,
                        ctx.tools_val,
                        ctx.bridge.as_ref(),
                        ctx.tool_retries,
                        ctx.tool_backoff_ms,
                    )
                    .await;
                    (outcome.result, outcome.executor)
                };
                tool_executor = executor;

                let rejected = matches!(
                    &exec_result,
                    Err(VmError::CategorizedError {
                        category: ErrorCategory::ToolRejected,
                        ..
                    })
                ) || exec_result.as_ref().ok().is_some_and(is_denied_tool_result);
                // Categorize before flattening to a string. `ToolRejected`
                // (or a denied dict) collapses to `permission_denied`; any
                // other categorized error projects through `from_internal`;
                // anything else is generic `tool_error`. This is the only
                // emission site that has the original `VmError` in scope —
                // downstream code only sees the rendered text.
                let category: Option<ToolCallErrorCategory> = match &exec_result {
                    Ok(val) => is_denied_tool_result(val)
                        .then_some(ToolCallErrorCategory::PermissionDenied),
                    Err(VmError::CategorizedError {
                        category: ErrorCategory::ToolRejected,
                        ..
                    }) => Some(ToolCallErrorCategory::PermissionDenied),
                    Err(VmError::CategorizedError { category: cat, .. }) => {
                        Some(ToolCallErrorCategory::from_internal(cat))
                    }
                    Err(_) => Some(ToolCallErrorCategory::ToolError),
                };
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
                (rejected, text, category)
            };

        if is_rejected && !state.rejected_tools.contains(&tool_name.to_string()) {
            state.rejected_tools.push(tool_name.to_string());
        }

        // Track run() exit codes for verification-gated exit.
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

        let result_text =
            crate::orchestration::run_post_tool_hooks(tool_name, &tool_args, &result_text).await?;

        let execution_duration_ms = tool_start.elapsed().as_millis() as u64;
        let duration_ms = tool_started_at.elapsed().as_millis() as u64;
        // Treat "Error:" / "ERROR:" prefixes (from a non-rejected
        // dispatch error or a tool that returned an error string) as a
        // failure on the wire — they were already classified as `error`
        // in the per-iteration tool_results aggregate (see `tool_status`
        // below). Pre-categorization at dispatch time supplies the
        // `error_category`; if that's unset (replay path with no
        // metadata), fall back to `ToolError` for the prefix case.
        let final_status_failed =
            is_rejected || result_text.starts_with("Error:") || result_text.starts_with("ERROR:");
        let final_error_category = if final_status_failed {
            dispatch_error_category.or(Some(ToolCallErrorCategory::ToolError))
        } else {
            None
        };
        super::emit_agent_event(&AgentEvent::ToolCallUpdate {
            session_id: ctx.session_id.to_string(),
            tool_call_id: tool_call_id.clone(),
            tool_name: tool_name.to_string(),
            status: if final_status_failed {
                ToolCallStatus::Failed
            } else {
                ToolCallStatus::Completed
            },
            raw_output: Some(serde_json::json!({
                "text": result_text,
                "tool_use_id": tool_id,
            })),
            error: if final_status_failed {
                Some(result_text.clone())
            } else {
                None
            },
            duration_ms: Some(duration_ms),
            execution_duration_ms: Some(execution_duration_ms),
            error_category: final_error_category,
            executor: tool_executor.clone(),
            parsing: None,

            raw_input: None,
            raw_input_partial: None,
        })
        .await;

        crate::tracing::span_end(tool_span_id);

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
                    LoopIntervention::Warn { .. } => format!("{result_text}{msg}"),
                    LoopIntervention::Block { .. } => msg,
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

        let mut transcript_metadata = serde_json::json!({
            "tool_name": tool_name,
            "tool_use_id": tool_id,
            "rejected": is_rejected,
        });
        if let Some(cat) = final_error_category {
            transcript_metadata["error_category"] =
                serde_json::Value::String(cat.as_str().to_string());
        }
        state.transcript_events.push(transcript_event(
            "tool_execution",
            "tool",
            "internal",
            &result_text,
            Some(transcript_metadata),
        ));

        if is_rejected {
            super::super::trace::emit_agent_event(
                super::super::trace::AgentTraceEvent::ToolRejected {
                    tool_name: tool_name.to_string(),
                    reason: result_text.clone(),
                    iteration,
                },
            );
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
                build_tool_result_message(tool_id, tool_name, &result_text, &opts.provider),
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
