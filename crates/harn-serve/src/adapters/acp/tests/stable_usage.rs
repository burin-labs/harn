use super::*;

struct MockReset;
impl Drop for MockReset {
    fn drop(&mut self) {
        harn_vm::llm::clear_cli_llm_mock_mode();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn actual_root_prompts_project_context_without_claiming_a_billed_session_total() {
    harn_clock::test_support::with_local_tasks(async {
        harn_vm::reset_thread_local_state();
        let fixtures = [
            serde_json::json!({"text":"first", "model":"gpt-4o-mini", "provider":"openai", "simulated_cost_usd":0.125}),
            serde_json::json!({"text":"second", "model":"gpt-4o-mini", "provider":"openai", "simulated_cost_usd":0.25}),
            serde_json::json!({"text":"free", "model":"gpt-4o-mini", "provider":"openai", "simulated_cost_usd":0.0}),
        ].into_iter().map(|value| harn_vm::llm::parse_llm_mock_value(&value).unwrap()).collect();
        harn_vm::llm::install_cli_llm_mocks(fixtures);
        let _reset = MockReset;
        let root = tempfile::tempdir().unwrap();
        let pipeline = root.path().join("context-usage.harn");
        std::fs::write(&pipeline, r#"import { agent_loop } from "std/agent/loop"
pipeline main(harness: Harness) {
  agent_loop(harness, prompt, nil, {
    provider: "openai", model: "gpt-4o-mini", max_tokens: 8, max_iterations: 1, stream: false,
  })
  harness.stdio.println("root turn reached")
}
"#).unwrap();
        let (request_tx, request_rx) = mpsc::unbounded_channel();
        let (response_tx, mut response_rx) = mpsc::unbounded_channel();
        let server = tokio::task::spawn_local(run_acp_channel_server(
            AcpServerConfig::new(Some(pipeline.to_string_lossy().into_owned())), request_rx, response_tx,
        ));
        request_tx.send(serde_json::json!({"jsonrpc":"2.0", "id":1, "method":"initialize", "params":{}})).unwrap();
        assert!(recv_response_with_id(&mut response_rx, 1).await.get("result").is_some());
        request_tx.send(serde_json::json!({"jsonrpc":"2.0", "id":2, "method":"session/new", "params":{
            "cwd":root.path(), "environmentPolicy":{"kind":"isolated"}
        }})).unwrap();
        let created = recv_response_with_id(&mut response_rx, 2).await;
        let session_id = created["result"]["sessionId"].as_str().unwrap();
        let upstream: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"), "/../../conformance/protocols/schemas/acp-1.26.0-upstream.schema.json"
        ))).unwrap();
        let validator = jsonschema::draft202012::new(&serde_json::json!({
            "$ref":"#/$defs/SessionNotification", "$defs":upstream["$defs"]
        })).unwrap();
        for (id, expected_run_cost) in [
            (3, 0.125), (4, 0.25), (5, 0.0),
        ] {
            request_tx.send(serde_json::json!({"jsonrpc":"2.0", "id":id, "method":"session/prompt", "params":{
                "sessionId":session_id, "prompt":[{"type":"text", "text":format!("Root turn {id}")}]
            }})).unwrap();
            let mut updates = Vec::new();
            let mut run_usage = None;
            let mut reached = false;
            loop {
                let frame = recv_json(&mut response_rx).await;
                if frame["method"] == "host/capabilities" { reply_empty_result(&request_tx, &frame); }
                if frame["method"] == HARN_AGENT_EVENT_METHOD && frame["params"]["kind"] == "provider_usage" {
                    assert_eq!(frame["params"]["sessionId"], session_id);
                    run_usage = Some(frame["params"]["run_usage"].clone());
                }
                if frame["method"] == "session/update" {
                    let params = &frame["params"];
                    if params["update"]["sessionUpdate"] == "usage_update" {
                        assert_eq!(params["sessionId"], session_id);
                        assert!(validator.is_valid(params), "{frame}");
                        let mut invalid = params.clone();
                        invalid["update"]["size"] = serde_json::json!("unknown");
                        assert!(!validator.is_valid(&invalid));
                        updates.push(params["update"].clone());
                    }
                    if params["update"]["sessionUpdate"] == "agent_message_chunk" {
                        reached |= params["update"]["content"]["text"].as_str().is_some_and(|text| text.contains("root turn reached"));
                    }
                }
                if frame["id"] == id && frame.get("method").is_none() {
                    assert_eq!(frame["result"]["stopReason"], "end_turn", "{frame}");
                    break;
                }
            }
            assert!(reached, "actual root prompt {id} must finish");
            {
                assert_eq!(updates.len(), 1, "the actual provider request must project once: {updates:?}");
                let final_update = updates.last().unwrap();
                assert!(final_update["used"].as_u64().unwrap() > 0);
                assert!(final_update["size"].as_u64().unwrap() > 0);
                assert!(!final_update["_meta"]["harn"]["callId"].as_str().unwrap().is_empty());
                assert!(final_update["cost"].is_null(), "a run price cannot establish the billed-session total");
                assert_eq!(final_update["_meta"]["harn"]["costAvailability"], "unmeasured");
                let usage = run_usage.expect("the actual provider ledger must settle");
                assert_eq!(usage["provider_call_count"].as_i64(), Some(1));
                assert_eq!(usage["cost_usd"].as_f64(), Some(expected_run_cost));
            }
        }
        drop(request_tx);
        server.await.unwrap();
    }).await;
}
