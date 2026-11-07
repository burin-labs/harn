use std::collections::BTreeMap;

use harn_vm::agent_events::AgentEvent;
use harn_vm::agent_session_spend::SessionLlmCost;

pub(super) struct PromptUsageView {
    session_id: String,
    prior: SessionLlmCost,
    runs: BTreeMap<String, SessionLlmCost>,
    unidentified_cost: bool,
    context: Option<(u64, u64, bool)>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settled(run: &str, input: i64, known: f64, total: Option<f64>) -> AgentEvent {
        AgentEvent::ProviderUsage {
            session_id: "s".into(), call_id: "call".into(),
            provider: "openai".into(), model: "gpt-4.1".into(),
            run_usage: Box::new(serde_json::from_value(serde_json::json!({
                "run_id":run,"parent_session_id":"","lineage_known":true,
                "input_tokens":input,"output_tokens":2,"cache_read_tokens":0,"cache_write_tokens":0,
                "provider_call_count":1,"unpriced_calls":if total.is_some(){0}else{1},
                "usage_unknown_calls":0,"known_cost_usd":known,"cost_usd":total
            })).unwrap()),
        }
    }

    #[test]
    fn resident_context_and_latest_cumulative_runs_preserve_cost_certainty() {
        let mut view = PromptUsageView::new(
            "s",
            SessionLlmCost {
                known_cost_usd: 0.25,
                cost_usd: Some(0.25),
            },
        );
        assert!(
            view.observe(&settled("r", 1000, 0.0, Some(0.0))).is_none(),
            "capacity was not measured"
        );
        assert!(
            view.observe(&AgentEvent::ContextUsage {
                session_id: "s".into(),
                used_tokens: Some(17),
                context_window: Some(128000),
                estimated: true,
            })
            .is_none(),
            "preparation does not invent a settled cost"
        );
        let zero = view.observe(&settled("r", 2000, 0.0, Some(0.0))).unwrap();
        assert_eq!(
            zero["update"]["used"], 17,
            "cumulative provider input is not resident context"
        );
        assert_eq!(zero["update"]["cost"]["amount"], 0.25);
        let priced = view.observe(&settled("r", 3000, 0.5, Some(0.5))).unwrap();
        assert_eq!(
            priced["update"]["cost"]["amount"], 0.75,
            "replace a run's cumulative snapshot"
        );
        let partial = view.observe(&settled("r2", 1, 0.125, None)).unwrap();
        assert!(partial["update"]["cost"].is_null());
        assert_eq!(partial["update"]["_meta"]["harn"]["knownCostUsd"], 0.875);
        let schema: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../conformance/protocols/schemas/acp-1.26.0-upstream.schema.json"
        )))
        .unwrap();
        let validator = jsonschema::draft202012::new(&serde_json::json!({
            "$ref":"#/$defs/SessionNotification","$defs":schema["$defs"]
        }))
        .unwrap();
        for actual in [zero, priced, partial] {
            assert!(validator.is_valid(&actual), "{actual}");
            let mut malformed = actual;
            malformed["update"].as_object_mut().unwrap().remove("used");
            assert!(!validator.is_valid(&malformed));
        }
        view.observe(&AgentEvent::ContextUsage {
            session_id: "s".into(),
            used_tokens: Some(17),
            context_window: None,
            estimated: true,
        });
        assert!(
            view.observe(&settled("r", 4000, 0.5, Some(0.5))).is_none(),
            "unknown capacity is not zero"
        );
    }
}

impl PromptUsageView {
    pub(super) fn new(session_id: &str, prior: SessionLlmCost) -> Self {
        Self {
            session_id: session_id.to_string(),
            prior,
            runs: BTreeMap::new(),
            unidentified_cost: false,
            context: None,
        }
    }

    pub(super) fn observe(&mut self, event: &AgentEvent) -> Option<serde_json::Value> {
        if event.session_id() != self.session_id {
            return None;
        }
        match event {
            AgentEvent::ContextUsage {
                used_tokens,
                context_window,
                estimated,
                ..
            } => {
                self.context = used_tokens
                    .zip(*context_window)
                    .filter(|(_, size)| *size > 0)
                    .map(|(used, size)| (used, size, *estimated));
                return None;
            }
            AgentEvent::ProviderUsage { run_usage, .. } => {
                if run_usage.run_id.is_empty() {
                    self.unidentified_cost = true;
                } else {
                    self.runs.insert(
                        run_usage.run_id.clone(),
                        SessionLlmCost {
                            known_cost_usd: run_usage.known_cost_usd,
                            cost_usd: run_usage.cost_usd,
                        },
                    );
                }
            }
            _ => return None,
        }
        let (used, size, estimated) = self.context?;
        let mut known = self.prior.known_cost_usd;
        let mut total = self.prior.cost_usd.filter(|_| !self.unidentified_cost);
        for cost in self.runs.values() {
            known += cost.known_cost_usd;
            total = total.zip(cost.cost_usd).map(|(total, cost)| total + cost);
        }
        let cost = total
            .filter(|cost| cost.is_finite() && *cost >= 0.0)
            .map(|amount| serde_json::json!({"amount": amount, "currency": "USD"}));
        Some(serde_json::json!({
            "sessionId": self.session_id,
            "update": {
                "sessionUpdate": "usage_update",
                "used": used,
                "size": size,
                "cost": cost,
                "_meta": {"harn": {
                    "contextMeasurement": if estimated { "estimated" } else { "reported" },
                    "knownCostUsd": known,
                }},
            },
        }))
    }
}
