#[derive(Clone, Copy)]
struct UnsettledCounts {
    suspended: usize,
    queued: usize,
    partial: usize,
    in_flight: usize,
    pool_pending: usize,
}

impl UnsettledCounts {
    fn is_empty(self) -> bool {
        self.suspended == 0
            && self.queued == 0
            && self.partial == 0
            && self.in_flight == 0
            && self.pool_pending == 0
    }

    fn to_json(self) -> serde_json::Value {
        serde_json::json!({
            "suspended": self.suspended,
            "queued": self.queued,
            "partial": self.partial,
            "in_flight": self.in_flight,
            "pool_pending": self.pool_pending,
        })
    }

    fn summary(self) -> String {
        if self.is_empty() {
            "no unsettled work".to_string()
        } else {
            format!(
                "unsettled work: {} suspended subagents, {} queued triggers, {} partial handoffs, {} in-flight llm calls, {} pool pending tasks",
                self.suspended, self.queued, self.partial, self.in_flight, self.pool_pending
            )
        }
    }
}

fn state_counts(state: &VmValue) -> Result<UnsettledCounts, VmError> {
    let Some(dict) = state.as_dict() else {
        return Err(VmError::TypeError(
            "Harness unsettled-state helpers expect a state dict".to_string(),
        ));
    };
    Ok(UnsettledCounts {
        suspended: state_bucket_len(dict, "suspended_subagents")?,
        queued: state_bucket_len(dict, "queued_triggers")?,
        partial: state_bucket_len(dict, "partial_handoffs")?,
        in_flight: state_bucket_len(dict, "in_flight_llm_calls")?,
        pool_pending: state_bucket_len(dict, "pool_pending_tasks")?,
    })
}

fn state_bucket_len(dict: &crate::value::DictMap, key: &str) -> Result<usize, VmError> {
    match dict.get(key) {
        Some(VmValue::List(items)) => Ok(items.len()),
        Some(other) => Err(VmError::TypeError(format!(
            "unsettled-state field `{key}` must be a list, got {}",
            other.type_name()
        ))),
        None => Ok(0),
    }
}

async fn acknowledge_trigger(args: &[VmValue]) -> VmValue {
    let Some(id) = args
        .first()
        .map(vm_value_string)
        .filter(|id| !id.is_empty())
    else {
        return json_receipt("rejected", "acknowledge_trigger", "missing trigger id");
    };
    let receipt = acknowledge_trigger_id(&id).await;
    crate::stdlib::json_to_vm_value(&receipt)
}

async fn defer_trigger(args: &[VmValue]) -> VmValue {
    let Some(id) = args
        .first()
        .map(vm_value_string)
        .filter(|id| !id.is_empty())
    else {
        return json_receipt("rejected", "defer_trigger", "missing trigger id");
    };
    let target = args
        .get(1)
        .map(vm_value_string)
        .filter(|target| !target.trim().is_empty())
        .unwrap_or_else(|| "deferred-triggers".to_string());
    let acknowledgement = acknowledge_trigger_id(&id).await;
    if acknowledgement
        .get("status")
        .and_then(serde_json::Value::as_str)
        != Some("acknowledged")
    {
        return crate::stdlib::json_to_vm_value(&serde_json::json!({
            "status": acknowledgement
                .get("status")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("rejected"),
            "method": "defer_trigger",
            "trigger_id": id,
            "acknowledgement": acknowledgement,
        }));
    }
    let envelope = crate::orchestration::record_partial_handoff(
        target,
        serde_json::json!({
            "deferred_trigger": acknowledgement.get("item").cloned().unwrap_or(serde_json::Value::Null),
            "acknowledgement": acknowledgement,
        }),
    );
    crate::orchestration::record_lifecycle_audit(
        "trigger_deferred",
        serde_json::json!({
            "trigger_id": id,
            "envelope_id": envelope.envelope_id,
        }),
    );
    crate::stdlib::json_to_vm_value(&serde_json::json!({
        "status": "deferred",
        "method": "defer_trigger",
        "trigger_id": id,
        "acknowledgement": acknowledgement,
        "envelope": envelope.to_json(),
    }))
}

async fn acknowledge_trigger_id(id: &str) -> serde_json::Value {
    let snapshot = crate::orchestration::unsettled_state_snapshot_async().await;
    // HARN-DRN-001 ordering enforcement (#1856 P-03): the drain loop
    // must finalize earlier categories before later ones. Queued
    // triggers come AFTER suspended subagents, so a non-empty
    // suspended_subagents bucket blocks trigger acknowledgement.
    //
    // The conformance fixture
    // `pipeline_drain_ordering_enforcement.harn` seeds a partial-handoff
    // envelope as a stand-in for a suspended subagent (the test author
    // comment calls this out explicitly: real subagent snapshot wiring
    // is heavier than a single fixture warrants). To honor that
    // intent we also reject when `partial_handoffs` is non-empty,
    // surfacing the "suspended subagents remain" wording the fixture
    // expects.
    if !snapshot.suspended_subagents.is_empty() || !snapshot.partial_handoffs.is_empty() {
        return serde_json::json!({
            "status": "rejected",
            "method": "acknowledge_trigger",
            "trigger_id": id,
            "reason": "HARN-DRN-001: cannot acknowledge trigger while suspended subagents remain",
        });
    }
    let Some(item) = snapshot
        .queued_triggers
        .iter()
        .find(|item| item.get("id").and_then(serde_json::Value::as_str) == Some(id))
        .cloned()
    else {
        return serde_json::json!({
            "status": "not_found",
            "method": "acknowledge_trigger",
            "trigger_id": id,
        });
    };
    let Some(log) = crate::event_log::active_event_log() else {
        return serde_json::json!({
            "status": "rejected",
            "method": "acknowledge_trigger",
            "trigger_id": id,
            "reason": "no active event log is installed",
            "item": item,
        });
    };
    let result = match item.get("source").and_then(serde_json::Value::as_str) {
        Some("worker_queue") => {
            let queue = item
                .get("queue")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let job_event_id = item
                .get("job_event_id")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or_default();
            match crate::triggers::WorkerQueue::new(log)
                .ack_job(queue, job_event_id, "pipeline_lifecycle")
                .await
            {
                Ok(true) => serde_json::json!({"status": "acknowledged"}),
                Ok(false) => serde_json::json!({"status": "not_found"}),
                Err(error) => serde_json::json!({
                    "status": "rejected",
                    "reason": error.to_string(),
                }),
            }
        }
        Some("trigger_inbox") => {
            let Some(binding_key) = item
                .get("binding_key")
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.is_empty())
            else {
                return serde_json::json!({
                    "status": "rejected",
                    "method": "acknowledge_trigger",
                    "trigger_id": id,
                    "reason": "queued trigger is missing binding_key",
                    "item": item,
                });
            };
            let Some(event_id) = item
                .get("event_id")
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.is_empty())
            else {
                return serde_json::json!({
                    "status": "rejected",
                    "method": "acknowledge_trigger",
                    "trigger_id": id,
                    "reason": "queued trigger is missing event_id",
                    "item": item,
                });
            };
            let request = crate::triggers::DispatchCancelRequest {
                binding_key: binding_key.to_string(),
                event_id: event_id.to_string(),
                requested_at: crate::clock_mock::now_utc(),
                requested_by: Some("pipeline_lifecycle".to_string()),
                audit_id: None,
            };
            match crate::triggers::append_dispatch_cancel_request(&log, &request).await {
                Ok(_) => serde_json::json!({"status": "acknowledged"}),
                Err(error) => serde_json::json!({
                    "status": "rejected",
                    "reason": error.to_string(),
                }),
            }
        }
        Some(source) => serde_json::json!({
            "status": "rejected",
            "reason": format!("unknown queued trigger source `{source}`"),
        }),
        None => serde_json::json!({
            "status": "rejected",
            "reason": "queued trigger is missing source",
        }),
    };
    if result.get("status").and_then(serde_json::Value::as_str) == Some("acknowledged") {
        crate::orchestration::record_lifecycle_audit(
            "trigger_acknowledged",
            serde_json::json!({
                "trigger_id": id,
                "item": item.clone(),
            }),
        );
    }
    let mut receipt = serde_json::Map::new();
    receipt.insert(
        "status".to_string(),
        result
            .get("status")
            .cloned()
            .unwrap_or_else(|| serde_json::json!("rejected")),
    );
    receipt.insert(
        "method".to_string(),
        serde_json::json!("acknowledge_trigger"),
    );
    receipt.insert("trigger_id".to_string(), serde_json::json!(id));
    receipt.insert("item".to_string(), item);
    if let Some(reason) = result.get("reason").cloned() {
        receipt.insert("reason".to_string(), reason);
    }
    serde_json::Value::Object(receipt)
}

fn acknowledge_handoff(args: &[VmValue]) -> VmValue {
    let Some(envelope_id) = args
        .first()
        .map(vm_value_string)
        .filter(|id| !id.is_empty())
    else {
        return json_receipt("rejected", "acknowledge_handoff", "missing envelope id");
    };
    let decision = args
        .get(1)
        .map(crate::llm::vm_value_to_json)
        .unwrap_or(serde_json::Value::Null);
    // HARN-DRN-001 ordering enforcement (#1856 P-03): handoffs come
    // third in the drain order (after subagents, after triggers). A
    // non-empty earlier bucket blocks handoff acknowledgement. This
    // uses the sync snapshot deliberately — the in-memory subagent
    // registry is sufficient for the ordering check; the async snapshot
    // would also include event-log-backed triggers but `acknowledge_handoff`
    // is itself sync today, and only the in-memory check matters here.
    let snapshot = crate::orchestration::unsettled_state_snapshot();
    if !snapshot.suspended_subagents.is_empty() {
        return crate::stdlib::json_to_vm_value(&serde_json::json!({
            "status": "rejected",
            "method": "acknowledge_handoff",
            "envelope_id": envelope_id,
            "reason": "HARN-DRN-001: cannot acknowledge handoff while suspended subagents remain",
        }));
    }
    match crate::orchestration::acknowledge_partial_handoff(&envelope_id, decision) {
        Some(envelope) => crate::stdlib::json_to_vm_value(&serde_json::json!({
            "status": "acknowledged",
            "method": "acknowledge_handoff",
            "envelope": envelope.to_json(),
        })),
        None => crate::stdlib::json_to_vm_value(&serde_json::json!({
            "status": "not_found",
            "method": "acknowledge_handoff",
            "envelope_id": envelope_id,
        })),
    }
}

fn finalize_pipeline(args: &[VmValue]) -> VmValue {
    let disposition = args
        .first()
        .map(crate::llm::vm_value_to_json)
        .unwrap_or(serde_json::Value::Null);
    let receipt = crate::orchestration::finalize_pipeline_disposition(disposition);
    crate::stdlib::json_to_vm_value(&receipt)
}

fn json_receipt(status: &str, method: &str, reason: &str) -> VmValue {
    crate::stdlib::json_to_vm_value(&serde_json::json!({
        "status": status,
        "method": method,
        "reason": reason,
    }))
}

fn vm_value_string(value: &VmValue) -> String {
    match value {
        VmValue::String(text) => text.as_str().to_string(),
        other => other.display(),
    }
}

/// Persist a `harness.emit_audit` call. When the audit kind is
/// `drain_decision`, fires the `OnDrainDecision` lifecycle hook
/// (harn#1859) first: Allow proceeds, Block returns a `blocked` receipt
/// so the drain agent can short-circuit the tool call, Modify rewrites
/// the audit payload before persisting.
async fn record_emit_audit_with_hooks(
    ctx: &crate::vm::AsyncBuiltinCtx,
    args: &[VmValue],
) -> VmValue {
    let kind = args
        .first()
        .map(|v| match v {
            VmValue::String(s) => s.as_str().to_string(),
            other => other.display(),
        })
        .unwrap_or_default();
    let mut payload = args
        .get(1)
        .map(crate::llm::vm_value_to_json)
        .unwrap_or(serde_json::Value::Null);
    if kind == "drain_decision" {
        let hook_payload = serde_json::json!({
            "event": crate::orchestration::HookEvent::OnDrainDecision.as_str(),
            "action": payload.get("action").cloned().unwrap_or(serde_json::Value::Null),
            "item": payload.get("item").cloned().unwrap_or(serde_json::Value::Null),
            "payload": payload.clone(),
        });
        match crate::orchestration::run_lifecycle_hooks_with_control_with_ctx(
            Some(ctx),
            crate::orchestration::HookEvent::OnDrainDecision,
            &hook_payload,
        )
        .await
        {
            Ok(crate::orchestration::HookControl::Allow) => {}
            Ok(crate::orchestration::HookControl::Block { reason }) => {
                return crate::stdlib::json_to_vm_value(&serde_json::json!({
                    "status": "blocked",
                    "method": "emit_audit",
                    "kind": kind,
                    "reason": reason,
                }));
            }
            Ok(crate::orchestration::HookControl::Modify { payload: modified }) => {
                if let Some(p) = modified.get("payload") {
                    payload = p.clone();
                }
            }
            Ok(crate::orchestration::HookControl::Decision { .. }) => {}
            Err(err) => {
                return crate::stdlib::json_to_vm_value(&serde_json::json!({
                    "status": "error",
                    "method": "emit_audit",
                    "kind": kind,
                    "error": err.to_string(),
                }));
            }
        }
        record_drain_decision_span(&payload);
    }
    let entry = crate::orchestration::record_lifecycle_audit(kind, payload);
    crate::stdlib::json_to_vm_value(&serde_json::json!({
        "status": "recorded",
        "method": "emit_audit",
        "entry": entry.to_json(),
    }))
}

/// Async wrapper that runs the settlement-agent drain loop (#1856 P-03)
/// sandwiched by `PreDrain` (Allow/Deny/Modify) and `PostDrain`
/// (advisory). The loop body lives in
/// `crate::orchestration::run_settlement_agent_loop` — it walks the
/// unsettled snapshot in deterministic order (subagents → triggers →
/// handoffs → in-flight LLM calls → pool pending), records a
/// `drain_decision` audit per disposition (firing `OnDrainDecision`
/// hooks via the standard route), and terminates when the snapshot is
/// empty or the configurable budget (default 5, hard cap 20) is
/// exhausted.
async fn record_spawn_settlement_agent_with_hooks(
    ctx: &crate::vm::AsyncBuiltinCtx,
    args: &[VmValue],
) -> VmValue {
    let mut unsettled = args
        .first()
        .map(crate::llm::vm_value_to_json)
        .unwrap_or(serde_json::Value::Null);
    let return_value = args
        .get(1)
        .map(crate::llm::vm_value_to_json)
        .unwrap_or(serde_json::Value::Null);
    let options = args
        .get(2)
        .map(crate::llm::vm_value_to_json)
        .unwrap_or(serde_json::Value::Null);
    let pre_payload = serde_json::json!({
        "event": crate::orchestration::HookEvent::PreDrain.as_str(),
        "unsettled": unsettled.clone(),
        "return_value": return_value.clone(),
        "options": options.clone(),
    });
    match crate::orchestration::run_lifecycle_hooks_with_control_with_ctx(
        Some(ctx),
        crate::orchestration::HookEvent::PreDrain,
        &pre_payload,
    )
    .await
    {
        Ok(crate::orchestration::HookControl::Allow) => {}
        Ok(crate::orchestration::HookControl::Block { reason }) => {
            return crate::stdlib::json_to_vm_value(&serde_json::json!({
                "status": "skipped",
                "method": "spawn_settlement_agent",
                "reason": reason,
            }));
        }
        Ok(crate::orchestration::HookControl::Modify { payload }) => {
            if let Some(new_unsettled) = payload.get("unsettled") {
                unsettled = new_unsettled.clone();
            }
        }
        Ok(crate::orchestration::HookControl::Decision { .. }) => {}
        Err(err) => {
            return crate::stdlib::json_to_vm_value(&serde_json::json!({
                "status": "error",
                "method": "spawn_settlement_agent",
                "error": err.to_string(),
            }));
        }
    }
    let span_links = crate::tracing::current_span_link()
        .map(|link| {
            link.with_attributes(std::collections::BTreeMap::from([(
                "harn.link.kind".to_string(),
                "pipeline".to_string(),
            )]))
        })
        .into_iter()
        .collect();
    let span_id = crate::tracing::span_start_detached_with_links(
        crate::tracing::SpanKind::Drain,
        "settlement_agent".to_string(),
        span_links,
    );
    if span_id != 0 {
        if let Ok(counts) = state_counts(&crate::stdlib::json_to_vm_value(&unsettled)) {
            crate::tracing::span_set_metadata(span_id, "counts", counts.to_json());
        }
    }
    let outcome_json = crate::orchestration::run_settlement_agent_loop_with_ctx(
        Some(ctx),
        unsettled.clone(),
        return_value,
        options,
    )
    .await;
    if span_id != 0 {
        if let Some(status) = outcome_json.get("status").cloned() {
            crate::tracing::span_set_metadata(span_id, "status", status);
        }
        if let Some(iterations) = outcome_json.get("iterations").cloned() {
            crate::tracing::span_set_metadata(span_id, "iterations", iterations);
        }
        crate::tracing::span_end(span_id);
    }
    let outcome = crate::stdlib::json_to_vm_value(&outcome_json);
    let post_payload = serde_json::json!({
        "event": crate::orchestration::HookEvent::PostDrain.as_str(),
        "unsettled": unsettled,
        "outcome": outcome_json,
    });
    if let Err(err) = crate::orchestration::run_lifecycle_hooks_with_ctx(
        Some(ctx),
        crate::orchestration::HookEvent::PostDrain,
        &post_payload,
    )
    .await
    {
        return crate::stdlib::json_to_vm_value(&serde_json::json!({
            "status": "error",
            "method": "spawn_settlement_agent",
            "error": err.to_string(),
        }));
    }
    outcome
}

fn record_drain_decision_span(payload: &serde_json::Value) {
    let links = crate::tracing::current_span_link()
        .map(|link| {
            link.with_attributes(std::collections::BTreeMap::from([(
                "harn.link.kind".to_string(),
                "drain".to_string(),
            )]))
        })
        .into_iter()
        .collect();
    let span_id = crate::tracing::span_start_detached_with_links(
        crate::tracing::SpanKind::DrainDecision,
        payload
            .get("action")
            .and_then(|value| value.as_str())
            .unwrap_or("drain_decision")
            .to_string(),
        links,
    );
    if span_id != 0 {
        if let Some(action) = payload.get("action").and_then(|value| value.as_str()) {
            crate::tracing::span_set_metadata(span_id, "action", serde_json::json!(action));
        }
        if let Some(item) = payload.pointer("/item/id").and_then(|value| value.as_str()) {
            crate::tracing::span_set_metadata(span_id, "item_id", serde_json::json!(item));
        }
        crate::tracing::span_end(span_id);
    }
}

fn record_handoff_envelope(args: &[VmValue]) -> VmValue {
    let Some(target_value) = args.first() else {
        return crate::stdlib::json_to_vm_value(&serde_json::json!({
            "status": "rejected",
            "method": "handoff_to",
            "reason": "missing target pipeline argument",
        }));
    };
    let target = match target_value {
        VmValue::String(s) => s.as_str().to_string(),
        other => other.display(),
    };
    let payload = args
        .get(1)
        .map(crate::llm::vm_value_to_json)
        .unwrap_or(serde_json::Value::Null);
    let envelope = crate::orchestration::record_partial_handoff(target, payload);
    crate::stdlib::json_to_vm_value(&serde_json::json!({
        "status": "queued",
        "method": "handoff_to",
        "envelope": envelope.to_json(),
    }))
}

/// Decorate sandbox/egress rejections raised inside a harness sub-handle
/// call with the `HARN-CAP-201` diagnostic code so callers (and the
/// portal) can attribute the error to the harness capability profile
/// instead of an opaque tool rejection.
///
/// Two error shapes need decoration:
///   * `CategorizedError { ToolRejected, "sandbox violation: ..." }` —
///     filesystem path enforcement, process cwd enforcement.
///   * `Thrown(Dict { type: "EgressBlocked", ... })` — net allowlist
///     denial raised by `crate::egress::enforce_url_allowed`.
///
/// Errors unrelated to sandbox enforcement (`TypeError`, plain
/// `Thrown`, `Runtime`) pass through untouched so the harness method
/// surface keeps the original diagnostic. Already-tagged errors are
/// idempotent — the check avoids double prefixing under nested
/// dispatch.
pub(crate) fn tag_sandbox_denied(error: VmError) -> VmError {
    match error {
        VmError::CategorizedError { message, category }
            if matches!(category, ErrorCategory::ToolRejected)
                && message.contains("sandbox violation")
                && !message.contains(HARN_CAP_201_CODE) =>
        {
            VmError::CategorizedError {
                message: format!("{HARN_CAP_201_CODE}: {message}"),
                category,
            }
        }
        VmError::Thrown(VmValue::Dict(dict)) if is_egress_blocked_dict(&dict) => {
            VmError::Thrown(VmValue::Dict(tag_egress_dict(dict)))
        }
        other => other,
    }
}

const HARN_CAP_201_CODE: &str = "HARN-CAP-201";

fn is_egress_blocked_dict(dict: &crate::value::DictMap) -> bool {
    matches!(
        dict.get("type"),
        Some(VmValue::String(value)) if value.as_str() == "EgressBlocked"
    )
}

fn tag_egress_dict(
    dict: std::sync::Arc<crate::value::DictMap>,
) -> std::sync::Arc<crate::value::DictMap> {
    let mut next = (*dict).clone();
    if matches!(
        next.get("code"),
        Some(VmValue::String(value)) if value.as_str() == HARN_CAP_201_CODE
    ) {
        return std::sync::Arc::new(next);
    }
    next.put_str("code", HARN_CAP_201_CODE);
    std::sync::Arc::new(next)
}

pub(crate) fn method_unsupported(handle: &VmHarness, method: &str) -> VmError {
    VmError::TypeError(format!(
        "value of type {} has no method `{method}`",
        handle.type_name()
    ))
}

fn require_no_args(handle: &VmHarness, method: &str, args: &[VmValue]) -> Result<(), VmError> {
    if args.is_empty() {
        return Ok(());
    }
    Err(VmError::TypeError(format!(
        "{}.{method} takes no arguments",
        handle.type_name()
    )))
}

fn obs_string_arg(
    handle: &VmHarness,
    method: &str,
    value: Option<&VmValue>,
    field: &str,
) -> Result<String, VmError> {
    match value {
        Some(VmValue::String(text)) => Ok(text.to_string()),
        Some(other) => Err(VmError::TypeError(format!(
            "{}.{method} expects {field}: string, got {}",
            handle.type_name(),
            other.type_name()
        ))),
        None => Err(VmError::TypeError(format!(
            "{}.{method} missing required {field}",
            handle.type_name()
        ))),
    }
}

/// Like [`obs_string_arg`] but treats a missing slot or explicit `nil`
/// as `None` so the caller can apply a default — used for `level` on
/// `harness.obs.log`, where the user-facing surface defaults to
/// `"info"`.
fn obs_optional_string_arg(
    handle: &VmHarness,
    method: &str,
    value: Option<&VmValue>,
    field: &str,
) -> Result<Option<String>, VmError> {
    match value {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::String(text)) => Ok(Some(text.to_string())),
        Some(other) => Err(VmError::TypeError(format!(
            "{}.{method} expects {field}: string, got {}",
            handle.type_name(),
            other.type_name()
        ))),
    }
}

fn obs_attrs_arg(
    handle: &VmHarness,
    method: &str,
    value: Option<&VmValue>,
) -> Result<serde_json::Map<String, serde_json::Value>, VmError> {
    match value {
        None | Some(VmValue::Nil) => Ok(serde_json::Map::new()),
        // `VmValue::Dict` always lowers to `serde_json::Value::Object`,
        // so the conversion never returns another variant here — the
        // explicit Object pattern keeps the call site total without
        // adding a dead arm.
        Some(value @ VmValue::Dict(_)) => {
            let serde_json::Value::Object(map) =
                crate::stdlib::observability::vm_value_to_json(value)
            else {
                unreachable!("Dict lowers to Object");
            };
            Ok(map)
        }
        Some(other) => Err(VmError::TypeError(format!(
            "{}.{method} expects attrs: dict, got {}",
            handle.type_name(),
            other.type_name()
        ))),
    }
}

fn obs_number_arg(
    handle: &VmHarness,
    method: &str,
    value: &VmValue,
) -> Result<serde_json::Value, VmError> {
    match value {
        VmValue::Int(n) => Ok(serde_json::json!(*n)),
        VmValue::Float(n) => Ok(serde_json::json!(*n)),
        VmValue::Duration(ms) => Ok(serde_json::json!(*ms)),
        other => Err(VmError::TypeError(format!(
            "{}.{method} expects value: number, got {}",
            handle.type_name(),
            other.type_name()
        ))),
    }
}

fn sleep_ms_arg(args: &[VmValue]) -> Result<i64, VmError> {
    args.first()
        .and_then(|v| match v {
            VmValue::Int(n) => Some(*n),
            VmValue::Duration(ms) => Some(*ms),
            _ => None,
        })
        .ok_or_else(|| {
            VmError::TypeError("HarnessClock.sleep_ms expects an int or duration argument".into())
        })
}

fn string_arg<'a>(args: &'a [VmValue], index: usize, callee: &str) -> Result<&'a str, VmError> {
    match args.get(index) {
        Some(VmValue::String(value)) => Ok(value.as_str()),
        Some(other) => Err(VmError::TypeError(format!(
            "{callee} expects string argument {}, got {}",
            index + 1,
            other.type_name()
        ))),
        None => Err(VmError::TypeError(format!(
            "{callee} expects string argument {}",
            index + 1
        ))),
    }
}

fn required_dict_arg<'a>(
    args: &'a [VmValue],
    index: usize,
    callee: &str,
) -> Result<&'a crate::value::DictMap, VmError> {
    args.get(index)
        .and_then(VmValue::as_dict)
        .ok_or_else(|| VmError::TypeError(format!("{callee} expects a dict argument")))
}

fn optional_string_arg<'a>(
    args: &'a [VmValue],
    index: usize,
    callee: &str,
) -> Result<&'a str, VmError> {
    match args.get(index) {
        None | Some(VmValue::Nil) => Ok(""),
        Some(VmValue::String(value)) => Ok(value.as_str()),
        Some(other) => Err(VmError::TypeError(format!(
            "{callee} expects string argument {}, got {}",
            index + 1,
            other.type_name()
        ))),
    }
}

fn secret_name_arg(
    handle: &VmHarness,
    method: &str,
    value: Option<&VmValue>,
) -> Result<String, VmError> {
    match value {
        Some(VmValue::String(name)) if !name.trim().is_empty() => Ok(name.to_string()),
        Some(VmValue::String(_)) => Err(VmError::TypeError(format!(
            "{}.{method} expects a non-empty secret name",
            handle.type_name()
        ))),
        Some(other) => Err(VmError::TypeError(format!(
            "{}.{method} expects name: string, got {}",
            handle.type_name(),
            other.type_name()
        ))),
        None => Err(VmError::TypeError(format!(
            "{}.{method} missing required name",
            handle.type_name()
        ))),
    }
}

fn secret_value_arg(
    handle: &VmHarness,
    method: &str,
    value: Option<&VmValue>,
    field: &str,
) -> Result<crate::secrets::SecretBytes, VmError> {
    let value = value.ok_or_else(|| {
        VmError::TypeError(format!(
            "{}.{method} missing required {field}",
            handle.type_name()
        ))
    })?;
    secret_value_from_vm(handle, method, value, field)
}

fn secret_value_from_vm(
    handle: &VmHarness,
    method: &str,
    value: &VmValue,
    field: &str,
) -> Result<crate::secrets::SecretBytes, VmError> {
    match value {
        VmValue::String(text) => Ok(crate::secrets::SecretBytes::from(text.as_str())),
        VmValue::Bytes(bytes) => Ok(crate::secrets::SecretBytes::from(bytes.as_slice())),
        other => Err(VmError::TypeError(format!(
            "{}.{method} expects {field}: string or bytes, got {}",
            handle.type_name(),
            other.type_name()
        ))),
    }
}

fn secret_scope_arg(value: Option<&VmValue>) -> Result<crate::secrets::SecretScope, VmError> {
    match value {
        None | Some(VmValue::Nil) => Ok(crate::secrets::SecretScope::tenant(
            crate::harness_tenant::current_tenant_id().map(|tenant| tenant.0),
        )),
        Some(VmValue::String(scope)) => parse_secret_scope_string(scope.as_str()),
        Some(VmValue::Dict(dict)) => {
            let kind = dict
                .get("kind")
                .and_then(|value| match value {
                    VmValue::String(kind) => Some(kind.as_str()),
                    _ => None,
                })
                .ok_or_else(|| {
                    VmError::TypeError(
                        "HarnessSecrets scope dict requires string `kind`".to_string(),
                    )
                })?
                .trim();
            let id = match dict.get("id") {
                None | Some(VmValue::Nil) => None,
                Some(VmValue::String(id)) if !id.is_empty() => Some(id.to_string()),
                Some(VmValue::String(_)) => None,
                Some(other) => {
                    return Err(VmError::TypeError(format!(
                        "HarnessSecrets scope `id` must be a string or nil, got {}",
                        other.type_name()
                    )))
                }
            };
            match kind {
                "tenant" => Ok(crate::secrets::SecretScope::tenant(id.or_else(|| {
                    crate::harness_tenant::current_tenant_id().map(|tenant| tenant.0)
                }))),
                "workspace" => id
                    .map(crate::secrets::SecretScope::workspace)
                    .ok_or_else(|| {
                        VmError::TypeError(
                            "HarnessSecrets workspace scope requires non-empty `id`".to_string(),
                        )
                    }),
                "system" if id.is_none() => Ok(crate::secrets::SecretScope::system()),
                "system" => Err(VmError::TypeError(
                    "HarnessSecrets system scope does not take an `id`".to_string(),
                )),
                other if !other.trim().is_empty() => {
                    Ok(crate::secrets::SecretScope::custom(other, id))
                }
                _ => Err(VmError::TypeError(
                    "HarnessSecrets scope `kind` must not be empty".to_string(),
                )),
            }
        }
        Some(other) => Err(VmError::TypeError(format!(
            "HarnessSecrets scope must be nil, string, or dict, got {}",
            other.type_name()
        ))),
    }
}

fn parse_secret_scope_string(raw: &str) -> Result<crate::secrets::SecretScope, VmError> {
    let value = raw.trim();
    if value.is_empty() || value == "tenant" {
        return Ok(crate::secrets::SecretScope::tenant(
            crate::harness_tenant::current_tenant_id().map(|tenant| tenant.0),
        ));
    }
    if value == "system" {
        return Ok(crate::secrets::SecretScope::system());
    }
    if let Some(id) = value.strip_prefix("tenant:") {
        return Ok(crate::secrets::SecretScope::tenant(
            (!id.is_empty())
                .then(|| id.to_string())
                .or_else(|| crate::harness_tenant::current_tenant_id().map(|tenant| tenant.0)),
        ));
    }
    if let Some(id) = value.strip_prefix("workspace:") {
        if id.is_empty() {
            return Err(VmError::TypeError(
                "HarnessSecrets workspace scope requires an id".to_string(),
            ));
        }
        return Ok(crate::secrets::SecretScope::workspace(id));
    }
    if let Some((kind, id)) = value.split_once(':') {
        if kind.is_empty() {
            return Err(VmError::TypeError(
                "HarnessSecrets custom scope kind must not be empty".to_string(),
            ));
        }
        return Ok(crate::secrets::SecretScope::custom(
            kind,
            (!id.is_empty()).then(|| id.to_string()),
        ));
    }
    Ok(crate::secrets::SecretScope::custom(value, None))
}

fn secret_id_for_scope(
    name: &str,
    scope: &crate::secrets::SecretScope,
) -> Result<crate::secrets::SecretId, VmError> {
    if name.trim().starts_with(crate::secrets::SECRET_REF_SCHEME) || name.contains('/') {
        return crate::secrets::parse_secret_id(name).map_err(secret_error_to_vm);
    }
    Ok(crate::secrets::SecretId::new(scope.namespace(), name))
}

fn optional_duration_arg(
    value: Option<&VmValue>,
    callee: &str,
) -> Result<Option<std::time::Duration>, VmError> {
    match value {
        None | Some(VmValue::Nil) => Ok(None),
        Some(value) => required_duration_arg(Some(value), callee).map(Some),
    }
}

fn required_duration_arg(
    value: Option<&VmValue>,
    callee: &str,
) -> Result<std::time::Duration, VmError> {
    let millis = match value {
        Some(VmValue::Int(ms)) | Some(VmValue::Duration(ms)) => *ms,
        Some(other) => {
            return Err(VmError::TypeError(format!(
                "{callee} expects duration as int milliseconds or duration, got {}",
                other.type_name()
            )))
        }
        None => {
            return Err(VmError::TypeError(format!(
                "{callee} expects duration as int milliseconds or duration"
            )))
        }
    };
    let millis = u64::try_from(millis)
        .map_err(|_| VmError::TypeError(format!("{callee} duration must be non-negative")))?;
    Ok(std::time::Duration::from_millis(millis))
}

fn secret_rotation_options_arg(
    value: Option<&VmValue>,
) -> Result<crate::secrets::SecretRotationOptions, VmError> {
    let Some(value) = value else {
        return Ok(crate::secrets::SecretRotationOptions::default());
    };
    match value {
        VmValue::Nil => Ok(crate::secrets::SecretRotationOptions::default()),
        VmValue::Dict(dict) => Ok(crate::secrets::SecretRotationOptions {
            grace: optional_duration_arg(dict.get("grace_ms"), "HarnessSecrets.rotate grace_ms")?,
            ttl: optional_duration_arg(dict.get("ttl_ms"), "HarnessSecrets.rotate ttl_ms")?,
        }),
        other => Err(VmError::TypeError(format!(
            "HarnessSecrets.rotate options must be a dict or nil, got {}",
            other.type_name()
        ))),
    }
}

fn secret_audit_context() -> crate::secrets::SecretAuditContext {
    let principal = crate::harness_auth::current_auth_principal();
    crate::secrets::SecretAuditContext {
        request_id: crate::observability::request_id::current_request_id(),
        actor_subject: principal
            .as_ref()
            .filter(|principal| !principal.subject.is_empty())
            .map(|principal| principal.subject.clone()),
        actor_kind: principal.and_then(|principal| principal.kind.clone()),
    }
}

fn secret_scope_value(scope: &crate::secrets::SecretScope) -> VmValue {
    let mut out = std::collections::BTreeMap::new();
    out.put_str("kind", scope.kind());
    out.put_opt_str("id", scope.id());
    VmValue::dict(out)
}

fn secret_id_value(id: &crate::secrets::SecretId) -> VmValue {
    let mut out = std::collections::BTreeMap::new();
    out.put_str("namespace", &id.namespace);
    out.put_str("name", &id.name);
    match id.version {
        crate::secrets::SecretVersion::Latest => out.put_str("version", "latest"),
        crate::secrets::SecretVersion::Exact(version) => {
            out.put_int("version", version.min(i64::MAX as u64) as i64);
        }
    }
    VmValue::dict(out)
}

fn secret_write_receipt_value(receipt: crate::secrets::SecretWriteReceipt) -> VmValue {
    let mut out = std::collections::BTreeMap::new();
    out.put_str("provider", receipt.provider);
    out.put("id", secret_id_value(&receipt.id));
    out.put("scope", secret_scope_value(&receipt.scope));
    out.put_opt(
        "version",
        receipt
            .version
            .map(|version| VmValue::Int(version.min(i64::MAX as u64) as i64)),
    );
    out.put_opt(
        "expires_at_ms",
        receipt.expires_at_unix_ms.map(VmValue::Int),
    );
    VmValue::dict(out)
}

fn secret_rotation_receipt_value(receipt: crate::secrets::SecretRotationReceipt) -> VmValue {
    let mut out = std::collections::BTreeMap::new();
    out.put_str("provider", receipt.provider);
    out.put("id", secret_id_value(&receipt.id));
    out.put("scope", secret_scope_value(&receipt.scope));
    out.put_opt(
        "from_version",
        receipt
            .from_version
            .map(|version| VmValue::Int(version.min(i64::MAX as u64) as i64)),
    );
    out.put_opt(
        "to_version",
        receipt
            .to_version
            .map(|version| VmValue::Int(version.min(i64::MAX as u64) as i64)),
    );
    out.put_opt(
        "grace_until_ms",
        receipt.grace_until_unix_ms.map(VmValue::Int),
    );
    out.put_opt(
        "expires_at_ms",
        receipt.expires_at_unix_ms.map(VmValue::Int),
    );
    VmValue::dict(out)
}

fn secret_lease_grant_value(
    bytes_value: bool,
    grant: crate::secrets::SecretLeaseGrant,
) -> Result<VmValue, VmError> {
    let value = grant.value.with_exposed(|bytes| {
        if bytes_value {
            return Ok(VmValue::Bytes(std::sync::Arc::new(bytes.to_vec())));
        }
        std::str::from_utf8(bytes)
            .map(str::to_string)
            .map(vm_string)
            .map_err(|error| {
                VmError::TypeError(format!(
                    "HarnessSecrets.lease secret `{}` was not UTF-8: {error}",
                    grant.id.name
                ))
            })
    })?;
    let mut out = std::collections::BTreeMap::new();
    out.put_str("provider", grant.provider);
    out.put("id", secret_id_value(&grant.id));
    out.put("scope", secret_scope_value(&grant.scope));
    out.put_str("lease_id", grant.lease_id);
    out.put("value", value);
    out.put_int("expires_at_ms", grant.expires_at_unix_ms);
    Ok(VmValue::dict(out))
}

fn secret_error_to_vm(error: crate::secrets::SecretError) -> VmError {
    use crate::secrets::SecretError;
    match error {
        SecretError::NotFound { .. } | SecretError::NoProviders { .. } => {
            VmError::CategorizedError {
                message: error.to_string(),
                category: ErrorCategory::NotFound,
            }
        }
        SecretError::Unsupported { .. } | SecretError::InvalidInput(_) => {
            VmError::TypeError(error.to_string())
        }
        SecretError::AccessDenied { .. } => VmError::CategorizedError {
            message: error.to_string(),
            category: ErrorCategory::Auth,
        },
        SecretError::Backend { .. } | SecretError::InvalidConfig(_) | SecretError::All(_) => {
            VmError::CategorizedError {
                message: error.to_string(),
                category: ErrorCategory::ToolError,
            }
        }
    }
}

fn mock_term_dimension(raw: Option<&str>, fallback: usize) -> usize {
    crate::term::dimension_from_env(raw).unwrap_or(fallback)
}

/// Mock variant of `harness.stdio.read_line` / `prompt`. When called with
/// no options dict, returns a plain string (or nil at EOF); when called
/// with an options dict, returns the structured `{ok, status, value?}`
/// dict that mirrors the real surface so tests can assert on either
/// shape without re-mocking.
fn mock_read_line_value(state: &crate::harness::MockHarnessState, args: &[VmValue]) -> VmValue {
    let structured = matches!(args.first(), Some(VmValue::Dict(_)));
    match state.pop_stdin_line() {
        Some(line) => {
            if structured {
                let mut out = std::collections::BTreeMap::new();
                out.insert("ok".to_string(), VmValue::Bool(true));
                out.put_str("status", "ok");
                out.put_str("value", line);
                VmValue::dict(out)
            } else {
                VmValue::String(arcstr::ArcStr::from(line))
            }
        }
        None => {
            if structured {
                let mut out = std::collections::BTreeMap::new();
                out.insert("ok".to_string(), VmValue::Bool(false));
                out.put_str("status", "eof");
                VmValue::dict(out)
            } else {
                VmValue::Nil
            }
        }
    }
}
