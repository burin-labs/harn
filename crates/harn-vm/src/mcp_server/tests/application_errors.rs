use super::*;

fn throwing_closure(name: &str) -> VmClosure {
    let source = format!(
        "fn {name}(input: dict) {{ throw {{variant: \"NotFound\", message: \"PRIVATE-CUSTOMER-DIAGNOSTIC-123456\"}} }}"
    );
    let program = harn_parser::check_source_strict(&source).expect("valid throwing function");
    let chunk = crate::compiler::Compiler::new()
        .compile(&program)
        .expect("compile throwing function");
    let function = chunk
        .functions
        .iter()
        .find(|function| function.name.as_str() == name)
        .expect("compiled throwing function")
        .clone();
    VmClosure {
        func: function,
        env: VmEnv::new(),
        source_dir: None,
        module_functions: None,
        module_state: None,
        retained_module_scope: None,
    }
}

#[tokio::test]
async fn declared_application_error_has_the_same_payload_inline_and_as_a_task() {
    let mut tool = tool_def(
        "lookup",
        "Look up a record",
        None,
        crate::mcp_tasks::McpTaskSupport::Optional,
    );
    tool.catalog.error_schema = Some(serde_json::json!({
        "type": "object",
        "properties": {
            "variant": {"const": "NotFound"},
            "message": {"type": "string"}
        },
        "required": ["variant", "message"],
        "additionalProperties": false
    }));
    tool.handler = throwing_closure("lookup");
    let server = McpServer::new(
        "test".to_string(),
        tool_set(vec![tool]),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
    let mut vm = crate::Vm::new();

    let inline = server
        .handle_json_rpc(
            crate::jsonrpc::request(
                1,
                "tools/call",
                stable_metadata_params(serde_json::json!({
                    "name": "lookup",
                    "arguments": {}
                })),
            ),
            &mut vm,
        )
        .await
        .expect("inline response");
    let inline_result = &inline["result"];
    assert_eq!(inline_result["isError"], true);
    assert_eq!(
        inline_result["_meta"][crate::tool_registry::HARN_MCP_TOOL_CONTRACT_META_KEY]
            ["applicationError"],
        serde_json::json!({
            "tool": "lookup",
            "data": {
                "variant": "NotFound",
                "message": "PRIVATE-CUSTOMER-DIAGNOSTIC-123456"
            }
        })
    );
    assert!(inline_result["structuredContent"].is_null());
    let inline_text = inline_result["content"][0]["text"]
        .as_str()
        .expect("safe inline text");
    assert!(inline_text.contains("declared application error"));
    assert!(!inline_text.contains("PRIVATE-CUSTOMER-DIAGNOSTIC"));

    let created = server
        .handle_json_rpc(
            crate::jsonrpc::request(
                2,
                "tools/call",
                task_client_params(serde_json::json!({
                    "name": "lookup",
                    "arguments": {}
                })),
            ),
            &mut vm,
        )
        .await
        .expect("task response");
    let task_id = created["result"]["taskId"].as_str().expect("task id");
    let read = server
        .handle_json_rpc(
            crate::jsonrpc::request(
                3,
                "tasks/get",
                task_client_params(serde_json::json!({"taskId": task_id})),
            ),
            &mut vm,
        )
        .await
        .expect("task read");
    assert_eq!(read["result"]["status"], "completed");
    assert_eq!(read["result"]["resultType"], "complete");
    let task_result = &read["result"]["result"];
    assert_eq!(task_result, inline_result);
    let task_text = task_result["content"][0]["text"]
        .as_str()
        .expect("safe task text");
    assert!(task_text.contains("declared application error"));
    assert!(!task_text.contains("PRIVATE-CUSTOMER-DIAGNOSTIC"));
}

#[tokio::test]
async fn wrong_application_error_shape_is_a_contract_failure_without_typed_metadata() {
    let mut tool = tool_def(
        "lookup",
        "Look up a record",
        None,
        crate::mcp_tasks::McpTaskSupport::Forbidden,
    );
    tool.catalog.error_schema = Some(serde_json::json!({
        "type": "object",
        "properties": {"variant": {"const": "Forbidden"}},
        "required": ["variant"],
        "additionalProperties": false
    }));
    tool.handler = throwing_closure("lookup");
    let server = McpServer::new(
        "test".to_string(),
        tool_set(vec![tool]),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
    let mut vm = crate::Vm::new();

    let response = server
        .handle_json_rpc(
            crate::jsonrpc::request(
                1,
                "tools/call",
                stable_metadata_params(serde_json::json!({
                    "name": "lookup",
                    "arguments": {}
                })),
            ),
            &mut vm,
        )
        .await
        .expect("response");
    assert_eq!(response["result"]["isError"], true);
    assert!(
        response["result"]["_meta"][crate::tool_registry::HARN_MCP_TOOL_CONTRACT_META_KEY]
            ["applicationError"]
            .is_null()
    );
    let content = response["result"]["content"][0]["text"]
        .as_str()
        .expect("safe contract failure text");
    assert!(
        content.contains("violates its declared schema"),
        "{content}"
    );
    assert!(
        !content.contains("PRIVATE-CUSTOMER-DIAGNOSTIC"),
        "{content}"
    );
}
