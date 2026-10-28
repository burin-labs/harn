use crate::llm::api::test_support::allow_stubbed_llm_transport;
use crate::llm::api::transport_stub_tests::{
    install_openai_stub_provider, read_http_request, spawn_llm_stub_many,
};
use crate::llm::env_guard;
use serde_json::{json, Value};
use std::io::Write;
use std::sync::{Arc, Mutex};

struct Cleanup;

struct UsageEvents(Arc<Mutex<Vec<Value>>>);

impl crate::agent_events::AgentEventSink for UsageEvents {
    fn handle_event(&self, event: &crate::agent_events::AgentEvent) {
        self.0
            .lock()
            .unwrap()
            .push(serde_json::to_value(event).unwrap());
    }
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        crate::llm_config::clear_user_overrides();
        crate::llm_config::clear_runtime_provider_endpoint_overrides();
        crate::agent_sessions::reset_session_store();
        crate::reset_thread_local_state();
    }
}

async fn execute(root: &std::path::Path, body: &str) -> Result<String, crate::value::VmError> {
    execute_with_cancel(root, body, None).await
}

async fn execute_with_cancel(
    root: &std::path::Path,
    body: &str,
    cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
) -> Result<String, crate::value::VmError> {
    let root = serde_json::to_string(root.to_str().unwrap()).unwrap();
    let source = format!(
        r#"
import {{ agent_loop }} from "std/agent/loop"
import {{ session_store_presence }} from "std/session-store"
fn main(harness: Harness) {{
  const session = harness.agent.open("prepared-run")
  const call_options = {{provider: "openai", model: "gpt-5.6-luna", session_id: session,
    effort: "high", api_mode: "chat_completions", stream: false, max_tokens: 64, call_role: "application.classifier"}}
  const options = {{provider: "openai", model: "gpt-5.6-luna", root: {root},
    session_id: session, effort: "high", api_mode: "chat_completions", stream: false, max_tokens: 64,
    tool_format: "text", progress_tool: {{}}, loop_until_done: false, max_iterations: 2}}
  {body}
}}
"#
    );
    let chunk = crate::compile_source(&source).expect("compile preparation scope fixture");
    let mut vm = crate::new_vm();
    crate::register_vm_stdlib(&mut vm);
    if let Some(cancel) = cancel {
        vm.install_cancel_token(cancel);
    }
    vm.execute(&chunk).await?;
    Ok(vm.output().to_string())
}

async fn project(root: &std::path::Path) -> crate::orchestration::RunRecord {
    let store = crate::stdlib::session_store::open_existing_canonical_store(root)
        .unwrap()
        .expect("preparation persisted a store");
    crate::orchestration::project_run_record_from_session(&store, "prepared-run")
        .await
        .expect("project the actual journal")
}

#[test]
fn preparation_and_actor_publish_each_settled_provider_usage() {
    let _env = env_guard();
    let _transport = allow_stubbed_llm_transport();
    let _cleanup = Cleanup;
    crate::agent_sessions::reset_session_store();
    crate::reset_thread_local_state();
    let wire = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured_wire = wire.clone();
    let server = spawn_llm_stub_many("live provider usage", 3, move |_, stream| {
        let (_, request) = read_http_request(stream);
        captured_wire.lock().unwrap().push(request);
        let body = json!({"id":"local", "model":"gpt-5.6-luna",
            "choices":[{"index":0,"message":{"role":"assistant","content":"Finished."},"finish_reason":"stop"}],
            "usage":{"prompt_tokens":3,"completion_tokens":1,"total_tokens":4}}).to_string();
        write!(stream, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).unwrap();
    });
    install_openai_stub_provider("openai", server.addr());
    crate::llm_config::set_runtime_provider_endpoint_overrides(
        crate::llm_config::RuntimeProviderEndpointOverrides::single(
            "openai",
            format!("http://{}/v1", server.addr()),
        )
        .unwrap(),
    );
    let root = tempfile::tempdir().unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::new(UsageEvents(events.clone()));
    let transport = crate::agent_events::AgentEventTransport::new(sink);
    let runtime = crate::test_support::current_thread_runtime();
    runtime.block_on(
        tokio::task::LocalSet::new().run_until(transport.scope(async {
            execute(
                root.path(),
                r#"
  harness.agent.with_run(session, options, { ->
    harness.llm.call("Classify this task.", nil, call_options)
    harness.llm.call("Prepare the task.", nil, call_options)
    agent_loop(harness, "Reply briefly.", nil, options)
  })
"#,
            )
            .await
            .expect("preparation and actor finish");
            let record = project(root.path()).await;
            assert_eq!(record.evidence.trace_spans.len(), 3);
            assert!(record
                .evidence
                .trace_spans
                .iter()
                .all(|span| span.cost_usd.is_some()));
        })),
    );
    assert_eq!(
        wire.lock().unwrap().len(),
        3,
        "all three provider calls reached the wire"
    );
    let events = events.lock().unwrap();
    let usage: Vec<_> = events
        .iter()
        .filter(|event| event["type"] == "provider_usage")
        .collect();
    assert_eq!(
        usage.len(),
        3,
        "each settled provider call publishes its ledger: {events:?}"
    );
    assert_eq!(usage[0]["run_usage"]["provider_call_count"], 1);
    assert_eq!(usage[1]["run_usage"]["provider_call_count"], 2);
    assert_eq!(usage[2]["run_usage"]["provider_call_count"], 3);
    assert!(usage[0]["run_usage"]["run_id"]
        .as_str()
        .is_some_and(|id| !id.is_empty()));
    assert_eq!(
        usage[0]["run_usage"]["run_id"],
        usage[1]["run_usage"]["run_id"]
    );
    assert_eq!(
        usage[1]["run_usage"]["run_id"],
        usage[2]["run_usage"]["run_id"]
    );
    assert_eq!(usage[0]["run_usage"]["parent_session_id"], "");
    assert_eq!(usage[0]["run_usage"]["lineage_known"], true);
    assert_ne!(usage[0]["call_id"], usage[1]["call_id"]);
    assert_ne!(usage[1]["call_id"], usage[2]["call_id"]);
}

#[test]
fn a_failed_provider_attempt_remains_unpriced_after_a_later_success() {
    let _env = env_guard();
    let _transport = allow_stubbed_llm_transport();
    let _cleanup = Cleanup;
    crate::agent_sessions::reset_session_store();
    crate::reset_thread_local_state();
    let wire = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured_wire = wire.clone();
    let server = spawn_llm_stub_many("unpriced provider attempt", 2, move |index, stream| {
        let (_, request) = read_http_request(stream);
        captured_wire.lock().unwrap().push(request);
        let (status, body) = if index == 0 {
            (
                "429 Too Many Requests",
                json!({"error":{"message":"rate limited"}}).to_string(),
            )
        } else {
            ("200 OK", json!({"id":"local", "model":"gpt-5.6-luna",
                "choices":[{"index":0,"message":{"role":"assistant","content":"Finished."},"finish_reason":"stop"}],
                "usage":{"prompt_tokens":3,"completion_tokens":1,"total_tokens":4}}).to_string())
        };
        write!(stream, "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).unwrap();
    });
    install_openai_stub_provider("openai", server.addr());
    crate::llm_config::set_runtime_provider_endpoint_overrides(
        crate::llm_config::RuntimeProviderEndpointOverrides::single(
            "openai",
            format!("http://{}/v1", server.addr()),
        )
        .unwrap(),
    );
    let root = tempfile::tempdir().unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let transport =
        crate::agent_events::AgentEventTransport::new(Arc::new(UsageEvents(events.clone())));
    let runtime = crate::test_support::current_thread_runtime();
    runtime.block_on(
        tokio::task::LocalSet::new().run_until(transport.scope(async {
            execute(
                root.path(),
                r#"
  harness.agent.with_run(session, options, { ->
    const refused = try { harness.llm.call("First attempt.", nil, call_options) }
    require is_err(refused), "the first provider response must fail"
    harness.llm.call("Second attempt.", nil, call_options)
  })
"#,
            )
            .await
            .expect("the later provider call succeeds");
            let persisted = project(root.path()).await.usage.expect("the run has usage");
            assert_eq!(persisted.call_count, 2);
            assert_eq!(persisted.unpriced_calls, 1);
            assert!(persisted.known_cost_usd > 0.0);
            assert!(persisted.cost_usd.is_none());
        })),
    );
    assert_eq!(
        wire.lock().unwrap().len(),
        2,
        "both requests reached the provider"
    );
    let events = events.lock().unwrap();
    let usage: Vec<_> = events
        .iter()
        .filter(|event| event["type"] == "provider_usage")
        .collect();
    assert_eq!(usage.len(), 2, "one usage event per settled request");
    assert_eq!(usage[0]["run_usage"]["provider_call_count"], 1);
    assert_eq!(usage[0]["run_usage"]["unpriced_calls"], 1);
    assert_eq!(usage[0]["run_usage"]["usage_unknown_calls"], 1);
    assert_eq!(usage[1]["run_usage"]["provider_call_count"], 2);
    assert_eq!(usage[1]["run_usage"]["unpriced_calls"], 1);
    assert_eq!(usage[1]["run_usage"]["usage_unknown_calls"], 1);
    assert!(usage[1]["run_usage"]["known_cost_usd"].as_f64().unwrap() > 0.0);
    assert!(usage[1]["run_usage"]["cost_usd"].is_null());
}

#[test]
fn an_unpriced_retry_keeps_the_later_priced_subtotal_in_the_durable_run() {
    let _env = env_guard();
    let _transport = allow_stubbed_llm_transport();
    let _cleanup = Cleanup;
    crate::agent_sessions::reset_session_store();
    crate::reset_thread_local_state();
    let wire = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured_wire = wire.clone();
    let server = spawn_llm_stub_many("unpriced retry", 2, move |index, stream| {
        let (_, request) = read_http_request(stream);
        captured_wire.lock().unwrap().push(request);
        let body = if index == 0 {
            json!({"id":"local-empty", "model":"gpt-5.6-luna",
                "choices":[{"index":0,"message":{"role":"assistant","content":""},"finish_reason":"stop"}]})
        } else {
            json!({"id":"local-success", "model":"gpt-5.6-luna",
                "choices":[{"index":0,"message":{"role":"assistant","content":"Finished."},"finish_reason":"stop"}],
                "usage":{"prompt_tokens":3,"completion_tokens":1,"total_tokens":4}})
        }
        .to_string();
        write!(stream, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).unwrap();
    });
    install_openai_stub_provider("openai", server.addr());
    crate::llm_config::set_runtime_provider_endpoint_overrides(
        crate::llm_config::RuntimeProviderEndpointOverrides::single(
            "openai",
            format!("http://{}/v1", server.addr()),
        )
        .unwrap(),
    );
    let root = tempfile::tempdir().unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let transport =
        crate::agent_events::AgentEventTransport::new(Arc::new(UsageEvents(events.clone())));
    let runtime = crate::test_support::current_thread_runtime();
    runtime.block_on(
        tokio::task::LocalSet::new().run_until(transport.scope(async {
            execute(
                root.path(),
                r#"
  harness.agent.with_run(session, options, { ->
    harness.llm.call("Reply briefly.", nil, call_options)
  })
"#,
            )
            .await
            .expect("the retry succeeds");
            let record = project(root.path()).await;
            assert_eq!(record.evidence.trace_spans.len(), 1);
            assert!(record.evidence.trace_spans[0].cost_usd.is_none());
            let persisted = record.usage.expect("the run has usage");
            assert_eq!(persisted.call_count, 1, "one logical call");
            assert_eq!(persisted.unpriced_calls, 1);
            assert!(persisted.known_cost_usd > 0.0);
            assert!(persisted.cost_usd.is_none());
        })),
    );
    assert_eq!(
        wire.lock().unwrap().len(),
        2,
        "both attempts reached the provider"
    );
    let events = events.lock().unwrap();
    let usage: Vec<_> = events
        .iter()
        .filter(|event| event["type"] == "provider_usage")
        .collect();
    assert_eq!(usage.len(), 2, "each attempt publishes usage");
    assert_eq!(usage[1]["run_usage"]["provider_call_count"], 2);
    assert_eq!(usage[1]["run_usage"]["unpriced_calls"], 1);
    assert_eq!(usage[1]["run_usage"]["usage_unknown_calls"], 1);
    assert!(usage[1]["run_usage"]["known_cost_usd"].as_f64().unwrap() > 0.0);
    assert!(usage[1]["run_usage"]["cost_usd"].is_null());
}

#[test]
fn bounded_presence_reuses_live_owner_and_refuses_unknown_legacy_state() {
    let _env = env_guard();
    let _cleanup = Cleanup;
    crate::agent_sessions::reset_session_store();
    crate::reset_thread_local_state();
    let root = tempfile::tempdir().unwrap();
    let legacy_dir = root.path().join(".harn/session-store");
    std::fs::create_dir_all(&legacy_dir).unwrap();
    std::fs::write(legacy_dir.join("legacy.jsonl"), "not a valid event\n").unwrap();
    let malformed_dir = root.path().join("malformed/.harn");
    std::fs::create_dir_all(&malformed_dir).unwrap();
    std::fs::write(malformed_dir.join("session-store.sqlite"), "not a database").unwrap();
    let runtime = crate::test_support::current_thread_runtime();
    runtime.block_on(tokio::task::LocalSet::new().run_until(async {
        execute(
            root.path(),
            r#"
  const absent = harness.agent.session_store_presence("missing", {root: options.root})
  require absent.state == "absent", "missing store is distinct from a missing session"
  for i in 0..128 {
    harness.agent.session_store_append("saved", {value: i}, {root: options.root, tenant_id: "owner"})
  }
  const before = harness.agent.session_store_events("saved", {root: options.root, tenant_id: "owner"}).value
  require len(before) == 128,
    "the long-journal fixture must actually contain its events"
  const unowned = harness.agent.session_store_presence("saved", {root: options.root})
  require unowned.state == "unavailable", "closed owner cannot claim the session is missing"
  const foreign_root = options.root + "/foreign"
  harness.agent.session_store_append("foreign", {value: 1}, {root: foreign_root})
  const malformed_root = options.root + "/malformed"
  const malformed = harness.agent.session_store_presence("saved", {root: malformed_root})
  require malformed.state == "unavailable", "an unowned database cannot be assumed valid"
  const malformed_read = try { harness.agent.session_store_events("saved", {root: malformed_root}) }
  require is_err(malformed_read), "the existing fallback surfaces a malformed database"
  harness.agent.with_run(session, options, { ->
    const saved = harness.agent.session_store_presence("saved", {root: options.root, tenant_id: "owner"})
    require saved.state == "present" && saved.has_events, "live owner finds the saved journal"
    const typed = session_store_presence(harness.agent, "saved", {root: options.root, tenant_id: "owner"})
    require typed.state == "present" && typed.has_events, "the typed public facade preserves the result"
    const unknown = harness.agent.session_store_presence("missing", {root: options.root})
    require unknown.state == "missing", "live owner distinguishes a missing session"
    const foreign = harness.agent.session_store_presence("foreign", {root: foreign_root})
    require foreign.state == "unavailable", "an owner of another root cannot answer this lookup"
    const wrong_tenant = try {
      harness.agent.session_store_presence("saved", {root: options.root, tenant_id: "other"})
    }
    require is_err(wrong_tenant), "tenant denial is not reported as missing"
    const legacy = harness.agent.session_store_presence("legacy", {root: options.root})
    require legacy.state == "unavailable", "unread legacy content is not reported as missing"
    const legacy_read = try { harness.agent.session_store_events("legacy", {root: options.root}) }
    require is_err(legacy_read), "the existing fallback surfaces malformed legacy data"
    const after = harness.agent.session_store_events("saved", {root: options.root, tenant_id: "owner"}).value
    require len(after) == len(before) && after[127].record_hash == before[127].record_hash,
      "presence reads leave the saved journal unchanged"
    require !harness.agent.exists("missing"), "the missing lookup does not open a session"
  })
"#,
        )
        .await
        .expect("the public owner-backed presence path satisfies its contract");
    }));
}

#[test]
fn preparation_and_actor_share_one_durable_run_without_cross_turn_leakage() {
    let _env = env_guard();
    let _transport = allow_stubbed_llm_transport();
    let _cleanup = Cleanup;
    crate::agent_sessions::reset_session_store();
    crate::reset_thread_local_state();
    let wire = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured = wire.clone();
    let server = spawn_llm_stub_many("run preparation", 7, move |_, stream| {
        let (_, request) = read_http_request(stream);
        assert_eq!(
            request["reasoning_effort"], "high",
            "assert the actual wire rung"
        );
        captured.lock().unwrap().push(request);
        let body = json!({"id":"local", "model":"gpt-5.6-luna",
            "choices":[{"index":0,"message":{"role":"assistant","content":"Finished."},"finish_reason":"stop"}],
            "usage":{"prompt_tokens":3,"completion_tokens":1,"total_tokens":4}}).to_string();
        write!(stream, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).unwrap();
    });
    install_openai_stub_provider("openai", server.addr());
    crate::llm_config::set_runtime_provider_endpoint_overrides(
        crate::llm_config::RuntimeProviderEndpointOverrides::single(
            "openai",
            format!("http://{}/v1", server.addr()),
        )
        .unwrap(),
    );
    let root = tempfile::tempdir().unwrap();
    let runtime = crate::test_support::current_thread_runtime();
    runtime.block_on(tokio::task::LocalSet::new().run_until(async {
        let mismatch = execute(
            root.path(),
            r#"
  harness.agent.with_run("outer-run", options, { ->
    harness.llm.call("Must not dispatch.", nil, call_options)
  })
"#,
        )
        .await
        .expect_err("different option session is rejected before preparation");
        assert!(mismatch.to_string().contains("options.session_id must match"));
        assert!(wire.lock().unwrap().is_empty());
        assert!(crate::stdlib::session_store::open_existing_canonical_store(root.path())
            .unwrap()
            .is_none());
        execute(
            root.path(),
            r#"
  harness.llm.call("Unscoped classifier control.", nil, call_options)
  agent_loop(harness, "Reply briefly.", nil, options)
"#,
        )
        .await
        .expect("unscoped control finishes");
        assert_eq!(wire.lock().unwrap().len(), 2);
        assert_eq!(
            project(root.path()).await.usage.unwrap().call_count,
            1,
            "the negative control demonstrates the unrecorded preparation"
        );
        for turn in 1..=2 {
            let output = execute(
                root.path(),
                r#"
  harness.agent.subscribe(session, { event ->
    if event.type == "iteration_end" {
      harness.stdio.println("LIVE " + json_stringify(event.iteration_info.run_usage))
    }
  })
  const result = harness.agent.with_run(session, options, { ->
    harness.llm.call("Classify this task.", nil, call_options)
    agent_loop(harness, "Reply briefly.", nil, options)
  })
  harness.stdio.println(json_stringify(result.physical_usage))
"#,
            )
            .await
            .expect("preparation and actor finish");
            assert!(output.contains("\"provider_call_count\":2"), "{output}");
            let live = output
                .lines()
                .find(|line| line.starts_with("LIVE "))
                .expect("iteration_end carries the run ledger");
            assert!(
                live.contains("\"provider_call_count\":2"),
                "the live total counts the preparatory call: {live}"
            );
            assert_eq!(wire.lock().unwrap().len(), 2 + turn * 2);
            let run = project(root.path()).await;
            assert_eq!(run.usage.as_ref().unwrap().call_count, 2);
            let receipts = run.evidence.reasoning_receipts.unwrap();
            assert_eq!(receipts.len(), 2);
            assert_eq!(
                receipts[0].call_role.as_deref(),
                Some("application.classifier")
            );
            assert_eq!(receipts[1].call_role.as_deref(), Some("agent.main"));
            assert_ne!(receipts[0].call_id, receipts[1].call_id);
            assert!(receipts
                .iter()
                .all(|receipt| receipt.sent_value == Some(json!("high"))));
        }
        let failed = execute(
            root.path(),
            r#"
  harness.agent.with_run(session, options, { ->
    harness.llm.call("Classify a failing task.", nil, call_options)
    throw "preparation refused"
  })
"#,
        )
        .await;
        assert!(failed.is_err());
        assert_eq!(wire.lock().unwrap().len(), 7);
        let failed = project(root.path()).await;
        assert_eq!(failed.status, "failed");
        assert_eq!(failed.usage.unwrap().call_count, 1);
        assert_eq!(failed.evidence.reasoning_receipts.unwrap().len(), 1);
        execute(
            root.path(),
            "harness.agent.with_run(session, options, { -> 42 })",
        )
        .await
        .expect("zero-call preparation completes");
        let empty = project(root.path()).await;
        assert_eq!(empty.evidence.reasoning_receipts, Some(vec![]));
        assert_eq!(
            wire.lock().unwrap().len(),
            7,
            "measured zero has no transport request"
        );
        execute(
            root.path(),
            "harness.agent.with_run(\"temporary-run\", options + {session_id: \"temporary-run\"}, { -> 42 })",
        )
        .await
        .expect("a scope can own a session without an agent loop");
        assert!(!crate::agent_sessions::exists("temporary-run"));
    }));
}

#[test]
fn admission_returns_the_same_physical_usage_as_the_durable_run() {
    let _env = env_guard();
    let _transport = allow_stubbed_llm_transport();
    let _cleanup = Cleanup;
    crate::agent_sessions::reset_session_store();
    crate::reset_thread_local_state();
    let wire = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured = wire.clone();
    let server = spawn_llm_stub_many("pre-loop admission", 2, move |_, stream| {
        let (_, request) = read_http_request(stream);
        assert_eq!(request["reasoning_effort"], "high");
        captured.lock().unwrap().push(request);
        let body = json!({"id":"local", "model":"gpt-5.6-luna",
            "choices":[{"index":0,"message":{"role":"assistant","content":"Classified."},"finish_reason":"stop"}],
            "usage":{"prompt_tokens":3,"completion_tokens":1,"total_tokens":4}}).to_string();
        write!(stream, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).unwrap();
    });
    install_openai_stub_provider("openai", server.addr());
    crate::llm_config::set_runtime_provider_endpoint_overrides(
        crate::llm_config::RuntimeProviderEndpointOverrides::single(
            "openai",
            format!("http://{}/v1", server.addr()),
        )
        .unwrap(),
    );
    let root = tempfile::tempdir().unwrap();
    let runtime = crate::test_support::current_thread_runtime();
    runtime.block_on(tokio::task::LocalSet::new().run_until(async {
        for hook_call in [false, true] {
            let body = if hook_call {
                r#"
  harness.agent.register_session_hook("user_prompt_submit", { _ ->
    harness.llm.call("Classify during admission.", nil, call_options)
    {block: true, reason: "admission control"}
  })
  const result = agent_loop(harness, "Must not start.", nil, options)
"#
            } else {
                r#"
  harness.agent.register_session_hook("user_prompt_submit", { _ ->
    {block: true, reason: "admission control"}
  })
  const result = harness.agent.with_run(session, options, { ->
    harness.llm.call("Classify before admission.", nil, call_options)
    agent_loop(harness, "Must not start.", nil, options)
  })
"#
            };
            let output = execute(root.path(), &format!(
                "{body}\n  harness.agent.clear_session_hooks()\n  harness.stdio.println(json_stringify(result))"
            )).await.expect("admission returns a blocked result");
            let result: Value = serde_json::from_str(output.trim()).unwrap();
            assert_eq!(result["status"], "blocked");
            assert_eq!(result["provider_call_count"], 1);
            assert_eq!(result["physical_usage"]["provider_call_count"], 1);
            assert_eq!(result["physical_usage"]["input_tokens"], 3);
            assert_eq!(result["physical_usage"]["output_tokens"], 1);
            assert_eq!(result["llm"]["iterations"], 0);
            assert_eq!(wire.lock().unwrap().len(), if hook_call { 2 } else { 1 });
            let run = project(root.path()).await;
            assert_eq!(run.status, "stopped");
            assert_eq!(run.usage.unwrap().call_count, 1);
            let receipts = run.evidence.reasoning_receipts.unwrap();
            assert_eq!(receipts.len(), 1);
            assert_eq!(receipts[0].call_role.as_deref(), Some("application.classifier"));
        }
        let output = execute(root.path(), r#"
  harness.agent.register_session_hook("user_prompt_submit", { _ ->
    {block: true, reason: "admission control"}
  })
  const result = harness.agent.with_run(session, options, { ->
    agent_loop(harness, "Must not start.", nil, options)
  })
  harness.agent.clear_session_hooks()
  harness.stdio.println(json_stringify(result))
"#).await.expect("zero-call admission returns a blocked result");
        let result: Value = serde_json::from_str(output.trim()).unwrap();
        assert_eq!(result["provider_call_count"], 0);
        assert_eq!(result["physical_usage"]["provider_call_count"], 0);
        assert_eq!(result["physical_usage"]["status"], "not_run");
        assert_eq!(wire.lock().unwrap().len(), 2);
        assert_eq!(project(root.path()).await.evidence.reasoning_receipts, Some(vec![]));
    }));
}

#[test]
fn cancellation_during_preparation_retains_the_dispatched_call_and_releases_the_run() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let _env = env_guard();
    let _transport = allow_stubbed_llm_transport();
    let _cleanup = Cleanup;
    crate::agent_sessions::reset_session_store();
    crate::reset_thread_local_state();
    let cancel = Arc::new(AtomicBool::new(false));
    let dispatched = Arc::new(AtomicUsize::new(0));
    let signal = cancel.clone();
    let observed = dispatched.clone();
    let server = spawn_llm_stub_many("cancel preparation", 1, move |_, stream| {
        let (_, request) = read_http_request(stream);
        assert_eq!(request["reasoning_effort"], "high");
        observed.fetch_add(1, Ordering::SeqCst);
        signal.store(true, Ordering::SeqCst);
        let body = json!({"model":"gpt-5.6-luna", "choices":[{"index":0,
            "message":{"role":"assistant","content":"Classified."},"finish_reason":"stop"}],
            "usage":{"prompt_tokens":3,"completion_tokens":1,"total_tokens":4}})
        .to_string();
        let _ = write!(stream, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len());
    });
    install_openai_stub_provider("openai", server.addr());
    crate::llm_config::set_runtime_provider_endpoint_overrides(
        crate::llm_config::RuntimeProviderEndpointOverrides::single(
            "openai",
            format!("http://{}/v1", server.addr()),
        )
        .unwrap(),
    );
    let root = tempfile::tempdir().unwrap();
    let runtime = crate::test_support::current_thread_runtime();
    runtime.block_on(tokio::task::LocalSet::new().run_until(async {
        let result = execute_with_cancel(
            root.path(),
            r#"
  harness.agent.with_run(session, options, { ->
    harness.llm.call("Classify before cancellation.", nil, call_options)
    agent_loop(harness, "Must not start.", nil, options)
  })
"#,
            Some(cancel),
        )
        .await;
        assert!(result.is_err());
        assert_eq!(
            dispatched.load(Ordering::SeqCst),
            1,
            "the cancellation follows a real dispatch"
        );
        let stopped = project(root.path()).await;
        assert_eq!(stopped.status, "cancelled");
        let receipts = stopped.evidence.reasoning_receipts.unwrap();
        assert_eq!(receipts.len(), 1);
        assert_eq!(
            receipts[0].call_role.as_deref(),
            Some("application.classifier")
        );
        execute(
            root.path(),
            "harness.agent.with_run(session, options, { -> 42 })",
        )
        .await
        .expect("a successor run can acquire the same session");
        assert_eq!(
            project(root.path()).await.evidence.reasoning_receipts,
            Some(vec![])
        );
        assert_eq!(dispatched.load(Ordering::SeqCst), 1);
    }));
}
