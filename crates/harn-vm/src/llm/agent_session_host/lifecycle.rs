//! Session initialization, terminal projection, and autonomy admission.

use super::*;

/// Initialize a Harn-driven agent session: open transcript, seed user message.
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "__host_agent_session_init(message: string, system?: string|nil, options?: dict|nil) -> {session_id: string, run_id: string, task: string, system: string|nil, max_iterations: int, max_verify_attempts: int, done: bool, result: any?}",
    kind = "async",
    category = "agent.host",
    runtime_only = true
)]
async fn host_agent_session_init(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let message = args.first().map(|v| v.display()).unwrap_or_default();
    let system = match args.get(1) {
        Some(VmValue::String(s)) => Some(s.to_string()),
        _ => None,
    };
    let opts_map = opts_dict(args.get(2));
    let host_bridge = crate::llm::agent_runtime::current_host_bridge();
    let session_id = opt_str(&opts_map, "session_id")
        .or_else(crate::agent_sessions::current_session_id)
        .unwrap_or_else(|| format!("agent_session_{}", now_id()));

    let initialized =
        live_transcript_journal::initialize(&session_id, &opts_map, system.clone()).await?;
    let has_canonical_history = initialized.has_canonical_history;
    let run_id = initialized.run_id;
    let prompt_session_id = initialized.session_id;
    // The prepared journal/session is live before any hook runs. One guard
    // owns rollback from here through host registration so `?`, unwind, and
    // future cancellation cannot strand a partial session.
    let mut init_rollback = cancellation::AgentSessionInitRollback::new(prompt_session_id.clone());

    let prompt_payload = serde_json::json!({
        "event": crate::orchestration::HookEvent::UserPromptSubmit.as_str(),
        "session": {"id": &prompt_session_id},
        "prompt": &message,
        "system": system.clone().unwrap_or_default(),
    });
    if let crate::orchestration::HookControl::Block { reason } =
        crate::orchestration::run_lifecycle_hooks_with_control_with_ctx(
            Some(&ctx),
            crate::orchestration::HookEvent::UserPromptSubmit,
            &prompt_payload,
        )
        .await?
    {
        live_transcript_journal::flush_init_terminal(
            &prompt_session_id,
            "blocked",
            "user_prompt_submit_blocked",
        )
        .await?;
        let blocked = build_user_prompt_block_result(&prompt_session_id, &message, &reason);
        init_rollback.disarm();
        return Ok(agent_init_control_done(
            &prompt_session_id,
            &run_id,
            &message,
            system.as_deref(),
            blocked,
        ));
    }

    let autonomy_budget = match check_autonomy_budget(&opts_map, &session_id).await? {
        AutonomyCheck::NoBudget => None,
        AutonomyCheck::Approved(config) => Some(config),
        AutonomyCheck::Denied(result) => {
            live_transcript_journal::flush_init_terminal(
                &prompt_session_id,
                "blocked",
                "autonomy_budget_denied",
            )
            .await?;
            init_rollback.disarm();
            return Ok(agent_init_control_done(
                &session_id,
                &run_id,
                &message,
                system.as_deref(),
                result,
            ));
        }
    };

    let session_system_prompt =
        crate::llm::helpers::compose_system_prompt(system.clone(), Some(&opts_map))?;
    let resolved = crate::agent_sessions::open_or_create(Some(session_id));
    if let Some(system_prompt) = session_system_prompt.as_deref() {
        crate::agent_sessions::record_system_prompt(&resolved, system_prompt)
            .map_err(VmError::Runtime)?;
    }

    let nested_policy_guard = match install_session_nested_budget(&opts_map, &resolved) {
        Ok(guard) => Some(CancelSafeNestedExecutionGuard::new(guard)),
        Err(error) => {
            let denial = build_nested_budget_denial(&resolved, &message, &error);
            live_transcript_journal::flush_init_terminal(
                &resolved,
                "blocked",
                "nested_policy_denied",
            )
            .await?;
            init_rollback.disarm();
            return Ok(agent_init_control_done(
                &resolved,
                &run_id,
                &message,
                system.as_deref(),
                denial,
            ));
        }
    };

    let max_iterations = opt_int(&opts_map, "max_iterations").unwrap_or(50).max(1);
    let max_verify_attempts = opt_int(&opts_map, "max_verify_attempts")
        .unwrap_or(20)
        .max(0);
    let daemon_config = crate::llm::daemon::parse_daemon_loop_config(Some(&opts_map));
    let resumed_iterations = match daemon_config.resume_path.as_deref() {
        Some(path) => crate::llm::daemon::load_snapshot(path)?.total_iterations,
        None => 0,
    };

    if let Some(config) = autonomy_budget.as_ref() {
        crate::llm::autonomy_budget::note_decision(config);
    }

    let persisted_active_skills = crate::agent_sessions::active_skills(&resolved);

    let tool_format = opt_str(&opts_map, "tool_format").unwrap_or_default();
    if !tool_format.is_empty() {
        crate::agent_sessions::claim_tool_format(&resolved, &tool_format)
            .map_err(VmError::Runtime)?;
    }

    let llm_transcript_dir = opt_str(&opts_map, "llm_transcript_dir").unwrap_or_default();
    let transcript_dir = (!llm_transcript_dir.is_empty()).then_some(llm_transcript_dir);
    // Seed any caller-managed conversation history BEFORE the fresh user turn,
    // so the first (and every) provider request presents the prior turns
    // exactly as `llm_call`'s `messages` array would. The caller owns this
    // history — it is transient seeding, not session persistence. See
    // `seed_history_messages`.
    let seeded_history = if has_canonical_history {
        Vec::new()
    } else {
        seed_history_messages(&opts_map)?
    };
    let has_history = has_canonical_history || !seeded_history.is_empty();
    for history_msg in seeded_history {
        crate::agent_sessions::inject_message(&resolved, history_msg).map_err(VmError::Runtime)?;
    }

    // Inject the fresh user turn. When history is present and the task message
    // is blank, the caller's history already carries the latest user turn, so
    // skip appending an empty user turn (providers reject empty content).
    if !(has_history && message.trim().is_empty()) {
        let user_msg = serde_json::json!({
            "role": "user",
            "content": initial_user_content(&opts_map, &message),
        });
        crate::agent_sessions::inject_message(&resolved, json_to_vm(&user_msg))
            .map_err(VmError::Runtime)?;
    }

    let session = AgentHostSession {
        session_id: resolved.clone(),
        run_id: run_id.clone(),
        task: message.clone(),
        tokens_used: 0,
        cost_used: 0.0,
        unpriced_calls: 0,
        usage_unknown_calls: 0,
        input_tokens: 0,
        output_tokens: 0,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        provider_call_count: 0,
        active_skills: persisted_active_skills,
        tool_calls: Vec::new(),
        successful_tools: Vec::new(),
        rejected_tools: Vec::new(),
        tool_mode: tool_format,
        last_provider: None,
        last_model: None,
        last_tool_format: None,
        transcript_dir: transcript_dir.clone(),
        started_at: now_id(),
        max_iterations,
        daemon_state: None,
        daemon_snapshot_path: None,
        resumed_iterations,
        daemon_watch_state: std::collections::BTreeMap::new(),
        daemon_idle_backoff_ms: 100,
        host_bridge,
        last_llm_stop_reason: None,
        file_provenance: crate::security::FileProvenanceLedger::default(),
        nested_policy_guard,
    };

    AGENT_HOST_SESSIONS.with(|sessions| {
        sessions.borrow_mut().insert(resolved.clone(), session);
    });
    // Push the session id onto the thread-local current-session stack so
    // tool handlers + nested calls inside the loop see it via
    // `agent_session_current_id()`. Paired with the pop in finalize.
    crate::agent_sessions::push_current_session(resolved.clone());
    if let Some(dir) = transcript_dir.as_deref() {
        crate::llm::agent_observe::push_llm_transcript_dir(dir);
    }

    let start_payload = serde_json::json!({
        "event": crate::orchestration::HookEvent::SessionStart.as_str(),
        "session": {"id": &resolved},
        "task": &message,
        "system": system.clone().unwrap_or_default(),
        "max_iterations": max_iterations,
    });
    let initialized = async {
        crate::orchestration::run_lifecycle_hooks_with_ctx(
            Some(&ctx),
            crate::orchestration::HookEvent::SessionStart,
            &start_payload,
        )
        .await?;
        // SessionStart is a paired event: hooks above run any user-registered
        // `session_start` closures, and this call lets canonical reminder
        // providers (currently `project_facts`) inject pre-turn context.
        // Mirrors the pattern used at the `PostToolUse` and `PostCompact` call
        // sites so adding new providers does not require new wiring.
        let _ = crate::llm::reminder_providers::evaluate_and_inject(
            Some(&ctx),
            crate::orchestration::HookEvent::SessionStart,
            &resolved,
            start_payload,
            crate::llm::reminder_providers::options_map_to_json(&opts_map),
        )
        .await?;
        crate::agent_session_journal::flush(&resolved).await
    }
    .await;
    if let Err(error) = initialized {
        init_rollback.fail().await;
        return Err(error);
    }
    init_rollback.disarm();

    Ok(agent_init_control(
        &resolved,
        &run_id,
        &message,
        system.as_deref(),
        max_iterations,
        max_verify_attempts,
        false,
        None,
    ))
}

enum AutonomyCheck {
    NoBudget,
    Approved(crate::llm::autonomy_budget::AgentAutonomyBudget),
    Denied(VmValue),
}

async fn check_autonomy_budget(
    opts_map: &crate::value::DictMap,
    session_id: &str,
) -> Result<AutonomyCheck, VmError> {
    let Some(config) = crate::llm::autonomy_budget::parse_autonomy_budget(
        Some(opts_map),
        session_id,
        "agent_loop",
    )?
    else {
        return Ok(AutonomyCheck::NoBudget);
    };
    let trace_id = crate::triggers::dispatcher::current_dispatch_context()
        .map(|context| context.trigger_event.trace_id.0)
        .unwrap_or_else(|| format!("trace_{}", uuid::Uuid::now_v7()));
    match crate::llm::autonomy_budget::enforce_budget(config, session_id, &trace_id).await? {
        crate::llm::autonomy_budget::BudgetCheckOutcome::Approved(config) => {
            Ok(AutonomyCheck::Approved(config))
        }
        crate::llm::autonomy_budget::BudgetCheckOutcome::Denied { result } => {
            Ok(AutonomyCheck::Denied(json_to_vm(&result)))
        }
    }
}

fn build_user_prompt_block_result(session_id: &str, prompt: &str, reason: &str) -> VmValue {
    let transcript_json = crate::agent_sessions::transcript(session_id)
        .as_ref()
        .map(vm_to_json)
        .unwrap_or(serde_json::Value::Null);
    let result = serde_json::json!({
        "status": "blocked",
        "final_status": "blocked",
        "stop_reason": "user_prompt_submit_blocked",
        "error": {
            "category": "hook_denied",
            "event": crate::orchestration::HookEvent::UserPromptSubmit.as_str(),
            "reason": reason,
        },
        "text": "",
        "visible_text": "",
        "private_reasoning": serde_json::Value::Null,
        "thinking_summary": serde_json::Value::Null,
        "llm": {"iterations": 0, "duration_ms": 0, "input_tokens": 0, "output_tokens": 0},
        "tools": {"calls": [], "successful": [], "rejected": [], "mode": ""},
        "transcript": transcript_json,
        "trace": serde_json::Value::Null,
        "tokens_used": 0,
        "cost_usd": 0.0,
        "session_id": session_id,
        "task": prompt,
        "daemon_state": serde_json::Value::Null,
        "daemon_snapshot_path": serde_json::Value::Null,
    });
    crate::stdlib::json_to_vm_value(&result)
}

/// Tear down a Harn-driven agent session and emit the final result dict.
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "__host_agent_session_finalize(session_id: string, status: dict) -> dict",
    kind = "async",
    category = "agent.host",
    runtime_only = true
)]
async fn host_agent_session_finalize(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let session_id = args
        .first()
        .map(|v| v.display())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| VmError::Runtime(format!("{HOST_SESSION_FINALIZE}: missing session_id")))?;
    let status_dict = opts_dict(args.get(1));
    let mut final_status = opt_str(&status_dict, "final_status").unwrap_or_default();
    let mut stop_reason = opt_str(&status_dict, "stop_reason").unwrap_or_default();
    let mut terminal_error = opt_json(&status_dict, "error");
    let iterations = opt_int(&status_dict, "iterations").unwrap_or(0);

    let mut session = AGENT_HOST_SESSIONS
        .with(|sessions| sessions.borrow_mut().remove(&session_id))
        .ok_or_else(|| {
            VmError::Runtime(format!(
                "{HOST_SESSION_FINALIZE}: unknown session `{session_id}`"
            ))
        })?;
    permissions::clear_session_grants(&session_id);
    crate::orchestration::clear_approval_policy_repeat_counts(&session_id);

    // Promote model-less success before the terminal marker so the durable
    // descriptor matches the returned terminal result.
    if agent_loop_made_no_llm_call(
        &final_status,
        terminal_error.is_some(),
        iterations,
        session.input_tokens,
        session.output_tokens,
    ) {
        terminal_error = Some(serde_json::json!({
            "category": "no_llm_call",
            "message": "agent turn made no LLM call: no model resolved / empty input. \
                        The agent loop completed without ever calling the provider \
                        (0 iterations, 0 tokens). Check that a model is configured and \
                        the prompt is non-empty.",
        }));
        final_status = "error".to_string();
        if stop_reason.is_empty() {
            stop_reason = "no_llm_call".to_string();
        }
    }

    let canonical_status = if final_status.is_empty() {
        "done".to_string()
    } else {
        final_status.clone()
    };
    if let Some(dir) = session.transcript_dir.as_deref() {
        crate::llm::agent_session_transcript::append_finalized_marker(
            &session_id,
            &canonical_status,
            &stop_reason,
            iterations,
        );
        crate::llm::agent_observe::remove_llm_transcript_dir(dir);
    }
    if terminal_error.is_some() || session_status_indicates_error(&final_status) {
        let error_payload = serde_json::json!({
            "event": crate::orchestration::HookEvent::SessionError.as_str(),
            "session": {"id": &session_id},
            "final_status": &canonical_status,
            "stop_reason": stop_reason,
            "error": terminal_error.clone(),
        });
        // SessionError hooks are advisory — log but do not propagate so
        // session cleanup always runs.
        if let Err(err) = crate::orchestration::run_lifecycle_hooks_with_ctx(
            Some(&ctx),
            crate::orchestration::HookEvent::SessionError,
            &error_payload,
        )
        .await
        {
            crate::events::log_warn(
                "agent.session_error_hook",
                &format!("session={session_id} hook error: {err}"),
            );
        }
    }

    let end_payload = serde_json::json!({
        "event": crate::orchestration::HookEvent::SessionEnd.as_str(),
        "session": {"id": &session_id},
        "final_status": &canonical_status,
        "stop_reason": stop_reason,
        "iterations": opt_int(&status_dict, "iterations").unwrap_or(0),
    });
    if let Err(err) = crate::orchestration::run_lifecycle_hooks_with_ctx(
        Some(&ctx),
        crate::orchestration::HookEvent::SessionEnd,
        &end_payload,
    )
    .await
    {
        crate::events::log_warn(
            "agent.session_end_hook",
            &format!("session={session_id} hook error: {err}"),
        );
    }

    // Pair with the push in init so subsequent loops see the right stack.
    crate::agent_sessions::remove_current_session(&session_id);
    let tool_mode = opt_str(&status_dict, "tool_mode").unwrap_or_else(|| session.tool_mode.clone());
    let acp_stop_reason = canonical_acp_stop_reason(
        &final_status,
        iterations,
        session.max_iterations,
        session.last_llm_stop_reason.as_deref(),
    );
    let terminal_class = agent_terminal_class(&final_status, &stop_reason, terminal_error.as_ref());
    // Classify once at the loop boundary; the bridge carries this exact value to ACP.
    let terminal_outcome = crate::agent_events::terminal_outcome_for_finalize(
        &canonical_status,
        &stop_reason,
        terminal_class,
        terminal_error.is_some(),
    )
    .with_error(terminal_error.as_ref());
    if let Some(bridge) = crate::llm::agent_runtime::current_host_bridge() {
        bridge.set_prompt_outcome(acp_stop_reason, &terminal_outcome);
    }
    if let Some(error) = terminal_error.as_ref() {
        let transcript_event = crate::llm::helpers::transcript_event(
            "agent_loop_terminal_error",
            "assistant",
            "internal",
            "Agent loop ended with a provider/tool-protocol failure",
            Some(serde_json::json!({
                "status": if final_status.is_empty() { "done" } else { final_status.as_str() },
                "final_status": final_status,
                "stop_reason": stop_reason,
                "terminal_class": terminal_class,
                "error": error,
            })),
        );
        crate::agent_sessions::append_event(&session_id, transcript_event)
            .map_err(VmError::Runtime)?;
    }
    let recap_store = crate::agent_sessions::journal_store(&session_id);
    let recap_from_event_id = crate::agent_sessions::journal_first_event_id(&session_id);
    live_transcript_journal::flush_terminal(
        &session_id,
        &canonical_status,
        &stop_reason,
        terminal_class.map(crate::llm::agent_terminal_class::AgentTerminalClass::as_str),
        terminal_error.as_ref(),
        &terminal_outcome,
    )
    .await?;
    let recap = if let Some(store) = recap_store {
        match crate::session_recap::query_session_recap(
            &store,
            crate::session_recap::SessionRecapQuery {
                run_id: Some(session.run_id.clone()),
                from_event_id: recap_from_event_id,
                ..crate::session_recap::SessionRecapQuery::for_session(&session_id)
            },
        )
        .await
        {
            Ok(Some(snapshot)) => {
                crate::session_recap::SessionRecapAvailability::available(snapshot)
            }
            Ok(None) => crate::session_recap::SessionRecapAvailability::unavailable(
                crate::session_recap::SessionRecapUnavailableReason::SessionMissing,
            ),
            Err(error) => {
                crate::events::log_warn(
                    "agent.session_recap_projection",
                    &format!("session={session_id} recap projection error: {error}"),
                );
                crate::session_recap::SessionRecapAvailability::unavailable(
                    crate::session_recap::SessionRecapUnavailableReason::ProjectionFailed,
                )
            }
        }
    } else {
        crate::session_recap::SessionRecapAvailability::unavailable(
            crate::session_recap::SessionRecapUnavailableReason::JournalUnavailable,
        )
    };
    cancellation::finish_agent_session(&mut session, &session_id, canonical_status != "suspended");
    let snapshot = crate::agent_sessions::transcript(&session_id);
    let transcript_json = snapshot
        .as_ref()
        .map(vm_to_json)
        .unwrap_or(serde_json::Value::Null);
    let visible_text = snapshot
        .as_ref()
        .and_then(crate::llm::agent_result_projection::last_assistant_text)
        .unwrap_or_default();

    emit_event(&terminal_outcome.checkpoint(&session_id, &canonical_status, &stop_reason));
    // The trace event log never carried tool or loop-lifecycle facts (#5997),
    // so the summary reads them from the same session state that produces
    // `tools` and `llm` below. Deriving both projections from one owner is
    // what keeps `trace.tool_executions` from reporting zero while the
    // transcript holds the calls.
    let trace_summary = crate::llm::trace::agent_trace_summary_with_loop(
        &crate::llm::agent_result_projection::terminal_loop_facts(
            &canonical_status,
            iterations,
            &session.successful_tools,
            &session.rejected_tools,
        ),
    );
    let result = serde_json::json!({
        "status": if final_status.is_empty() { "done" } else { final_status.as_str() },
        "final_status": final_status,
        "stop_reason": stop_reason,
        "acp_stop_reason": acp_stop_reason,
        "terminal_class": terminal_class,
        "terminal": terminal_outcome.to_json(),
        "error": terminal_error,
        "text": visible_text,
        "visible_text": visible_text,
        "private_reasoning": serde_json::Value::Null,
        "thinking_summary": serde_json::Value::Null,
        "llm": {
            // One accepted result per agent turn. Schema retries, empty
            // completions, and model-ladder advances never reach this
            // accounting, so it is legitimately smaller than the trace's
            // `total_input_tokens` — sometimes by a large factor. Naming the
            // scope is what lets a reader tell that apart from a defect.
            "token_scope": "accepted_turn_results",
            "iterations": iterations,
            "duration_ms": 0,
            "input_tokens": session.input_tokens,
            "output_tokens": session.output_tokens,
            "cache_read_tokens": session.cache_read_tokens,
            "cache_write_tokens": session.cache_write_tokens,
            "accounting_status": if session.unpriced_calls == 0 && session.usage_unknown_calls == 0 {
                "reported"
            } else if session.cost_used > 0.0 || session.tokens_used > 0 {
                "partial"
            } else {
                "unknown"
            },
            "known_cost_usd": session.cost_used,
            "unpriced_calls": session.unpriced_calls,
            "usage_unknown_calls": session.usage_unknown_calls,
        },
        "tools": {
            "calls": session.tool_calls,
            "successful": session.successful_tools,
            "rejected": session.rejected_tools,
            "mode": tool_mode,
        },
        "transcript": transcript_json,
        "recap": recap,
        "trace": trace_summary,
        "tokens_used": session.tokens_used,
        "cost_usd": if session.unpriced_calls == 0 {
            Some(session.cost_used)
        } else {
            None
        },
        "known_cost_usd": session.cost_used,
        "unpriced_calls": session.unpriced_calls,
        "usage_unknown_calls": session.usage_unknown_calls,
        "provider_call_count": session.provider_call_count,
        "session_id": session.session_id,
        "run_id": session.run_id,
        "started_at": session.started_at,
        "task": session.task,
        "daemon_state": session.daemon_state,
        "daemon_snapshot_path": session.daemon_snapshot_path,
    });
    Ok(json_to_vm(&result))
}

/// Map an agent-loop terminal state to the canonical ACP `stopReason`
/// enumeration documented at <https://agentclientprotocol.com/protocol/prompt-turn>.
///
/// ACP defines five values: `end_turn`, `max_tokens`, `max_turn_requests`,
/// `refusal`, and `cancelled`. `cancelled` is decided one layer up by the
/// adapter (it observes the cancel notification directly) so this
/// function only chooses among the other four.
///
/// Precedence: a loop that ran out of turn budget overrides any
/// per-call signal — the caller stopped the agent before the model
/// could refuse or truncate again. When the loop exited cleanly we fall
/// through to the most recent provider stop_reason.
pub(crate) fn canonical_acp_stop_reason(
    final_status: &str,
    iterations: i64,
    max_iterations: i64,
    last_llm_stop_reason: Option<&str>,
) -> &'static str {
    if final_status == "budget_exhausted" {
        if max_iterations > 0 && iterations >= max_iterations {
            return "max_turn_requests";
        }
        // Token / cost / autonomy budgets all cap how many requests the
        // loop will issue, so they collapse to the same canonical
        // reason. ACP's `max_tokens` is reserved for a single response
        // truncated by the provider's `max_tokens` parameter.
        return "max_turn_requests";
    }
    canonical_provider_stop_reason(last_llm_stop_reason)
}

pub(crate) fn canonical_provider_stop_reason(last_llm_stop_reason: Option<&str>) -> &'static str {
    match last_llm_stop_reason {
        Some(reason) if crate::llm::api::result::stop_reason_is_length(reason) => "max_tokens",
        Some(reason) if reason.eq_ignore_ascii_case("refusal") => "refusal",
        _ => "end_turn",
    }
}

/// True when a provider stop_reason means "I ran out of output-token budget
/// mid-emit", i.e. the response was cut off rather than completed.
///
/// Keys on the normalized condition, not one wire format: OpenAI / OpenRouter /
/// Ollama (`/v1` finish_reason + native `done_reason`) report `length`;
/// Anthropic reports `max_tokens`. Both canonicalize to `max_tokens` via
/// [`canonical_provider_stop_reason`], so we reuse that mapping as the single
/// source of truth — a new provider that adopts either spelling is covered for
/// free.
pub(crate) fn is_length_truncation(stop_reason: Option<&str>) -> bool {
    canonical_provider_stop_reason(stop_reason) == "max_tokens"
}

/// True when the model's text looks like it was mid-tool-call when the stream
/// was cut off: it contains a text-tool-call opener (the `<tool_call>` tag or a
/// bare `name(` shape) but the turn resolved ZERO usable tool calls. This is the
/// "truncated, unparseable tool call" fingerprint — distinct from a model that
/// simply ran long on prose with no tool intent.
///
/// Deliberately permissive on the *prefix* side (any opener) but strict on the
/// *outcome* side (zero calls dispatched): a turn that landed even one tool call
/// made real progress and is not a truncation casualty.
pub(super) fn text_has_tool_call_prefix(text: &str) -> bool {
    if text.contains(crate::llm::tools::TEXT_TOOL_CALL_OPEN)
        || text.contains(crate::llm::tools::TEXT_TOOL_CALL_OPEN_COMPACT)
    {
        return true;
    }
    // Bare `name(` shape at the start of any line — the text-tool wire format
    // the agent loop reads back. We only need a cheap structural sniff here;
    // the authoritative parse already ran and produced zero calls, so this just
    // decides whether continuing is worthwhile.
    for line in text.lines() {
        let trimmed = line.trim_start();
        if let Some(ident) = crate::llm::tools::ident_length(trimmed.as_bytes()) {
            if trimmed.as_bytes().get(ident) == Some(&b'(') {
                return true;
            }
        }
    }
    false
}

/// Decide whether the agent loop should AUTO-CONTINUE (re-issue the completion
/// with a raised output cap) instead of burning the turn on parse-guidance.
///
/// Fires only when ALL hold:
///   1. `stop_reason` is a length truncation (the response was cut off), AND
///   2. the turn resolved ZERO usable tool calls, AND
///   3. there is a partial tool-call signal — either the parser emitted a
///      diagnostic (e.g. "unterminated heredoc") or the raw text carries a
///      tool-call opener prefix.
///
/// A clean stop with a genuinely malformed call returns `false` here: that is
/// the parse-tolerance / narration-as-prose domain (#3137) and the
/// reasoning-leak domain (#3142), which this must NOT double-handle. The
/// length-truncation gate is what keeps the two from colliding — those cases
/// stop with `end_turn`/`stop`, never `length`/`max_tokens`.
pub(crate) fn truncated_tool_call_should_continue(
    stop_reason: Option<&str>,
    text: &str,
    tool_call_count: i64,
    has_parse_errors: bool,
) -> bool {
    if !is_length_truncation(stop_reason) {
        return false;
    }
    if tool_call_count > 0 {
        return false;
    }
    has_parse_errors || text_has_tool_call_prefix(text)
}

/// Check per-agent autonomy budget and return an approval-shaped denial.
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "__host_autonomy_budget_check(session_id: string, budget_config: dict) -> dict",
    kind = "async",
    category = "agent.host",
    runtime_only = true
)]
async fn host_autonomy_budget_check(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let session_id = args
        .first()
        .map(|value| value.display())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| format!("agent_session_{}", now_id()));
    let mut opts = crate::value::DictMap::new();
    if let Some(config) = args.get(1) {
        opts.insert(crate::value::intern_key("autonomy_budget"), config.clone());
    }
    match check_autonomy_budget(&opts, &session_id).await? {
        AutonomyCheck::Denied(result) => {
            let mut out = crate::value::DictMap::new();
            out.insert(crate::value::intern_key("approved"), VmValue::Bool(false));
            out.insert(crate::value::intern_key("denial_result"), result);
            Ok(VmValue::dict(out))
        }
        AutonomyCheck::Approved(_) | AutonomyCheck::NoBudget => {
            let mut out = crate::value::DictMap::new();
            out.insert(crate::value::intern_key("approved"), VmValue::Bool(true));
            Ok(VmValue::dict(out))
        }
    }
}

const LIFECYCLE_BUILTINS: &[&VmBuiltinDef] = &[
    &HOST_AGENT_SESSION_INIT_DEF,
    &HOST_AGENT_SESSION_FINALIZE_DEF,
    &HOST_AUTONOMY_BUDGET_CHECK_DEF,
];

pub(super) fn register_lifecycle_primitives(vm: &mut Vm) {
    register_builtin_defs(vm, LIFECYCLE_BUILTINS);
}
