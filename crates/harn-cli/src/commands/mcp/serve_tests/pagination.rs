use super::*;

#[test]
fn list_endpoints_page_with_cursor() {
    harn_vm::initialize_runtime_assets();
    run_list_endpoints_page_with_cursor();
}

#[inline(never)]
fn run_list_endpoints_page_with_cursor() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(Box::pin(list_endpoints_page_with_cursor_async()));
}

async fn list_endpoints_page_with_cursor_async() {
    let _env_lock = lock_env().lock().await;
    let _guard = lock_harn_state();
    let _page_size = ScopedEnvVar::set(mcp_protocol::MCP_LIST_PAGE_SIZE_ENV, "1");
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);
    write_file(
        temp.path(),
        "first.harn.prompt",
        "---\nid = \"first\"\n---\nFirst",
    );
    write_file(
        temp.path(),
        "second.harn.prompt",
        "---\nid = \"second\"\n---\nSecond",
    );
    let service = McpOrchestratorService::new(&fixture_args(&temp)).unwrap();
    let mut session = init_session(&service).await;
    call_tool(
        &service,
        &mut session,
        "harn.trigger.fire",
        json!({
            "trigger_id": "cron-ok",
            "payload": { "headers": { "x-page-test": "1" } }
        }),
    )
    .await;

    let first_tools = service
        .handle_request(
            &mut session,
            harn_vm::jsonrpc::request(40, "tools/list", json!({})),
        )
        .await;
    assert_eq!(first_tools["result"]["tools"].as_array().unwrap().len(), 1);
    let tools_cursor = first_tools["result"]["nextCursor"].as_str().unwrap();
    let next_tools = service
        .handle_request(
            &mut session,
            harn_vm::jsonrpc::request(41, "tools/list", json!({"cursor": tools_cursor})),
        )
        .await;
    assert_eq!(next_tools["result"]["tools"].as_array().unwrap().len(), 1);
    assert_ne!(
        first_tools["result"]["tools"][0]["name"],
        next_tools["result"]["tools"][0]["name"]
    );

    let first_prompts = service
        .handle_request(
            &mut session,
            harn_vm::jsonrpc::request(42, "prompts/list", json!({})),
        )
        .await;
    assert_eq!(
        first_prompts["result"]["prompts"].as_array().unwrap().len(),
        1
    );
    let prompts_cursor = first_prompts["result"]["nextCursor"].as_str().unwrap();
    let next_prompts = service
        .handle_request(
            &mut session,
            harn_vm::jsonrpc::request(43, "prompts/list", json!({"cursor": prompts_cursor})),
        )
        .await;
    assert_eq!(
        next_prompts["result"]["prompts"].as_array().unwrap().len(),
        1
    );
    assert_ne!(
        first_prompts["result"]["prompts"][0]["name"],
        next_prompts["result"]["prompts"][0]["name"]
    );

    let first_resources = service
        .handle_request(
            &mut session,
            harn_vm::jsonrpc::request(44, "resources/list", json!({})),
        )
        .await;
    assert_eq!(
        first_resources["result"]["resources"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let resources_cursor = first_resources["result"]["nextCursor"].as_str().unwrap();
    let next_resources = service
        .handle_request(
            &mut session,
            harn_vm::jsonrpc::request(45, "resources/list", json!({"cursor": resources_cursor})),
        )
        .await;
    assert_eq!(
        next_resources["result"]["resources"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_ne!(
        first_resources["result"]["resources"][0]["uri"],
        next_resources["result"]["resources"][0]["uri"]
    );

    let templates = service
        .handle_request(
            &mut session,
            harn_vm::jsonrpc::request(46, "resources/templates/list", json!({})),
        )
        .await;
    assert_eq!(
        templates["result"]["resourceTemplates"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(templates["result"]["nextCursor"].is_string());

    let invalid = service
        .handle_request(
            &mut session,
            harn_vm::jsonrpc::request(47, "resources/list", json!({"cursor": "nope"})),
        )
        .await;
    assert_eq!(invalid["error"]["code"], json!(-32602));
}
