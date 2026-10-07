use super::*;

/// What every terminal arm of one completed provider attempt reports.
///
/// Observation runs in its own non-inlined functions so its JSON temporaries
/// stay out of `observed_llm_call`'s future, whose frame is paid on every
/// nested agent descent.
pub(super) struct CompletedAttempt<'a> {
    pub(super) opts: &'a crate::llm::api::LlmCallOptions,
    pub(super) result: &'a crate::llm::api::LlmResult,
    pub(super) call_id: &'a str,
    pub(super) bridge: Option<&'a Arc<crate::bridge::HostBridge>>,
    pub(super) iteration: Option<usize>,
    pub(super) attempt: usize,
    pub(super) duration_ms: u64,
    pub(super) user_visible: bool,
    pub(super) effective_tool_format: &'a str,
}

/// Report an attempt that ended as a terminal unproductive completion.
#[inline(never)]
pub(super) fn observe_terminal_unproductive_completion(
    attempt: CompletedAttempt<'_>,
    error: &VmError,
    attempt_count: usize,
) {
    let CompletedAttempt {
        opts,
        result,
        call_id,
        bridge,
        iteration,
        attempt,
        duration_ms,
        user_visible,
        effective_tool_format,
    } = attempt;
    let category = crate::value::error_to_category(error);
    let message = error.to_string();
    let classified = crate::llm::api::classify_vm_llm_error(error);
    let status = "retries_exhausted";
    let usage = result.usage();
    annotate_current_span(&[
        ("status", serde_json::json!(status)),
        ("error", serde_json::json!(message.as_str())),
        ("retryable", serde_json::json!(false)),
        ("failover_eligible", serde_json::json!(true)),
        ("attempt", serde_json::json!(attempt)),
    ]);
    annotate_current_span(&usage.metadata_pairs(&result.provider, &result.model));
    dump_llm_response(
        iteration.unwrap_or(0),
        call_id,
        result,
        duration_ms,
        opts.applied_structural_experiment.as_ref(),
        opts.call_stage.as_deref(),
    );
    append_provider_call_error_observability(ProviderCallErrorObservation {
        iteration: iteration.unwrap_or(0),
        call_id,
        attempt: attempt_count,
        status,
        opts,
        category: &category,
        classified: &classified,
        message: &message,
        stream_failure: None,
        schema_failure: None,
        usage: Some(&usage),
        retryable: false,
        failover_eligible: true,
        attempt_count: Some(attempt_count),
    });
    dump_resolved_dispatch(
        iteration.unwrap_or(0),
        call_id,
        opts,
        effective_tool_format,
        &crate::llm::resolved_dispatch::DispatchOutcome::from_error(error),
    );
    if let Some(b) = bridge {
        b.send_call_end(
            call_id,
            "llm",
            "llm_call",
            duration_ms,
            status,
            serde_json::json!({
                "error": message,
                "retryable": false,
                "failover_eligible": true,
                "attempt": attempt,
                "user_visible": user_visible,
            }),
        );
    }
    if let Some(metrics) = crate::active_metrics_registry() {
        metrics.record_llm_call(&result.provider, &result.model, status, &usage);
    }
    trace_llm_call(LlmTraceEntry {
        model: result.model.clone(),
        provider: result.provider.clone(),
        usage,
        duration_ms,
    });
}

/// Report an attempt the provider answered, and close or feed the breaker.
#[inline(never)]
pub(super) fn observe_successful_completion(
    attempt: CompletedAttempt<'_>,
    empty_completion_retries: usize,
) {
    let CompletedAttempt {
        opts,
        result,
        call_id,
        bridge,
        iteration,
        duration_ms,
        user_visible,
        effective_tool_format,
        ..
    } = attempt;
    let usage = result.usage();
    annotate_current_span(&[("status", serde_json::json!("ok"))]);
    annotate_current_span(&usage.metadata_pairs(&result.provider, &result.model));
    dump_llm_response(
        iteration.unwrap_or(0),
        call_id,
        result,
        duration_ms,
        opts.applied_structural_experiment.as_ref(),
        opts.call_stage.as_deref(),
    );
    dump_resolved_dispatch(
        iteration.unwrap_or(0),
        call_id,
        opts,
        effective_tool_format,
        &crate::llm::resolved_dispatch::DispatchOutcome::from_result(
            result,
            empty_completion_retries,
        ),
    );
    annotate_current_span(&[(
        "structural_experiment",
        opts.applied_structural_experiment
            .as_ref()
            .map(serde_json::to_value)
            .transpose()
            .unwrap_or(None)
            .unwrap_or(serde_json::Value::Null),
    )]);
    if let Some(b) = bridge {
        b.send_call_end(
            call_id,
            "llm",
            "llm_call",
            duration_ms,
            "ok",
            serde_json::json!({
                "model": result.model,
                "input_tokens": usage.input_tokens,
                "output_tokens": usage.output_tokens,
                "user_visible": user_visible,
                "structural_experiment": opts.applied_structural_experiment.as_ref(),
            }),
        );
    }
    trace_llm_call(LlmTraceEntry {
        model: result.model.clone(),
        provider: result.provider.clone(),
        usage: usage.clone(),
        duration_ms,
    });
    if let Some(metrics) = crate::active_metrics_registry() {
        metrics.record_llm_call(&result.provider, &result.model, "succeeded", &usage);
        if usage.cache_hit {
            metrics.record_llm_cache_hit(&result.provider);
        }
    }
    crate::llm::trace::emit_agent_event(crate::llm::trace::AgentTraceEvent::LlmCall {
        call_id: call_id.to_string(),
        model: result.model.clone(),
        usage,
        duration_ms,
        iteration: iteration.unwrap_or(0),
    });
    crate::llm::agent_session_host::record_auxiliary_call_usage(opts, result);
    // A terminal unproductive completion (a zero-token empty or a
    // billed-noncommittal turn that survived the built-in
    // empty-completion retry budget) is served-but-useless. It must
    // NOT close the breaker as if the route answered — that reset is
    // exactly what let the same throttled/empty lane be re-dispatched
    // every turn, storming 18-43x per trial. Feed the always-on
    // unproductive-completion streak instead so a route that keeps
    // empty-completing trips `circuit_open` fast (governor-independent,
    // and works for a single-provider model harn#4023's failover
    // cannot rescue). A genuinely answering turn closes the breaker.
    if is_retryable_unproductive_completion(result)
        && !crate::llm::providers::is_internal_simulator(&opts.provider)
    {
        let reason = if is_empty_unproductive_completion(result) {
            UnproductiveCompletionReason::EmptyGeneration
        } else {
            UnproductiveCompletionReason::UnproductiveCompletion
        };
        crate::llm::rate_limit::observe_unproductive_completion_for_llm_call(opts, reason.as_str());
    } else {
        crate::llm::rate_limit::observe_network_outcome_for_llm_call(opts, false);
    }
}
