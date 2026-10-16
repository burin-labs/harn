//! The child's final usage must include provider requests from both its tool
//! turn and its exhausted structured-output wrap-up.

use super::*;
use crate::value::{VmClosure, VmEnv, VmValue};
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::{Arc, Mutex};

struct ProviderOverrides;

impl Drop for ProviderOverrides {
    fn drop(&mut self) {
        crate::llm_config::clear_user_overrides();
    }
}

fn compiled_probe_handler() -> Arc<VmClosure> {
    let program = harn_parser::check_source_strict(
        r#"
fn handler(_args: dict) -> string {
  return "native-tool-executed"
}
"#,
    )
    .expect("probe handler parses");
    let chunk = crate::compiler::Compiler::new()
        .compile(&program)
        .expect("probe handler compiles");
    let function = chunk
        .functions
        .iter()
        .find(|function| function.name.as_str() == "handler")
        .expect("compiled handler function")
        .clone();
    Arc::new(VmClosure {
        func: function,
        env: VmEnv::new(),
        source_dir: None,
        module_functions: None,
        module_state: None,
        retained_module_scope: None,
    })
}

fn probe_registry() -> VmValue {
    let entry = VmValue::dict([
        ("name", VmValue::string("probe")),
        (
            "description",
            VmValue::string("Return a fixed integration-test marker."),
        ),
        (
            "inputSchema",
            crate::schema::json_to_vm_value(&serde_json::json!({
                "type": "object",
                "properties": {},
                "required": [],
                "additionalProperties": false
            })),
        ),
        ("handler", VmValue::Closure(compiled_probe_handler())),
    ]);
    VmValue::dict([
        ("_type", VmValue::string("tool_registry")),
        ("tools", VmValue::List(Arc::new(vec![entry]))),
    ])
}

fn respond(stream: &mut std::net::TcpStream, body: &serde_json::Value) {
    let body = body.to_string();
    write!(
        stream,
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        body.len(),
        body
    )
    .expect("write provider response");
}

fn read_request_body(reader: &mut BufReader<&mut std::net::TcpStream>) -> serde_json::Value {
    let mut request_line = String::new();
    reader
        .read_line(&mut request_line)
        .expect("read request line");
    assert!(
        request_line.starts_with("POST /v1/chat/completions HTTP/1.1\r\n"),
        "unexpected provider request line: {request_line:?}"
    );
    let mut content_length = None;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).expect("read request headers");
        if line == "\r\n" || line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse::<usize>().ok();
            }
        }
    }
    let length = content_length.expect("provider request has content-length");
    let mut body = vec![0; length];
    reader
        .read_exact(&mut body)
        .expect("read full request body");
    serde_json::from_slice(&body).expect("provider request body is JSON")
}

#[tokio::test(flavor = "current_thread")]
async fn exhausted_structured_child_usage_includes_native_tool_and_wrapup_requests() {
    let _env = crate::llm::env_guard();
    let _allow_transport = crate::llm::api::test_support::allow_stubbed_llm_transport();
    crate::agent_sessions::reset_session_store();
    reset_llm_mock_state();

    let requests = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let captured = Arc::clone(&requests);
    let server = crate::llm::api::transport_stub_tests::spawn_llm_stub_many(
        "sub-agent physical usage",
        2,
        move |attempt, stream| {
            let mut reader = BufReader::new(stream);
            let request = read_request_body(&mut reader);
            captured.lock().expect("request capture lock").push(request);
            let response = if attempt == 0 {
                serde_json::json!({
                    "id": "tool-turn",
                    "object": "chat.completion",
                    "created": 0,
                    "model": "gpt-5.6-luna",
                    "choices": [{
                        "index": 0,
                        "message": {
                            "role": "assistant",
                            "content": null,
                            "tool_calls": [{
                                "id": "call_probe",
                                "type": "function",
                                "function": {"name": "probe", "arguments": "{}"}
                            }]
                        },
                        "finish_reason": "tool_calls"
                    }],
                    "usage": {"prompt_tokens": 11, "completion_tokens": 3, "total_tokens": 14}
                })
            } else {
                serde_json::json!({
                    "id": "final-wrapup",
                    "object": "chat.completion",
                    "created": 0,
                    "model": "gpt-5.6-luna",
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": "{\"answer\":\"wrapped\"}"},
                        "finish_reason": "stop"
                    }],
                    "usage": {"prompt_tokens": 7, "completion_tokens": 5, "total_tokens": 12}
                })
            };
            respond(reader.get_mut(), &response);
        },
    );
    crate::llm::api::transport_stub_tests::install_openai_stub_provider("openai", server.addr());
    let _provider_overrides = ProviderOverrides;

    let schema = serde_json::json!({
        "type": "object",
        "properties": {"answer": {"type": "string"}},
        "required": ["answer"],
        "additionalProperties": false
    });
    let mut options = crate::value::DictMap::from_iter([
        (
            crate::value::intern_key("provider"),
            VmValue::string("openai"),
        ),
        (
            crate::value::intern_key("model"),
            VmValue::string("gpt-5.6-luna"),
        ),
        (crate::value::intern_key("max_iterations"), VmValue::Int(1)),
        (
            crate::value::intern_key("loop_until_done"),
            VmValue::Bool(true),
        ),
        (
            crate::value::intern_key("tool_format"),
            VmValue::string("native"),
        ),
        (crate::value::intern_key("token_budget"), VmValue::Int(26)),
        (crate::value::intern_key("tools"), probe_registry()),
        (
            crate::value::intern_key("output"),
            crate::schema::json_to_vm_value(&serde_json::json!({"schema": schema})),
        ),
    ]);
    annotate_nested_execution_options(
        &mut options,
        NestedExecutionKind::SubAgentRun,
        "physical-usage-worker",
    );
    let spec = SubAgentRunSpec {
        name: "physical-usage-worker".to_string(),
        task: "Call probe, then return the declared answer.".to_string(),
        system: None,
        options,
        returns_schema: Some(crate::schema::json_to_vm_value(&schema)),
        session_id: format!("child-physical-usage-{}", uuid::Uuid::now_v7()),
        run_id: format!("agent_run_physical_usage_{}", uuid::Uuid::now_v7()),
        parent_session_id: None,
        parent_run_id: None,
        reminder_propagation: Vec::new(),
        workspace_anchor: None,
        stop_emitted: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };

    let mut vm = crate::new_vm();
    vm.set_harness(crate::Harness::real());
    crate::register_vm_stdlib(&mut vm);
    let ctx = crate::vm::AsyncBuiltinCtx::for_test(vm);
    let result = execute_sub_agent(&ctx, spec)
        .await
        .expect("child execution should return a terminal envelope");

    assert_eq!(result.payload["status"].as_str(), Some("budget_exhausted"));
    assert_eq!(result.payload["ok"].as_bool(), Some(false));
    assert_eq!(result.payload["budget_exceeded"].as_bool(), Some(true));
    assert_eq!(result.payload["data"]["answer"].as_str(), Some("wrapped"));
    let usage = &result.payload["usage"];
    assert_eq!(usage["token_scope"].as_str(), Some("every_provider_call"));
    assert_eq!(usage["status"].as_str(), Some("complete"));
    assert_eq!(usage["input_tokens"].as_i64(), Some(18));
    assert_eq!(usage["output_tokens"].as_i64(), Some(8));
    assert_eq!(usage["total_tokens"].as_i64(), Some(26));
    assert_eq!(usage["provider_call_count"].as_i64(), Some(2));

    // Error receipts use the same wrapper for nested-budget and tool-policy
    // refusals. Neither may discard the accounting reached above.
    for category in ["budget_exceeded", "permission_denied"] {
        let failed = wrap_sub_agent_error(
            String::new(),
            VmValue::List(Arc::new(Vec::new())),
            0,
            26,
            usage,
            category == "budget_exceeded",
            "physical-usage-error-projection",
            sub_agent_error_dict(category, "refused", None),
            None,
        );
        let failed = crate::llm::vm_value_to_json(&failed);
        assert_eq!(failed["ok"], false);
        assert_eq!(failed["tokens_used"], 26);
        assert_eq!(&failed["usage"], usage);
    }

    let requests = requests.lock().expect("request capture lock");
    assert_eq!(
        requests.len(),
        2,
        "both physical provider calls reached HTTP"
    );
    assert_eq!(
        requests[0]["tools"][0]["function"]["name"].as_str(),
        Some("probe"),
        "the first request served the native tool contract"
    );
    let wrapup_messages = requests[1]["messages"]
        .as_array()
        .expect("wrap-up request messages");
    assert!(
        wrapup_messages.iter().any(|message| {
            message["role"].as_str() == Some("tool")
                && message["content"]
                    .as_str()
                    .is_some_and(|content| content.contains("native-tool-executed"))
        }),
        "wrap-up request follows the fired tool result: {wrapup_messages:?}"
    );
    assert_eq!(
        requests[1]["response_format"]["type"].as_str(),
        Some("json_schema"),
        "the exhausted wrap-up request carries the declared output contract"
    );
    assert_eq!(
        requests[1]["response_format"]["json_schema"]["schema"]["properties"]["answer"]["type"]
            .as_str(),
        Some("string")
    );
}
