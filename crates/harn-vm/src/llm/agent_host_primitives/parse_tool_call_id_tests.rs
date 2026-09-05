use super::host_agent_parse_tool_calls_impl;
use crate::value::VmValue;

fn vm_str(value: &str) -> VmValue {
    VmValue::String(arcstr::ArcStr::from(value))
}

fn look_tool_catalog() -> VmValue {
    crate::stdlib::json_to_vm_value(&serde_json::json!({
        "tools": [
            {
                "name": "look",
                "description": "Read a file",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "file": { "type": "string" },
                        "intent": { "type": "string" }
                    },
                    "required": ["file", "intent"]
                }
            }
        ]
    }))
}

fn parse_ids(text: &str) -> Vec<String> {
    let mut vm = crate::vm::Vm::new();
    crate::register_core_stdlib(&mut vm);
    crate::stdlib::macros::register_builtin_defs(
        &mut vm,
        crate::llm::tools::PARSE_HOST_PRIMITIVE_BUILTINS,
    );
    let value = futures::executor::block_on(host_agent_parse_tool_calls_impl(
        crate::vm::AsyncBuiltinCtx::for_test(vm),
        vec![vm_str(text), look_tool_catalog(), vm_str("text")],
    ))
    .expect("parse primitive succeeds");
    let json = crate::llm::helpers::vm_value_to_json(&value);
    json.get("calls")
        .and_then(|calls| calls.as_array())
        .expect("calls array")
        .iter()
        .map(|call| {
            call.get("id")
                .and_then(|id| id.as_str())
                .expect("call id")
                .to_string()
        })
        .collect()
}

#[test]
fn parse_tool_call_ids_are_session_scoped_across_turns() {
    crate::agent_sessions::reset_session_store();
    let session_a = crate::agent_sessions::open_or_create_for_test(Some("parse-id-a".to_string()));
    let session_b = crate::agent_sessions::open_or_create_for_test(Some("parse-id-b".to_string()));
    let text = "<tool_call>\nlook({ file: \"Cargo.toml\", intent: \"read\" })\n</tool_call>";

    {
        let _guard = crate::agent_sessions::enter_current_session(session_a.clone());
        assert_eq!(parse_ids(text), vec!["tc_0"]);
        assert_eq!(parse_ids(text), vec!["tc_1"]);
    }

    {
        let _guard = crate::agent_sessions::enter_current_session(session_b);
        assert_eq!(parse_ids(text), vec!["tc_0"]);
    }

    {
        let _guard = crate::agent_sessions::enter_current_session(session_a);
        assert_eq!(parse_ids(text), vec!["tc_2"]);
    }
}

#[test]
fn parse_tool_call_ids_are_unique_within_one_turn() {
    crate::agent_sessions::reset_session_store();
    let session =
        crate::agent_sessions::open_or_create_for_test(Some("parse-id-batch".to_string()));
    let _guard = crate::agent_sessions::enter_current_session(session);
    let text = [
        "<tool_call>\nlook({ file: \"Cargo.toml\", intent: \"read\" })\n</tool_call>",
        "<tool_call>\nlook({ file: \"README.md\", intent: \"read\" })\n</tool_call>",
    ]
    .join("\n");

    assert_eq!(parse_ids(&text), vec!["tc_0", "tc_1"]);
}

#[test]
fn parse_tool_call_ids_continue_after_seeded_transcript() {
    crate::agent_sessions::reset_session_store();
    let session = crate::agent_sessions::seed_from_messages(
        Some("parse-id-seeded".to_string()),
        &[
            serde_json::json!({
                "role": "assistant",
                "content": "",
                "tool_calls": [
                    {
                        "id": "tc_5",
                        "type": "function",
                        "function": {
                            "name": "look",
                            "arguments": "{\"file\":\"Cargo.toml\",\"intent\":\"read\"}"
                        }
                    }
                ]
            }),
            serde_json::json!({
                "role": "tool",
                "tool_call_id": "tc_5",
                "content": "{}"
            }),
        ],
        serde_json::json!({}),
        None,
        Some("text".to_string()),
    )
    .expect("seed session");
    let _guard = crate::agent_sessions::enter_current_session(session);
    let text = "<tool_call>\nlook({ file: \"README.md\", intent: \"read\" })\n</tool_call>";

    assert_eq!(parse_ids(text), vec!["tc_6"]);
}
