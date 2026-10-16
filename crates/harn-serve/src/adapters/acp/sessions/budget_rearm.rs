//! Authenticated session ceiling updates and their durable audit boundary.
use super::*;
use harn_vm::llm::BudgetCeilingUpdate;

impl ConcurrentSessionControls {
    /// Re-arm a live session and retain the ceiling for subsequent turns.
    pub(in crate::adapters::acp) fn try_apply_budget_rearm(
        &self,
        msg: &serde_json::Value,
    ) -> Result<bool, harn_vm::VmError> {
        let Some(rearm) = BudgetRearm::parse(msg) else {
            return Ok(false);
        };
        if !self.authenticated.load(Ordering::SeqCst) {
            return Ok(false);
        }
        let session_field = msg.get("params").and_then(|params| params.get("sessionId"));
        let session_id = session_field.and_then(serde_json::Value::as_str);
        let control = session_id.and_then(|session_id| {
            self.sessions
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .get(session_id)
                .cloned()
        });
        if let Some(control) = control {
            if let Some(quota) = &self.machine_spend_quota {
                quota.record_session_budget_update(
                    session_id.expect("named session control"),
                    "authenticated-acp-client",
                    rearm
                        .llm_cost_usd
                        .map_or(BudgetCeilingUpdate::Unchanged, |ceiling| {
                            BudgetCeilingUpdate::from_ceiling(ceiling.cap())
                        }),
                    rearm
                        .llm_tokens
                        .map_or(BudgetCeilingUpdate::Unchanged, |ceiling| {
                            BudgetCeilingUpdate::from_ceiling(ceiling.token_cap())
                        }),
                )?;
            }
            let mut spend = control
                .llm_spend
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if rearm.llm_cost_usd.is_some() {
                if let (Some(ceiling), Some(cost)) = (rearm.llm_cost_usd, &spend.active_cost) {
                    cost.set_ceiling(ceiling.cap());
                }
                spend.llm_cost_usd = rearm.llm_cost_usd;
            }
            if rearm.llm_tokens.is_some() {
                if let (Some(ceiling), Some(tokens)) = (rearm.llm_tokens, &spend.active_tokens) {
                    tokens.set_ceiling(ceiling.token_cap());
                }
                spend.llm_tokens = rearm.llm_tokens;
            }
        } else if session_field.is_none() && self.machine_spend_quota.is_none() {
            rearm.apply_live();
        }
        Ok(true)
    }

    pub(in crate::adapters::acp) fn apply_budget_rearm_with_output(
        &self,
        msg: &serde_json::Value,
        output: &AcpOutput,
    ) -> bool {
        match self.try_apply_budget_rearm(msg) {
            Ok(applied) => applied,
            Err(error) => {
                send_routed_error(
                    output,
                    msg.get("id").unwrap_or(&serde_json::Value::Null),
                    -32000,
                    &error.to_string(),
                );
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn machine_quota_rearm_requires_authentication_and_a_durable_receipt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("spend.sqlite");
        let quota = harn_vm::llm::MachineSpendQuota::open(
            &path,
            "person",
            harn_vm::llm::MachineSpendPolicy {
                daily_limit_microusd: None,
                monthly_limit_microusd: Some(1_000_000),
                lifetime_limit_microusd: None,
            },
        )
        .unwrap();
        let mut controls = ConcurrentSessionControls::new(false, json!({}));
        controls.machine_spend_quota = Some(quota.clone());
        let control = ConcurrentSessionControl::new();
        controls.register("target", control.clone());
        let frame = json!({"method":"session/set_budget", "params":{"sessionId":"target", "llm_cost_usd":2.0, "llm_tokens":null}});
        assert!(!controls.try_apply_budget_rearm(&frame).unwrap());
        assert_eq!(control.llm_spend.lock().unwrap().llm_cost_usd, None);
        controls.mark_authenticated();
        assert!(controls.try_apply_budget_rearm(&frame).unwrap());
        assert_eq!(
            control.llm_spend.lock().unwrap().llm_cost_usd,
            Some(CeilingRearm::Set(2.0))
        );
        let db = rusqlite::Connection::open(&path).unwrap();
        let receipt: (String, f64, bool, Option<i64>) = db.query_row(
            "SELECT approved_by, cost_usd, tokens_changed, tokens FROM spend_session_budget_audit WHERE session_id = 'target' AND scope = 'person'",
            [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        ).unwrap();
        assert_eq!(
            receipt,
            ("authenticated-acp-client".into(), 2.0, true, None)
        );
        assert_eq!(
            quota.policy().unwrap().monthly_limit_microusd,
            Some(1_000_000)
        );
        db.execute_batch("CREATE TRIGGER refuse_budget_audit BEFORE INSERT ON spend_session_budget_audit BEGIN SELECT RAISE(ABORT, 'audit unavailable'); END;").unwrap();
        let refused = json!({"method":"session/set_budget", "params":{"sessionId":"target", "llm_cost_usd":3.0}});
        assert!(controls.try_apply_budget_rearm(&refused).is_err());
        assert_eq!(
            control.llm_spend.lock().unwrap().llm_cost_usd,
            Some(CeilingRearm::Set(2.0))
        );
    }
}
