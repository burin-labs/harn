use super::*;

fn conservative(ceiling: f64) -> BudgetSpec {
    BudgetSpec {
        llm_cost_usd: Some(ceiling),
        llm_admission: Some(harn_vm::llm::AdmissionMode::Conservative),
        ..BudgetSpec::default()
    }
}

#[test]
fn conservative_host_budget_round_trips_and_rejects_invalid_ceiling() {
    let value = budget_config_value(&conservative(0.6));
    assert_eq!(
        parse_budget_config_value(&value).unwrap(),
        SessionBudget::Custom(conservative(0.6))
    );
    for value in [
        r#"{"llm_admission":"conservative"}"#,
        r#"{"llm_admission":"unknown","llm_cost_usd":1}"#,
        r#"{"llm_admission":"conservative","llm_cost_usd":-1}"#,
    ] {
        assert!(parse_budget_config_value(value).is_err());
    }
    for value in [-1.0, f64::NAN, f64::INFINITY] {
        let config = AcpServerConfig::new(None).with_budget(conservative(value));
        assert!(config.budget.unwrap().conservative_ceiling().is_err());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn conservative_acp_prompt_uses_and_retains_the_native_allowance() {
    harn_vm::reset_thread_local_state();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("admission.harn");
    std::fs::write(
        &path,
        r#"
pipeline main(harness: Harness) {
  const refused: bool = try {
    harness.llm.call("Reply briefly", nil, {
      provider: "openai", model: "gpt-5.6-luna", max_tokens: 16,
    })
    false
  } catch (error) {
    assert(error.admission_reason == "insufficient_allowance")
    true
  }
  assert(refused, "the native host allowance must refuse before transport")
  const admission = harness.llm.session_cost()?.admission
  assert(admission != nil, "the prepared provider path must see the host ledger")
  assert(to_float(admission.ceiling_usd) == 0.01)
  assert(admission.denied_attempts >= 1)
  harness.stdio.println(to_string(admission.denied_attempts))
}
"#,
    )
    .unwrap();
    let routing = harn_vm::llm_config::parse_config_toml(
        r#"
[providers.openai]
base_url = "http://admission.invalid/v1"
auth_style = "none"
"#,
    )
    .unwrap();
    let config = AcpServerConfig::new(Some(path.to_string_lossy().into_owned()))
        .with_budget(conservative(0.01))
        .with_llm_overrides(Some(routing), None);
    tokio::task::LocalSet::new().run_until(async {
        let (request_tx, mut response_rx, server, session_id) =
            start_acp_channel_session_with_config(config, serde_json::json!(dir.path())).await;
        for request_id in [2, 3] {
            request_tx.send(serde_json::json!({
                "jsonrpc":"2.0", "id":request_id, "method":"session/prompt",
                "params":{"sessionId":session_id,"prompt":[{"type":"text","text":"Run the admission control."}]}
            })).unwrap();
            let mut terminal = None;
            let mut output = String::new();
            for _ in 0..64 {
                let message = recv_json(&mut response_rx).await;
                if message["method"] == "host/capabilities" {
                    request_tx.send(serde_json::json!({
                        "jsonrpc":"2.0", "id":message["id"], "result":{}
                    })).unwrap();
                } else if message["params"]["update"]["sessionUpdate"] == "agent_message_chunk" {
                    output.push_str(message["params"]["update"]["content"]["text"].as_str().unwrap_or_default());
                } else if message["id"] == request_id {
                    terminal = Some(message);
                    break;
                }
            }
            let terminal = terminal.expect("native prompt must emit its terminal response");
            assert!(terminal.get("error").is_none(), "{terminal}");
            let receipt: serde_json::Value = serde_json::from_str(output.trim()).unwrap();
            assert_eq!(receipt, request_id - 1,
                "successive native prompts must retain one allowance");
        }
        drop(request_tx);
        server.await.unwrap();
    }).await;
}

#[test]
fn conservative_acp_forks_share_the_parent_allowance() {
    harn_vm::reset_thread_local_state();
    let dir = tempfile::tempdir().unwrap();
    let mut server = AcpServer::new(AcpServerConfig::new(None).with_budget(conservative(0.01)));
    let session_id = "conservative-native-session";
    server
        .insert_session(
            session_id.to_string(),
            dir.path().to_path_buf(),
            SessionInfo::default(),
        )
        .unwrap();
    let retained = server.prompt_admission(session_id).unwrap().unwrap();
    server.handle_session_fork(
        &serde_json::json!(3),
        &serde_json::json!({
            "sessionId":session_id, "id":"conservative-native-fork"
        }),
    );
    let fork = server
        .sessions
        .get("conservative-native-fork")
        .expect("native session fork");
    fork.admission.as_ref().unwrap().tighten(0.005).unwrap();
    assert_eq!(
        retained.receipt().unwrap().ceiling_usd.to_string(),
        "0.005",
        "forks must share the parent allowance"
    );
    server.sessions.get_mut(session_id).unwrap().budget = SessionBudget::Unlimited;
    assert!(
        server.prompt_admission(session_id).unwrap().is_some(),
        "unlimited cannot remove an activated host ceiling"
    );
}

#[test]
fn conservative_acp_refuses_late_and_cold_restored_activation() {
    harn_vm::reset_thread_local_state();
    let dir = tempfile::tempdir().unwrap();
    let mut server = AcpServer::new(AcpServerConfig::new(None));
    server
        .insert_session(
            "late-admission".to_string(),
            dir.path().to_path_buf(),
            SessionInfo::default(),
        )
        .unwrap();
    assert!(server.prompt_admission("late-admission").unwrap().is_none());
    server.sessions.get_mut("late-admission").unwrap().budget =
        SessionBudget::Custom(conservative(1.0));
    assert!(server.prompt_admission("late-admission").is_err());
    server
        .register_restored_session("restored-admission", &serde_json::json!({"cwd":dir.path()}))
        .unwrap();
    server
        .sessions
        .get_mut("restored-admission")
        .unwrap()
        .budget = SessionBudget::Custom(conservative(1.0));
    assert!(server.prompt_admission("restored-admission").is_err());
}
