//! The `ReminderInject` handler (#1876): render the binding\'s body template
//! against the event and inject the result into a running agent session.

use super::*;

impl Dispatcher {
    /// ReminderInject (#1876) — resolve the target running session, render
    /// the body template against the event payload, build a `SystemReminder`
    /// from the binding's reminder metadata, and inject it via the existing
    /// `agent_sessions::inject_reminder` pipeline. Missing-target dispatches
    /// emit a `triggers.reminder_inject.audit` audit entry tagged
    /// `target_missing` and return a `dropped` outcome instead of failing
    /// the dispatch — that matches the "drop the reminder + emit audit"
    /// failure-mode contract in the #1876 spec.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn dispatch_reminder_inject(
        &self,
        binding: &TriggerBinding,
        route: &DispatchUri,
        event: &TriggerEvent,
        target: &TargetExpr,
        body_template: &str,
        tags: &[String],
        ttl_turns: Option<i64>,
        dedupe_key: Option<&str>,
        propagate: crate::llm::helpers::ReminderPropagate,
        role_hint: crate::llm::helpers::ReminderRoleHint,
        preserve_on_compact: bool,
        autonomy_tier: AutonomyTier,
        wait_lease: Option<DispatchWaitLease>,
        cancel_rx: &mut broadcast::Receiver<()>,
    ) -> Result<DispatchCallResult, DispatchError> {
        // Step 1: resolve the target session. Closure resolution runs through
        // the standard VM invocation path so dispatch context, cancellation,
        // and policy intersection apply uniformly with other handler variants.
        let resolved_target = match target {
            TargetExpr::Current => crate::agent_sessions::current_session_id(),
            TargetExpr::Parent => crate::agent_sessions::current_session_id()
                .and_then(|id| crate::agent_sessions::parent_id(&id)),
            TargetExpr::Concrete(id) => Some(id.clone()),
            TargetExpr::Closure(closure) => {
                let value = self
                    .invoke_vm_callable(
                        &crate::value::VmCallable::Eager(Arc::clone(closure)),
                        &binding.binding_key(),
                        event,
                        None,
                        binding.id.as_str(),
                        &event.qualified_kind(),
                        autonomy_tier,
                        wait_lease,
                        cancel_rx,
                    )
                    .await?;
                match value {
                    crate::value::VmValue::String(s) => {
                        let id = s.trim().to_string();
                        if id.is_empty() {
                            None
                        } else {
                            Some(id)
                        }
                    }
                    crate::value::VmValue::Nil => None,
                    other => {
                        return Err(DispatchError::Local(format!(
                            "trigger '{}': ReminderInject target closure must return a string session id or nil, got {}",
                            binding.id.as_str(),
                            other.type_name()
                        )));
                    }
                }
            }
        };

        // Step 2: render the body template. Bindings expose `event` (with the
        // full serialized event), `match` (matched_at), and a top-level
        // `batch` projection when flow-control batching turned this dispatch
        // into a synthetic batched event. Missing identifiers preserve the
        // pre-v2 silent-passthrough contract in the template engine, so
        // body strings without `{{ ... }}` round-trip unchanged.
        let event_json =
            serde_json::to_value(event).map_err(|error| DispatchError::Serde(error.to_string()))?;
        let mut bindings = crate::value::DictMap::new();
        bindings.insert(
            crate::value::intern_key("event"),
            crate::schema::json_to_vm_value(&event_json),
        );
        bindings.insert(
            crate::value::intern_key("match"),
            crate::schema::json_to_vm_value(&serde_json::json!({
                "matched_at": event_json.get("received_at").cloned().unwrap_or(serde_json::Value::Null),
            })),
        );
        if let Some(batch_value) = event_json.get("batch").or_else(|| {
            event_json
                .get("provider_payload")
                .and_then(|payload| payload.get("batch"))
        }) {
            bindings.insert(
                crate::value::intern_key("batch"),
                crate::schema::json_to_vm_value(batch_value),
            );
        }
        let rendered_body = crate::stdlib::template::render_template_to_string(
            body_template,
            Some(&bindings),
            None,
            None,
        )
        .map_err(|error| {
            DispatchError::Local(format!(
                "trigger '{}': ReminderInject template render failed: {error}",
                binding.id.as_str()
            ))
        })?;

        // Step 3: handle missing target as a graceful drop with audit + a
        // `dropped` dispatch result. The action-graph success_outcome maps
        // `status: "dropped"` to a distinct `dropped` label so observers
        // can tell a no-op from a successful inject without re-reading the
        // result blob.
        let target_kind = target.kind();
        let Some(target_session_id) = resolved_target else {
            crate::orchestration::record_lifecycle_audit(
                "triggers.reminder_inject.audit",
                serde_json::json!({
                    "trigger_id": binding.id.as_str(),
                    "binding_key": binding.binding_key(),
                    "event_id": event.id.0,
                    "outcome": "dropped",
                    "reason": "target_missing",
                    "target_kind": target_kind,
                }),
            );
            let mut metadata = route.dispatch_boundary_metadata();
            metadata.insert("target_kind".to_string(), serde_json::json!(target_kind));
            metadata.insert("status".to_string(), serde_json::json!("dropped"));
            metadata.insert(
                "rejection_reason".to_string(),
                serde_json::json!("target_missing"),
            );
            let mut output = serde_json::Map::new();
            output.insert("status".to_string(), serde_json::json!("dropped"));
            output.insert("reason".to_string(), serde_json::json!("target_missing"));
            output.insert("target_kind".to_string(), serde_json::json!(target_kind));
            return Ok(DispatchCallResult {
                output: serde_json::Value::Object(output),
                metadata,
            });
        };

        // Step 4: target must exist as a registered session. Same graceful
        // drop semantics as a fully-missing target.
        if !crate::agent_sessions::exists(&target_session_id) {
            crate::orchestration::record_lifecycle_audit(
                "triggers.reminder_inject.audit",
                serde_json::json!({
                    "trigger_id": binding.id.as_str(),
                    "binding_key": binding.binding_key(),
                    "event_id": event.id.0,
                    "outcome": "dropped",
                    "reason": "target_unknown_session",
                    "target_kind": target_kind,
                    "target_session_id": &target_session_id,
                }),
            );
            let mut metadata = route.dispatch_boundary_metadata();
            metadata.insert("target_kind".to_string(), serde_json::json!(target_kind));
            metadata.insert(
                "target_session_id".to_string(),
                serde_json::json!(&target_session_id),
            );
            metadata.insert("status".to_string(), serde_json::json!("dropped"));
            metadata.insert(
                "rejection_reason".to_string(),
                serde_json::json!("target_unknown_session"),
            );
            let mut output = serde_json::Map::new();
            output.insert("status".to_string(), serde_json::json!("dropped"));
            output.insert(
                "reason".to_string(),
                serde_json::json!("target_unknown_session"),
            );
            output.insert("target_kind".to_string(), serde_json::json!(target_kind));
            output.insert(
                "target_session_id".to_string(),
                serde_json::json!(&target_session_id),
            );
            return Ok(DispatchCallResult {
                output: serde_json::Value::Object(output),
                metadata,
            });
        }

        // Step 5: build the SystemReminder + inject via the canonical
        // agent_sessions pipeline. Reminders are sourced as `in_pipeline`
        // so the rendering machinery treats them the same as any other
        // dispatch-side reminder.
        let reminder = crate::llm::helpers::SystemReminder {
            id: uuid::Uuid::now_v7().to_string(),
            tags: tags.to_vec(),
            dedupe_key: dedupe_key.map(str::to_string),
            ttl_turns,
            preserve_on_compact,
            propagate,
            role_hint,
            source: crate::llm::helpers::ReminderSource::InPipeline,
            body: rendered_body,
            fired_at_turn: 0,
            originating_agent_id: None,
        };
        let reminder_id = reminder.id.clone();
        let report = crate::agent_sessions::inject_reminder(&target_session_id, reminder)
            .map_err(DispatchError::Local)?;

        crate::orchestration::record_lifecycle_audit(
            "triggers.reminder_inject.audit",
            serde_json::json!({
                "trigger_id": binding.id.as_str(),
                "binding_key": binding.binding_key(),
                "event_id": event.id.0,
                "outcome": "injected",
                "target_kind": target_kind,
                "target_session_id": &target_session_id,
                "reminder_id": &reminder_id,
                "deduped_count": report.deduped_count,
            }),
        );

        let mut metadata = route.dispatch_boundary_metadata();
        metadata.insert("target_kind".to_string(), serde_json::json!(target_kind));
        metadata.insert(
            "target_session_id".to_string(),
            serde_json::json!(&target_session_id),
        );
        metadata.insert("status".to_string(), serde_json::json!("injected"));
        metadata.insert("reminder_id".to_string(), serde_json::json!(&reminder_id));
        if report.deduped_count > 0 {
            metadata.insert(
                "deduped_count".to_string(),
                serde_json::json!(report.deduped_count),
            );
        }
        let mut output = serde_json::Map::new();
        output.insert("status".to_string(), serde_json::json!("injected"));
        output.insert(
            "target_session_id".to_string(),
            serde_json::json!(&target_session_id),
        );
        output.insert("target_kind".to_string(), serde_json::json!(target_kind));
        output.insert("reminder_id".to_string(), serde_json::json!(&reminder_id));
        output.insert(
            "deduped_count".to_string(),
            serde_json::json!(report.deduped_count),
        );
        Ok(DispatchCallResult {
            output: serde_json::Value::Object(output),
            metadata,
        })
    }
}
