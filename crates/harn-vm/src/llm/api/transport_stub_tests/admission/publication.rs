//! Real agent-loop history crosses extraction, admission and HTTP unchanged
//! except for private publication state, which stays in the session transcript.
use super::*;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::Mutex;

fn request(stream: &mut std::net::TcpStream) -> Value {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    assert!(line.starts_with("POST /v1/responses "), "{line}");
    let mut length = None;
    loop {
        line.clear();
        assert!(reader.read_line(&mut line).unwrap() > 0);
        if line == "\r\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                length = Some(value.trim().parse::<usize>().unwrap());
            }
        }
    }
    let mut bytes = vec![0; length.expect("bounded request body")];
    reader.read_exact(&mut bytes).unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn reply(stream: &mut std::net::TcpStream, output: Vec<Value>) {
    let body = json!({"id":"local", "model":"gpt-5.6-luna","status":"completed",
        "output":output,
        "usage":{"input_tokens":3,"output_tokens":1,"total_tokens":4}})
    .to_string();
    write!(stream, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).unwrap();
}

#[test]
fn publication_stays_private_through_a_conservatively_budgeted_native_tool_turn() {
    let _env = env_guard();
    let _transport = allow_stubbed_llm_transport();
    let _cleanup = Cleanup;
    crate::llm::cost::reset_cost_state();
    let budget = crate::llm::admission::ConservativeLlmBudget::new(7.0).unwrap();
    let observed_budget = budget.clone();
    let wire = Arc::new(Mutex::new(Vec::new()));
    let captured = wire.clone();
    let server = spawn_llm_stub_many("assistant publication egress", 2, move |turn, stream| {
        captured.lock().unwrap().push((
            request(stream),
            observed_budget.receipt().unwrap().settled_upper_usd,
        ));
        reply(
            stream,
            if turn == 0 {
                vec![
                    json!({"type":"message","role":"assistant","content":[{
                        "type":"output_text","text":"Private inspection draft."
                    }]}),
                    json!({"type":"function_call","id":"fc_inspect","call_id":"call_inspect",
                        "name":"inspect","arguments":"{}","status":"completed"}),
                ]
            } else {
                vec![json!({"type":"message","role":"assistant","content":[{
                    "type":"output_text","text":"<user_response>Inspected.</user_response>\n<done>FINISHED</done>"
                }]})]
            },
        );
    });
    install(server.addr());
    let source = include_str!("publication.harn");
    let path = std::path::Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/llm/api/transport_stub_tests/admission/publication.harn"
    ));
    let chunk = crate::compile_source_at_path(source, path, None)
        .expect("real agent-loop fixture compiles with its imported contracts");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let output = runtime
        .block_on(budget.scope(tokio::task::LocalSet::new().run_until(async {
            let mut vm = crate::new_vm();
            crate::register_vm_stdlib(&mut vm);
            vm.execute(&chunk).await.expect("two native turns finish");
            vm.output().to_string()
        })))
        .expect("VM stays within its host allowance");
    let report: Value = output
        .lines()
        .filter_map(|line| line.strip_prefix("[harn] "))
        .find_map(|line| serde_json::from_str(line).ok())
        .expect("terminal fixture report");
    assert_eq!(report["status"], "done", "{report}");
    assert_eq!(report["text"], "Inspected.");
    assert_eq!(report["executed_tools"], 1);
    let history = report["history"].as_array().unwrap();
    let draft = history
        .iter()
        .find(|message| message["content"] == "Private inspection draft.")
        .expect("tool-turn prose retained in history");
    assert_eq!(draft["harn_assistant_publication"], "withheld");
    assert!(history
        .iter()
        .any(|message| message["harn_assistant_publication"] == "published"));
    assert!(!crate::llm::assistant_publication::is_visible(
        &crate::schema::json_to_vm_value(draft)
    ));
    let requests = wire.lock().unwrap();
    assert_eq!(
        requests.len(),
        2,
        "second request reached real HTTP after the tool executed"
    );
    assert_eq!(requests[0].1, rust_decimal::Decimal::ZERO);
    let first_charge = requests[1].1;
    assert!(first_charge > rust_decimal::Decimal::ZERO);
    let messages = requests[1].0["input"].as_array().unwrap();
    assert!(messages
        .iter()
        .all(|message| message.get("harn_assistant_publication").is_none()));
    assert!(messages
        .iter()
        .any(|message| message["type"] == "function_call"));
    let tool_result = history
        .iter()
        .find(|message| {
            message["role"] == "tool_result" && message["tool_call_id"] == "call_inspect"
        })
        .expect("executed tool result retained in history");
    let tool_content = tool_result["content"].as_str().expect("text tool result");
    assert!(tool_content.contains("inspected"), "{tool_result}");
    assert!(
        messages.iter().any(|message| {
            message["type"] == "function_call_output"
                && message["call_id"] == "call_inspect"
                && message["output"] == tool_content
        }),
        "wire result must preserve the canonical tool result: {messages:?}"
    );
    let receipt = budget.receipt().expect("read the VM's host allowance");
    assert_eq!(receipt.ceiling_usd, rust_decimal::Decimal::from(7));
    assert_eq!(
        receipt.settled_upper_usd,
        first_charge * rust_decimal::Decimal::from(2)
    );
    assert_eq!(receipt.in_flight_usd, rust_decimal::Decimal::ZERO);
    assert_eq!(receipt.uncertain_usd, rust_decimal::Decimal::ZERO);
    assert_eq!(receipt.denied_attempts, 0);
    assert!(!receipt.contract_broken);
}

#[test]
fn publication_egress_preserves_strict_pre_dispatch_refusals() {
    let _env = env_guard();
    let _transport = allow_stubbed_llm_transport();
    let _cleanup = Cleanup;
    crate::llm::cost::reset_cost_state();
    let count = Arc::new(AtomicUsize::new(0));
    let server = stub(count.clone(), false, false);
    install(server.addr());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        for message in [
            json!({"role":"assistant","content":"draft","harn_assistant_publication":"invalid"}),
            json!({"role":"assistant","content":"draft","harn_assistant_publication":null}),
            json!({"role":"assistant","content":"draft","harn_assistant_publication":{"withheld":null}}),
            json!({"role":"user","content":"draft","harn_assistant_publication":"published"}),
            json!({"role":"assistant","content":"draft","harn_assistant_publication":"withheld","unknown_metadata":true}),
        ] {
            let input = json!({"provider":"openai","model":"gpt-5.6-luna","messages":[message],
                "max_tokens":64,"stream":false,"budget":{"admission":"conservative","total_budget_usd":7.0}});
            let opts = crate::llm::helpers::extract_llm_options(&[
                crate::value::VmValue::string(""), crate::value::VmValue::Nil,
                crate::schema::json_to_vm_value(&input),
            ]).unwrap();
            let error = vm_call_llm_full(&opts).await.expect_err("unknown or misplaced metadata is refused");
            assert!(error.to_string().contains("unsupported_billing_shape"), "{error}");
            assert_eq!(count.load(Ordering::SeqCst), 0);
        }
        let mut unsupported = options(7.0, false);
        unsupported.provider_tools = vec![json!({"type":"web_search"})];
        let error = vm_call_llm_full(&unsupported).await.unwrap_err();
        assert!(error.to_string().contains("unsupported_billing_shape"), "{error}");
        assert_eq!(count.load(Ordering::SeqCst), 0);
        // Known-positive transport control makes the zero meaningful.
        assert_eq!(vm_call_llm_full(&options(7.0, false)).await.unwrap().text, "hello");
        assert_eq!(count.load(Ordering::SeqCst), 1);
    });
}
