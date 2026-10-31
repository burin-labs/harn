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
        if (self.machine_spend_quota.is_some() || session_field.is_some()) && control.is_none() {
            return Err(harn_vm::VmError::TypeError(
                "session/set_budget requires a known sessionId".into(),
            ));
        }
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
            Ok(applied) => {
                if applied {
                    if let Some(id) = msg.get("id") {
                        output.write_line(
                            &serde_json::json!({
                                "jsonrpc": "2.0", "id": id, "result": {}
                            })
                            .to_string(),
                        );
                    }
                }
                applied
            }
            Err(error) => {
                if let Some(id) = msg.get("id") {
                    let code = if matches!(&error, harn_vm::VmError::TypeError(_)) {
                        -32602
                    } else {
                        -32000
                    };
                    send_routed_error(output, id, code, &error.to_string());
                } else {
                    tracing::warn!(%error, "Refused session budget notification");
                }
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
        let messages = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let captured = messages.clone();
        let output = AcpOutput::callback(move |line| {
            captured.lock().unwrap().push(line.to_string());
        });
        assert!(controls.apply_budget_rearm_with_output(&refused, &output));
        assert!(
            messages.lock().unwrap().is_empty(),
            "notifications have no response"
        );
        let mut request = refused;
        request["id"] = json!(42);
        assert!(controls.apply_budget_rearm_with_output(&request, &output));
        let messages = messages.lock().unwrap();
        assert_eq!(
            messages.len(),
            1,
            "the same failure must respond to a request"
        );
        let response: serde_json::Value = serde_json::from_str(&messages[0]).unwrap();
        assert_eq!(response["id"], 42);
        assert_eq!(response["error"]["code"], -32000);
        assert_eq!(
            control.llm_spend.lock().unwrap().llm_cost_usd,
            Some(CeilingRearm::Set(2.0))
        );
    }

    #[test]
    fn machine_quota_rearm_requires_a_known_session() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("spend.sqlite");
        let controls = ConcurrentSessionControls {
            machine_spend_quota: Some(
                harn_vm::llm::MachineSpendQuota::open(
                    &path,
                    "person",
                    harn_vm::llm::MachineSpendPolicy {
                        daily_limit_microusd: None,
                        monthly_limit_microusd: Some(1_000_000),
                        lifetime_limit_microusd: None,
                    },
                )
                .unwrap(),
            ),
            ..Default::default()
        };
        let control = ConcurrentSessionControl::new();
        controls.register("target", control.clone());
        let _ambient = harn_vm::install_llm_cost_budget_seeded(Some(5.0), 0.0);
        for mut params in [
            json!({}),
            json!({"sessionId":null}),
            json!({"sessionId":"missing"}),
        ] {
            params["llm_cost_usd"] = json!(9.0);
            let frame =
                json!({"jsonrpc":"2.0", "id":7, "method":"session/set_budget", "params":params});
            assert!(matches!(
                controls.try_apply_budget_rearm(&frame),
                Err(harn_vm::VmError::TypeError(_))
            ));
            let messages = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
            let captured = messages.clone();
            let output = AcpOutput::callback(move |line| {
                captured.lock().unwrap().push(line.to_string());
            });
            assert!(controls.apply_budget_rearm_with_output(&frame, &output));
            let messages = messages.lock().unwrap();
            assert_eq!(messages.len(), 1);
            let response: serde_json::Value = serde_json::from_str(&messages[0]).unwrap();
            assert_eq!(response["id"], 7);
            assert_eq!(response["error"]["code"], -32602);
            assert_eq!(harn_vm::peek_llm_cost_budget(), Some(5.0));
            assert_eq!(control.llm_spend.lock().unwrap().llm_cost_usd, None);
        }
        let count: i64 = rusqlite::Connection::open(&path)
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM spend_session_budget_audit",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0, "no session was approved");
    }
}
