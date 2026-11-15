use super::{
    base_opts, read_http_request, spawn_llm_stub, spawn_llm_stub_many,
    vm_call_llm_full_streaming_offthread,
};
use crate::llm::api::test_support::allow_stubbed_llm_transport;
use crate::llm::env_guard;

#[test]
fn gemini_generate_content_stream_uses_sse_and_preserves_terminal_usage() {
    let _guard = env_guard();
    let _allow_llm_transport = allow_stubbed_llm_transport();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .build()
        .expect("runtime");

    runtime.block_on(async {
        let server = spawn_llm_stub("Gemini GenerateContent stream", |stream| {
            use std::io::{Read, Write};

            let mut buffer = vec![0u8; 16_384];
            let count = stream.read(&mut buffer).expect("read request");
            let request = String::from_utf8_lossy(&buffer[..count]);
            assert!(request.starts_with(
                "POST /v1beta/models/gemini-2.5-flash-lite:streamGenerateContent?alt=sse HTTP/1.1\r\n"
            ));

            let body = concat!(
                r#"data: {"candidates":[{"content":{"parts":[{"text":"STREAM_"}]}}]}"#,
                "\n\n",
                r#"data: {"candidates":[{"content":{"parts":[{"text":"OK"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":3,"candidatesTokenCount":2,"totalTokenCount":5}}"#,
                "\n\n"
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).expect("write response");
        });
        let mut providers = crate::llm_config::ProvidersConfig::default();
        providers.providers.insert(
            "gemini".to_string(),
            crate::llm_config::ProviderDef {
                base_url: format!("http://{}", server.addr()),
                auth_style: "none".to_string(),
                auth_env: crate::llm_config::AuthEnv::None,
                ..Default::default()
            },
        );
        crate::llm_config::set_user_overrides(Some(providers));

        let local = tokio::task::LocalSet::new();
        let (result, deltas) = local
            .run_until(async {
                let mut options = base_opts("gemini");
                options.model = "gemini-2.5-flash-lite".to_string();
                options.stream = true;
                let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
                let result = vm_call_llm_full_streaming_offthread(&options, sender)
                    .await
                    .expect("Gemini stream should succeed");
                let mut deltas = Vec::new();
                while let Ok(delta) = receiver.try_recv() {
                    deltas.push(delta);
                }
                (result, deltas)
            })
            .await;

        crate::llm_config::clear_user_overrides();
        drop(server);

        assert_eq!(result.text, "STREAM_OK");
        assert_eq!(deltas, ["STREAM_", "OK"]);
        assert_eq!(result.input_tokens, 3);
        assert_eq!(result.output_tokens, 2);
        assert_eq!(result.stop_reason.as_deref(), Some("STOP"));
    });
}

/// A real agent loop and Gemini HTTP transport must answer the name the model
/// invoked, while durable execution evidence keeps the semantic handler name.
#[test]
fn retained_resolution_keeps_gemini_response_names_and_semantic_audit() {
    use serde_json::{json, Value};
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    let _guard = env_guard();
    let _transport = allow_stubbed_llm_transport();
    crate::reset_thread_local_state();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let capture = requests.clone();
    let server = spawn_llm_stub_many("retained Gemini identity", 3, move |turn, stream| {
        let (headers, request) = read_http_request(stream);
        assert!(headers.starts_with("POST /v1beta/models/gemini-2.5-flash-lite:generateContent"));
        let parts = match turn {
            0 => json!([
                {"functionCall":{"id":"original-edit","name":"edit_value","args":{}}},
                {"functionCall":{"id":"observe","name":"read_evidence","args":{}}}
            ]),
            1 => {
                // The scripted provider reads the actual feedback the model
                // received, rather than inventing a pending handle independently.
                let proposal = request["contents"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .flat_map(|message| message["parts"].as_array().unwrap())
                    .filter_map(|part| part["functionResponse"]["response"]["result"].as_str())
                    .find_map(|text| text.split_once("proposal_id="))
                    .and_then(|(_, tail)| {
                        tail.split(|ch: char| ch.is_whitespace() || ch == '.')
                            .next()
                    })
                    .filter(|id| !id.is_empty())
                    .expect("second request must contain the real retained proposal");
                json!([{"functionCall":{
                    "id":"confirm","name":"__harn_resolve_tool_call",
                    "args":{"proposal_id":proposal,"action":"resume"}
                }}])
            }
            2 => json!([{"text":"Finished. ##DONE##"}]),
            _ => unreachable!(),
        };
        capture.lock().unwrap().push(request);
        let body = json!({
            "candidates":[{"content":{"role":"model","parts":parts},"finishReason":"STOP"}],
            "usageMetadata":{"promptTokenCount":1,"candidatesTokenCount":1,"totalTokenCount":2}
        })
        .to_string();
        write!(stream, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).unwrap();
    });
    let mut providers = crate::llm_config::ProvidersConfig::default();
    providers.providers.insert(
        "gemini".to_string(),
        crate::llm_config::ProviderDef {
            base_url: format!("http://{}", server.addr()),
            auth_style: "none".to_string(),
            auth_env: crate::llm_config::AuthEnv::None,
            ..Default::default()
        },
    );
    crate::llm_config::set_user_overrides(Some(providers));

    let source = r#"
import { agent_loop } from "std/agent/loop"
import { AgentSpec } from "std/agent/options"
import { agent_tool_handler_result } from "std/agent/tool_lifecycle"

pipeline main(harness: Harness) {
  const sid = harness.agent.open()
  const writes = harness.runtime.atomic(0)
  let tools = tool_registry()
  tools = tool_define(tools, "read_evidence", "Read the evidence.", {
    parameters: {}, annotations: {kind: "read", side_effect_level: "read_only"},
    handler: { _args -> return "Evidence ready." },
  })
  tools = tool_define(tools, "edit_value", "Apply the chosen edit.", {
    parameters: {}, annotations: {kind: "edit", side_effect_level: "workspace_write"},
    handler: { _args ->
      harness.runtime.atomic_add(writes, 1)
      return agent_tool_handler_result("applied", {
        mutation_status: "applied", requires_repair_before_verify: false,
      })
    },
  })
  const options: AgentSpec = {
    provider: "gemini", model: "gemini-2.5-flash-lite", tools: tools,
    session_id: sid, tool_format: "native", stream: false, max_tokens: 256,
    loop_until_done: true, max_iterations: 3, final_wrapup: false, auto_compact: false,
  }
  const outcome = agent_loop(harness, "Read before confirming the edit.", nil, options)
  const stored = harness.agent.snapshot(sid).messages.find({ message ->
    message?.tool_call_id == "confirm"
  })
  harness.stdio.log(json_stringify({
    mutations: harness.runtime.atomic_get(writes), semantic_name: stored?.name,
    status: outcome.status,
  }))
  harness.agent.close(sid)
}
"#;
    let path = std::path::Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/llm/api/transport_stub_tests/gemini_retained_resolution_test.harn"
    ));
    let chunk = crate::compile_source_at_path(source, path, None).expect("fixture compiles");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let output = runtime.block_on(tokio::task::LocalSet::new().run_until(async {
        let mut vm = crate::new_vm();
        crate::register_vm_stdlib(&mut vm);
        vm.execute(&chunk)
            .await
            .expect("actual Gemini agent loop finishes");
        vm.output().to_string()
    }));
    crate::llm_config::clear_user_overrides();
    drop(server);
    let requests = requests.lock().unwrap();
    assert_eq!(
        requests.len(),
        3,
        "all actor requests must reach real HTTP transport"
    );
    assert!(
        requests[0]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|tool| tool["functionDeclarations"].as_array())
            .flatten()
            .any(|tool| tool["name"] == "__harn_resolve_tool_call"),
        "the initial real Gemini request must disclose the stable resolver"
    );
    let responses = |body: &Value| {
        body["contents"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|message| message["parts"].as_array().unwrap())
            .filter_map(|part| part.get("functionResponse"))
            .cloned()
            .collect::<Vec<_>>()
    };
    let second = responses(&requests[1]);
    assert!(second
        .iter()
        .any(|result| result["name"] == "read_evidence"));
    assert!(second.iter().any(|result| result["name"] == "edit_value"));
    let followup = responses(&requests[2]);
    let confirmation = followup
        .iter()
        .find(|result| result["id"] == "confirm")
        .expect("the actual confirmation result must reach Gemini");
    let invoked = requests[2]["contents"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|message| message["parts"].as_array().unwrap())
        .filter_map(|part| part.get("functionCall"))
        .find(|call| call["id"] == "confirm")
        .expect("actual preceding Gemini invocation");
    assert_eq!(invoked["name"], "__harn_resolve_tool_call");
    assert!(
        followup
            .iter()
            .any(|result| result["name"] == "read_evidence"),
        "ordinary neighbor name must remain intact"
    );
    let report: Value = output
        .lines()
        .filter_map(|line| line.strip_prefix("[harn] "))
        .find_map(|line| serde_json::from_str(line).ok())
        .expect("actual loop report");
    assert_eq!(report["mutations"], 1);
    assert_eq!(
        report["semantic_name"], "edit_value",
        "durable execution facts must not be renamed"
    );
    assert_eq!(report["status"], "done");
    assert_eq!(
        confirmation["name"], invoked["name"],
        "Gemini response must answer the actual invoked name: {confirmation}"
    );
}
