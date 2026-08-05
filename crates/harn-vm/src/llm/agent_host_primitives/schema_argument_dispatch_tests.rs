use super::host_agent_dispatch_tool_call;

#[tokio::test]
async fn dispatch_flattens_schema_declared_discriminator_envelope() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("dispatch-proof.txt");
    std::fs::write(&path, "schema dispatch proof\n").expect("write fixture");
    let tools = crate::stdlib::json_to_vm_value(&serde_json::json!({
        "tools": [{
            "name": "read_file",
            "description": "Read a file through the local Harn executor.",
            "parameters": {
                "operation": {
                    "type": "string",
                    "enum": ["read"]
                },
                "path": {"type": "string"}
            }
        }]
    }));
    let call = crate::stdlib::json_to_vm_value(&serde_json::json!({
        "id": "schema-envelope-dispatch",
        "name": "read_file",
        "arguments": {
            "read": {"path": path}
        }
    }));

    let result = host_agent_dispatch_tool_call(
        crate::vm::AsyncBuiltinCtx::for_test(crate::vm::Vm::new()),
        call,
        Some(&tools),
        &crate::value::DictMap::new(),
    )
    .await
    .expect("dispatch succeeds");
    let result = crate::llm::helpers::vm_value_to_json(&result);

    assert_eq!(result["ok"], serde_json::json!(true));
    assert_eq!(
        result["executor"]["kind"],
        serde_json::json!("harn_builtin")
    );
    assert_eq!(result["arguments"]["operation"], serde_json::json!("read"));
    assert_eq!(result["arguments"]["path"], serde_json::json!(path));
    assert_eq!(
        result["rendered_result"],
        serde_json::json!("1\tschema dispatch proof"),
        "the normalized call must reach the real local executor"
    );
}
