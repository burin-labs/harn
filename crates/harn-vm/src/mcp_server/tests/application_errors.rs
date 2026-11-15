use super::*;

#[tokio::test]
async fn declared_read_only_policy_blocks_mcp_handler_writes_but_hints_do_not() {
    use crate::tool_annotations::{SideEffectLevel, ToolKind};
    for effect in [
        None,
        Some(SideEffectLevel::ReadOnly),
        Some(SideEffectLevel::WorkspaceWrite),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("effect.txt");
        let mut vm = crate::new_vm();
        crate::register_vm_stdlib(&mut vm);
        vm.set_harness(crate::Harness::real());
        let harness = vm.root_harness_value().unwrap();
        let factory = compiled_closure(
            "make",
            r#"
          fn make(harness: Harness) {
            return { input: dict ->
              harness.fs.write_text(input.path, "written")
              return "written"
            }
          }
        "#,
        );
        let VmValue::Closure(handler) = vm.call_closure_pub(&factory, &[harness]).await.unwrap()
        else {
            panic!("factory must return a handler");
        };
        let mut tool = tool_def(
            "write",
            "Try a write",
            None,
            crate::mcp_tasks::McpTaskSupport::Forbidden,
        );
        tool.handler = (*handler).clone();
        tool.catalog.annotations =
            Some(serde_json::from_value(serde_json::json!({"readOnlyHint": true})).unwrap());
        tool.catalog.policy = effect.map(|side_effect_level| crate::tool_registry::ToolPolicy {
            kind: ToolKind::Read,
            side_effect_level,
        });
        let registry = script_registry(tool_set(vec![tool]));
        let response = registry
            .call_tool(
                &serde_json::json!(1),
                &serde_json::json!({
                    "name": "write", "arguments": {"path": path.to_str().unwrap()}
                }),
                &mut vm,
                None,
                false,
            )
            .await;
        let refused = effect == Some(SideEffectLevel::ReadOnly);
        assert_eq!(
            response["result"]["isError"].as_bool().unwrap_or(false),
            refused,
            "{response}"
        );
        assert_eq!(path.exists(), !refused, "{response}");
        if !refused {
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "written");
        }
    }
}

fn throwing_closure(name: &str) -> VmClosure {
    let source = format!(
        "type LookupError = {{variant: \"NotFound\", message: string}}\nfn {name}(input: dict) throws LookupError {{ throw {{variant: \"NotFound\", message: \"PRIVATE-CUSTOMER-DIAGNOSTIC-123456\"}} }}"
    );
    compiled_closure(name, &source)
}

fn compiled_closure(name: &str, source: &str) -> VmClosure {
    let program = harn_parser::check_source_strict(source).expect("valid tool function");
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
async fn success_receipt_without_output_schema_survives_inline_and_task_mcp() {
    let mut tool = tool_def(
        "scaffold",
        "Create a resource",
        None,
        crate::mcp_tasks::McpTaskSupport::Optional,
    );
    tool.handler = compiled_closure(
        "scaffold",
        r#"
        fn scaffold(input: dict) {
          return {
            schema: "harn.agent_tool_handler_result.v2", outcome: "ok",
            text: "Created and checked the resource.",
            data: {mutation_status: "applied", changed_paths: ["resources/widgets.py"],
              verification: {status: "verified", verified_paths: ["resources/widgets.py"]}},
          }
        }
    "#,
    );
    let registry = script_registry(tool_set(vec![tool]));
    assert!(registry.surface().tools[0].get("outputSchema").is_none());
    let params = serde_json::json!({"name": "scaffold", "arguments": {}});
    let inline = call(&registry, params.clone(), false).await;
    let task = call(&registry, params, true).await;
    assert_eq!(inline["result"], task["result"]);
    assert_eq!(inline["result"]["isError"], false);
    assert_eq!(
        inline["result"]["content"][0]["text"],
        "Created and checked the resource."
    );
    assert_eq!(
        inline["result"]["structuredContent"]["mutation_status"],
        "applied"
    );
    assert_eq!(
        inline["result"]["structuredContent"]["verification"]["status"],
        "verified"
    );
}

#[tokio::test]
async fn prepared_tool_is_refused_before_mcp_handler_or_task_execution() {
    for prepared in [false, true] {
        let mut tool = crate::value::DictMap::new();
        tool.put_str("name", "lookup");
        tool.put_str("description", "Look up a record");
        tool.put("parameters", VmValue::dict(crate::value::DictMap::new()));
        tool.put(
            "handler",
            VmValue::Closure(Arc::new(throwing_closure("lookup"))),
        );
        tool.insert(
            "execution".into(),
            crate::schema::json_to_vm_value(&serde_json::json!({"taskSupport": "optional"})),
        );
        if prepared {
            tool.put(
                "prepare",
                VmValue::Closure(Arc::new(empty_closure("prepare"))),
            );
        }
        tool.insert(
            "errorSchema".into(),
            crate::schema::json_to_vm_value(&serde_json::json!({
                "type": "object",
                "properties": {"variant": {"const": "NotFound"}, "message": {"type": "string"}},
                "required": ["variant", "message"], "additionalProperties": false
            })),
        );
        let mut registry = crate::value::DictMap::new();
        registry.put_str("_type", "tool_registry");
        registry.put("tools", VmValue::list(vec![VmValue::dict(tool)]));
        let tools = tool_registry_to_mcp_tools(&VmValue::dict(registry)).unwrap();
        let registry = script_registry(tools);
        let params = serde_json::json!({"name": "lookup", "arguments": {}});
        let response = call(&registry, params.clone(), false).await;
        if !prepared {
            assert_eq!(
                response["result"]["_meta"][crate::tool_registry::HARN_MCP_TOOL_CONTRACT_META_KEY]
                    ["applicationError"]["data"]["message"],
                "PRIVATE-CUSTOMER-DIAGNOSTIC-123456"
            );
        } else {
            assert_eq!(response["error"]["code"], -32602);
            assert!(response["error"]["message"]
                .as_str()
                .unwrap()
                .contains("requires approved invocation preparation"));
            // Refused at admission, so a server never hands out a task.
            let refused = registry
                .admit_tool(&serde_json::json!(2), &params)
                .expect_err("prepared tool is refused before a task exists");
            assert_eq!(refused["error"]["code"], -32602);
        }
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
    let registry = script_registry(tool_set(vec![tool]));
    let params = serde_json::json!({"name": "lookup", "arguments": {}});

    let inline = call(&registry, params.clone(), false).await;
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

    let task = call(&registry, params, true).await;
    let task_result = &task["result"];
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
    let response = call(
        &script_registry(tool_set(vec![tool])),
        serde_json::json!({"name": "lookup", "arguments": {}}),
        false,
    )
    .await;
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
