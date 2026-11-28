use super::*;
use harn_vm::agent_events::register_sink;

struct ResetCompaction;
impl Drop for ResetCompaction {
    fn drop(&mut self) {
        harn_vm::agent_sessions::reset_default_transcript_budget_policy();
        harn_vm::llm::clear_cli_llm_mock_mode();
    }
}

#[test]
fn actual_budget_compaction_projects_its_persisted_receipt_identity() {
    harn_vm::reset_thread_local_state();
    let mock = harn_vm::llm::parse_llm_mock_value(
        &serde_json::json!({"text":"Canonical compacted summary"}),
    )
    .unwrap();
    harn_vm::llm::install_cli_llm_mocks(vec![mock]);
    let _reset = ResetCompaction;
    harn_vm::agent_sessions::set_default_transcript_budget_policy(
        harn_vm::agent_sessions::SessionTranscriptBudgetPolicy::compact(3, 5, 1),
    );
    let session = harn_vm::agent_sessions::open_or_create(None).unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut server = AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));
    server.handle_initialize(
        &serde_json::json!(1),
        &serde_json::json!({"clientCapabilities":{
            "session":{"compaction":{}}
        }}),
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&rx.try_recv().unwrap()).unwrap()["id"],
        1
    );
    register_sink(
        &session,
        Arc::new(AcpAgentEventSink::new(server.output.clone())),
    );
    for (role, text) in [
        ("user", "one"),
        ("assistant", "two"),
        ("user", "three"),
        ("assistant", "four"),
    ] {
        harn_vm::agent_sessions::inject_message(&session, make_acp_test_message(role, text))
            .unwrap();
    }
    let transcript =
        harn_vm::llm::vm_value_to_json(&harn_vm::agent_sessions::transcript(&session).unwrap());
    let receipts: Vec<_> = transcript["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["kind"] == "compaction")
        .collect();
    assert_eq!(
        receipts.len(),
        1,
        "the canonical engine must actually compact"
    );
    assert_eq!(receipts[0]["metadata"]["engine_strategy"], "llm");
    let mut updates = Vec::new();
    while let Ok(line) = rx.try_recv() {
        let frame: serde_json::Value = serde_json::from_str(&line).unwrap();
        if frame["method"] == "session/update" {
            updates.push(frame["params"].clone());
        }
    }
    assert_eq!(updates.len(), 1);
    let params = &updates[0];
    assert_eq!(params["sessionId"], session);
    assert_eq!(params["update"]["sessionUpdate"], "compaction_update");
    assert_eq!(params["update"]["compactionId"], receipts[0]["id"]);
    assert_eq!(params["update"]["status"], "completed");
    let validator =
        jsonschema::draft202012::new(&super::update_negotiation::upstream_session_update_schema())
            .unwrap();
    assert!(validator.is_valid(params), "{params}");
    harn_vm::agent_sessions::close(&session);
}
