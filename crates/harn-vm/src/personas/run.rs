use super::*;

const DEFAULT_LEASE_TTL_MS: i64 = 5 * 60 * 1000;

pub(crate) enum PersonaRunAdmission {
    Admitted(PersonaRunContext),
    Terminal(PersonaRunReceipt),
}

pub(crate) struct PersonaRunContext {
    envelope: PersonaTriggerEnvelope,
    cost: PersonaRunCost,
    lease: PersonaLease,
    run_id: Uuid,
    value_metadata: serde_json::Value,
    pre_queue: Vec<QueueEntry>,
}

impl PersonaRunContext {
    pub(crate) fn with_cost(mut self, cost: PersonaRunCost) -> Self {
        self.value_metadata = run_value_metadata(&self.envelope, &self.lease, &cost);
        self.cost = cost;
        self
    }
}

pub async fn fire_schedule(
    log: &Arc<AnyEventLog>,
    binding: &PersonaRuntimeBinding,
    cost: PersonaRunCost,
    now_ms: i64,
) -> Result<PersonaRunReceipt, String> {
    let schedule = binding
        .schedules
        .first()
        .cloned()
        .unwrap_or_else(|| "manual".to_string());
    let envelope = PersonaTriggerEnvelope {
        provider: "schedule".to_string(),
        kind: "cron.tick".to_string(),
        subject_key: format!("schedule:{}:{schedule}:{}", binding.name, format_ms(now_ms)),
        source_event_id: None,
        received_at_ms: now_ms,
        metadata: BTreeMap::from([
            ("persona".to_string(), binding.name.clone()),
            ("schedule".to_string(), schedule),
            ("fired_at".to_string(), format_ms(now_ms)),
        ]),
        raw: json!({}),
    };
    append_persona_event(
        log,
        &binding.name,
        "persona.schedule.fired",
        json!({"persona": binding.name, "envelope": envelope}),
        now_ms,
    )
    .await?;
    run_for_envelope(log, binding, envelope, cost, now_ms).await
}

pub async fn fire_trigger(
    log: &Arc<AnyEventLog>,
    binding: &PersonaRuntimeBinding,
    provider: &str,
    kind: &str,
    metadata: BTreeMap<String, String>,
    cost: PersonaRunCost,
    now_ms: i64,
) -> Result<PersonaRunReceipt, String> {
    match begin_persona_trigger(log, binding, provider, kind, metadata, cost, now_ms).await? {
        PersonaRunAdmission::Admitted(context) => {
            complete_persona_run(log, binding, context, None, now_ms).await
        }
        PersonaRunAdmission::Terminal(receipt) => Ok(receipt),
    }
}

pub(crate) async fn begin_persona_trigger(
    log: &Arc<AnyEventLog>,
    binding: &PersonaRuntimeBinding,
    provider: &str,
    kind: &str,
    metadata: BTreeMap<String, String>,
    cost: PersonaRunCost,
    now_ms: i64,
) -> Result<PersonaRunAdmission, String> {
    let envelope = normalize_trigger_envelope(provider, kind, metadata, now_ms);
    append_persona_event(
        log,
        &binding.name,
        "persona.trigger.received",
        json!({"persona": binding.name, "envelope": envelope}),
        now_ms,
    )
    .await?;
    begin_for_envelope(log, binding, envelope, cost, now_ms).await
}

pub async fn record_persona_spend(
    log: &Arc<AnyEventLog>,
    binding: &PersonaRuntimeBinding,
    cost: PersonaRunCost,
    now_ms: i64,
) -> Result<PersonaBudgetStatus, String> {
    enforce_budget(log, binding, &cost, now_ms).await?;
    append_budget_record(log, &binding.name, &cost, None, now_ms).await?;
    persona_status(log, binding, now_ms)
        .await
        .map(|status| status.budget)
}

pub(super) async fn run_for_envelope(
    log: &Arc<AnyEventLog>,
    binding: &PersonaRuntimeBinding,
    envelope: PersonaTriggerEnvelope,
    cost: PersonaRunCost,
    now_ms: i64,
) -> Result<PersonaRunReceipt, String> {
    match begin_for_envelope(log, binding, envelope, cost, now_ms).await? {
        PersonaRunAdmission::Admitted(context) => {
            complete_persona_run(log, binding, context, None, now_ms).await
        }
        PersonaRunAdmission::Terminal(receipt) => Ok(receipt),
    }
}

async fn begin_for_envelope(
    log: &Arc<AnyEventLog>,
    binding: &PersonaRuntimeBinding,
    envelope: PersonaTriggerEnvelope,
    cost: PersonaRunCost,
    now_ms: i64,
) -> Result<PersonaRunAdmission, String> {
    let pre_queue = queue_snapshot(log, binding, now_ms).await?;
    let status = persona_status(log, binding, now_ms).await?;
    match status.state {
        PersonaLifecycleState::Paused => {
            append_persona_event(
                log,
                &binding.name,
                "persona.trigger.queued",
                json!({
                    "work_key": envelope.subject_key,
                    "envelope": envelope,
                    "cost": cost,
                    "reason": "paused",
                }),
                now_ms,
            )
            .await?;
            let receipt = PersonaRunReceipt {
                status: "queued".to_string(),
                persona: binding.name.clone(),
                run_id: None,
                work_key: envelope.subject_key,
                lease: None,
                queued: true,
                error: None,
                budget_receipt_id: None,
                result: None,
            };
            return terminal_persona_admission(log, binding, &pre_queue, receipt, now_ms).await;
        }
        PersonaLifecycleState::Disabled => {
            append_persona_event(
                log,
                &binding.name,
                "persona.trigger.dead_lettered",
                json!({
                    "work_key": envelope.subject_key,
                    "envelope": envelope,
                    "reason": "disabled",
                }),
                now_ms,
            )
            .await?;
            let receipt = PersonaRunReceipt {
                status: "dead_lettered".to_string(),
                persona: binding.name.clone(),
                run_id: None,
                work_key: envelope.subject_key,
                lease: None,
                queued: false,
                error: Some("persona is disabled".to_string()),
                budget_receipt_id: None,
                result: None,
            };
            return terminal_persona_admission(log, binding, &pre_queue, receipt, now_ms).await;
        }
        _ => {}
    }

    if let Err(error) = enforce_budget(log, binding, &cost, now_ms).await {
        let receipt = PersonaRunReceipt {
            status: "budget_exhausted".to_string(),
            persona: binding.name.clone(),
            run_id: None,
            work_key: envelope.subject_key,
            lease: None,
            queued: false,
            error: Some(error),
            budget_receipt_id: None,
            result: None,
        };
        return terminal_persona_admission(log, binding, &pre_queue, receipt, now_ms).await;
    }

    if work_completed(log, &binding.name, &envelope.subject_key).await? {
        append_persona_event(
            log,
            &binding.name,
            "persona.trigger.duplicate",
            json!({
                "work_key": envelope.subject_key,
                "envelope": envelope,
                "reason": "already_completed",
            }),
            now_ms,
        )
        .await?;
        let receipt = PersonaRunReceipt {
            status: "duplicate".to_string(),
            persona: binding.name.clone(),
            run_id: None,
            work_key: envelope.subject_key,
            lease: None,
            queued: false,
            error: None,
            budget_receipt_id: None,
            result: None,
        };
        return terminal_persona_admission(log, binding, &pre_queue, receipt, now_ms).await;
    }

    let Some(lease) = acquire_lease(
        log,
        binding,
        &envelope.subject_key,
        "persona-runtime",
        DEFAULT_LEASE_TTL_MS,
        now_ms,
    )
    .await?
    else {
        let receipt = PersonaRunReceipt {
            status: "lease_busy".to_string(),
            persona: binding.name.clone(),
            run_id: None,
            work_key: envelope.subject_key,
            lease: status.active_lease,
            queued: false,
            error: Some("active lease already owns persona work".to_string()),
            budget_receipt_id: None,
            result: None,
        };
        return terminal_persona_admission(log, binding, &pre_queue, receipt, now_ms).await;
    };

    let run_id = Uuid::now_v7();
    let value_metadata = run_value_metadata(&envelope, &lease, &cost);
    append_persona_event(
        log,
        &binding.name,
        "persona.run.started",
        json!({
            "work_key": envelope.subject_key,
            "run_id": run_id,
            "started_at_ms": now_ms,
            "entry_workflow": binding.entry_workflow,
            "lease_id": lease.id,
        }),
        now_ms,
    )
    .await?;
    emit_persona_value_event(
        log,
        binding,
        run_id,
        PersonaValueEventDelta {
            kind: PersonaValueEventKind::RunStarted,
            metadata: value_metadata.clone(),
            ..Default::default()
        },
        now_ms,
    )
    .await?;
    Ok(PersonaRunAdmission::Admitted(PersonaRunContext {
        envelope,
        cost,
        lease,
        run_id,
        value_metadata,
        pre_queue,
    }))
}

pub(crate) async fn complete_persona_run(
    log: &Arc<AnyEventLog>,
    binding: &PersonaRuntimeBinding,
    context: PersonaRunContext,
    result: Option<serde_json::Value>,
    now_ms: i64,
) -> Result<PersonaRunReceipt, String> {
    let PersonaRunContext {
        envelope,
        cost,
        lease,
        run_id,
        value_metadata,
        pre_queue,
    } = context;
    let budget_receipt_id =
        append_budget_record(log, &binding.name, &cost, Some(&lease.id), now_ms).await?;
    if cost.avoided_cost_usd > 0.0 || cost.deterministic_steps > 0 {
        emit_persona_value_event(
            log,
            binding,
            run_id,
            PersonaValueEventDelta {
                kind: PersonaValueEventKind::DeterministicExecution,
                avoided_cost_usd: cost.avoided_cost_usd,
                deterministic_steps: cost.deterministic_steps.max(1),
                metadata: value_metadata.clone(),
                ..Default::default()
            },
            now_ms,
        )
        .await?;
    }
    if cost.frontier_escalations > 0 {
        emit_persona_value_event(
            log,
            binding,
            run_id,
            PersonaValueEventDelta {
                kind: PersonaValueEventKind::FrontierEscalation,
                paid_cost_usd: cost.cost_usd,
                llm_steps: cost.llm_steps.max(cost.frontier_escalations),
                metadata: value_metadata.clone(),
                ..Default::default()
            },
            now_ms,
        )
        .await?;
    }
    let completion_paid_cost = if cost.frontier_escalations > 0 {
        0.0
    } else {
        cost.cost_usd
    };
    let completion_llm_steps = if cost.frontier_escalations > 0 {
        0
    } else {
        cost.llm_steps
    };
    emit_persona_value_event(
        log,
        binding,
        run_id,
        PersonaValueEventDelta {
            kind: PersonaValueEventKind::RunCompleted,
            paid_cost_usd: completion_paid_cost,
            llm_steps: completion_llm_steps,
            metadata: value_metadata,
            ..Default::default()
        },
        now_ms,
    )
    .await?;
    append_persona_event(
        log,
        &binding.name,
        "persona.run.completed",
        json!({
            "work_key": envelope.subject_key,
            "run_id": run_id,
            "completed_at_ms": now_ms,
            "entry_workflow": binding.entry_workflow,
            "lease_id": lease.id,
        }),
        now_ms,
    )
    .await?;
    append_persona_event(
        log,
        &binding.name,
        "persona.lease.released",
        json!({
            "id": lease.id,
            "work_key": envelope.subject_key,
            "released_at_ms": now_ms,
        }),
        now_ms,
    )
    .await?;
    let receipt = PersonaRunReceipt {
        status: "completed".to_string(),
        persona: binding.name.clone(),
        run_id: Some(run_id),
        work_key: envelope.subject_key,
        lease: Some(lease),
        queued: false,
        error: None,
        budget_receipt_id: Some(budget_receipt_id),
        result,
    };
    finish_persona_receipt(log, binding, &pre_queue, receipt, now_ms).await
}

pub(crate) async fn fail_persona_run(
    log: &Arc<AnyEventLog>,
    binding: &PersonaRuntimeBinding,
    context: PersonaRunContext,
    error: &str,
    now_ms: i64,
) -> Result<PersonaRunReceipt, String> {
    let PersonaRunContext {
        envelope,
        cost,
        lease,
        run_id,
        pre_queue,
        ..
    } = context;
    let budget_receipt_id =
        append_budget_record(log, &binding.name, &cost, Some(&lease.id), now_ms).await?;
    append_persona_event(
        log,
        &binding.name,
        "persona.lease.released",
        json!({
            "id": lease.id,
            "work_key": envelope.subject_key,
            "released_at_ms": now_ms,
        }),
        now_ms,
    )
    .await?;
    append_persona_event(
        log,
        &binding.name,
        "persona.run.failed",
        json!({
            "work_key": envelope.subject_key,
            "run_id": run_id,
            "failed_at_ms": now_ms,
            "entry_workflow": binding.entry_workflow,
            "lease_id": lease.id,
            "error": error,
        }),
        now_ms,
    )
    .await?;
    let receipt = PersonaRunReceipt {
        status: "failed".to_string(),
        persona: binding.name.clone(),
        run_id: Some(run_id),
        work_key: envelope.subject_key,
        lease: Some(lease),
        queued: false,
        error: Some(error.to_string()),
        budget_receipt_id: Some(budget_receipt_id),
        result: None,
    };
    finish_persona_receipt(log, binding, &pre_queue, receipt, now_ms).await
}

async fn terminal_persona_admission(
    log: &Arc<AnyEventLog>,
    binding: &PersonaRuntimeBinding,
    pre_queue: &[QueueEntry],
    receipt: PersonaRunReceipt,
    now_ms: i64,
) -> Result<PersonaRunAdmission, String> {
    finish_persona_receipt(log, binding, pre_queue, receipt, now_ms)
        .await
        .map(PersonaRunAdmission::Terminal)
}

async fn finish_persona_receipt(
    log: &Arc<AnyEventLog>,
    binding: &PersonaRuntimeBinding,
    pre_queue: &[QueueEntry],
    receipt: PersonaRunReceipt,
    now_ms: i64,
) -> Result<PersonaRunReceipt, String> {
    let post_queue = queue_snapshot(log, binding, now_ms).await?;
    emit_queue_position_supervision(log, binding, pre_queue, &post_queue, now_ms).await?;
    emit_receipt_supervision(log, binding, &receipt, now_ms).await?;
    Ok(receipt)
}

async fn acquire_lease(
    log: &Arc<AnyEventLog>,
    binding: &PersonaRuntimeBinding,
    work_key: &str,
    holder: &str,
    ttl_ms: i64,
    now_ms: i64,
) -> Result<Option<PersonaLease>, String> {
    let status = persona_status(log, binding, now_ms).await?;
    if let Some(lease) = status.active_lease {
        if lease.expires_at_ms > now_ms {
            append_persona_event(
                log,
                &binding.name,
                "persona.lease.conflict",
                json!({
                    "active_lease": lease,
                    "requested_work_key": work_key,
                    "at_ms": now_ms,
                }),
                now_ms,
            )
            .await?;
            return Ok(None);
        }
        append_persona_event(
            log,
            &binding.name,
            "persona.lease.expired",
            json!({
                "id": lease.id,
                "work_key": lease.work_key,
                "expired_at_ms": now_ms,
            }),
            now_ms,
        )
        .await?;
    }

    let lease = PersonaLease {
        id: format!("persona_lease_{}", Uuid::now_v7()),
        holder: holder.to_string(),
        work_key: work_key.to_string(),
        acquired_at_ms: now_ms,
        expires_at_ms: now_ms + ttl_ms,
    };
    append_persona_event(
        log,
        &binding.name,
        "persona.lease.acquired",
        serde_json::to_value(&lease).map_err(|error| error.to_string())?,
        now_ms,
    )
    .await?;
    Ok(Some(lease))
}

async fn enforce_budget(
    log: &Arc<AnyEventLog>,
    binding: &PersonaRuntimeBinding,
    cost: &PersonaRunCost,
    now_ms: i64,
) -> Result<(), String> {
    let status = persona_status(log, binding, now_ms).await?;
    let reason = if binding
        .budget
        .run_usd
        .is_some_and(|limit| cost.cost_usd > limit)
    {
        Some("run_usd")
    } else if binding
        .budget
        .daily_usd
        .is_some_and(|limit| status.budget.spent_today_usd + cost.cost_usd > limit)
    {
        Some("daily_usd")
    } else if binding
        .budget
        .hourly_usd
        .is_some_and(|limit| status.budget.spent_this_hour_usd + cost.cost_usd > limit)
    {
        Some("hourly_usd")
    } else if binding
        .budget
        .max_tokens
        .is_some_and(|limit| status.budget.tokens_today + cost.tokens > limit)
    {
        Some("max_tokens")
    } else {
        None
    };

    if let Some(reason) = reason {
        let receipt_id = format!("persona_budget_{}", Uuid::now_v7());
        append_persona_event(
            log,
            &binding.name,
            "persona.budget.exhausted",
            json!({
                "receipt_id": receipt_id,
                "reason": reason,
                "attempted_cost_usd": cost.cost_usd,
                "attempted_tokens": cost.tokens,
                "persona": binding.name,
            }),
            now_ms,
        )
        .await?;
        return Err(format!("persona budget exhausted: {reason}"));
    }

    Ok(())
}

async fn append_budget_record(
    log: &Arc<AnyEventLog>,
    persona: &str,
    cost: &PersonaRunCost,
    lease_id: Option<&str>,
    now_ms: i64,
) -> Result<String, String> {
    let receipt_id = format!("persona_budget_{}", Uuid::now_v7());
    append_persona_event(
        log,
        persona,
        "persona.budget.recorded",
        json!({
            "receipt_id": receipt_id,
            "persona": persona,
            "cost_usd": cost.cost_usd,
            "tokens": cost.tokens,
            "lease_id": lease_id,
        }),
        now_ms,
    )
    .await?;
    Ok(receipt_id)
}

fn normalize_trigger_envelope(
    provider: &str,
    kind: &str,
    metadata: BTreeMap<String, String>,
    now_ms: i64,
) -> PersonaTriggerEnvelope {
    let provider = provider.to_ascii_lowercase();
    let kind = kind.to_string();
    let source_event_id = metadata
        .get("event_id")
        .or_else(|| metadata.get("id"))
        .cloned();
    let subject_key = match provider.as_str() {
        "github" => {
            let repo = metadata
                .get("repository")
                .or_else(|| metadata.get("repository.full_name"))
                .cloned()
                .unwrap_or_else(|| "unknown".to_string());
            if let Some(number) = metadata
                .get("pr")
                .or_else(|| metadata.get("pull_request.number"))
                .or_else(|| metadata.get("number"))
            {
                format!("github:{repo}:pr:{number}")
            } else if let Some(check) = metadata
                .get("check_run.name")
                .or_else(|| metadata.get("check_name"))
            {
                format!("github:{repo}:check:{check}")
            } else {
                format!("github:{repo}:{kind}")
            }
        }
        "linear" => {
            let issue = metadata
                .get("issue_key")
                .or_else(|| metadata.get("issue.identifier"))
                .or_else(|| metadata.get("issue_id"))
                .or_else(|| metadata.get("id"))
                .cloned()
                .unwrap_or_else(|| "unknown".to_string());
            format!("linear:issue:{issue}")
        }
        "slack" => {
            let channel = metadata
                .get("channel")
                .or_else(|| metadata.get("channel_id"))
                .cloned()
                .unwrap_or_else(|| "unknown".to_string());
            let ts = metadata
                .get("ts")
                .or_else(|| metadata.get("event_ts"))
                .cloned()
                .unwrap_or_else(|| "unknown".to_string());
            format!("slack:{channel}:{ts}")
        }
        "webhook" => metadata
            .get("dedupe_key")
            .or_else(|| metadata.get("event_id"))
            .map(|value| format!("webhook:{value}"))
            .unwrap_or_else(|| format!("webhook:{kind}:{}", Uuid::now_v7())),
        _ => metadata
            .get("dedupe_key")
            .or_else(|| metadata.get("event_id"))
            .map(|value| format!("{provider}:{kind}:{value}"))
            .unwrap_or_else(|| format!("{provider}:{kind}:{}", Uuid::now_v7())),
    };

    PersonaTriggerEnvelope {
        provider,
        kind,
        subject_key,
        source_event_id,
        received_at_ms: now_ms,
        raw: json!({"metadata": metadata}),
        metadata,
    }
}
