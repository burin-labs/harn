use super::*;

#[test]
fn cassette_recording_redacts_and_replays_with_new_ids() {
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "lookup",
            "arguments": { "api_key": "secret-token" }
        }
    });
    let response = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {
            "structuredContent": { "ok": true, "token": "secret-token" }
        }
    });
    let interaction = record_interaction(0, &request, &response, 7);
    assert_eq!(
        interaction.request["params"]["arguments"]["api_key"],
        crate::redact::REDACTED_PLACEHOLDER
    );
    assert_eq!(
        interaction.response["result"]["structuredContent"]["token"],
        crate::redact::REDACTED_PLACEHOLDER
    );

    let mut replayer = McpCassetteReplayer::new(McpCassette {
        interactions: vec![interaction],
        ..McpCassette::default()
    });
    let replay_request = json!({
        "jsonrpc": "2.0",
        "id": 99,
        "method": "tools/call",
        "params": {
            "name": "lookup",
            "arguments": { "api_key": "[redacted]" }
        }
    });
    let replayed = replayer
        .replay_request(&replay_request)
        .expect("replay succeeds")
        .expect("request has response");
    assert_eq!(replayed["id"], 99);
    assert!(replayer.is_finished());
}

#[test]
fn cassette_verify_flags_schema_and_structured_content_drift() {
    let recorded = McpCassette {
        interactions: vec![record_interaction(
            0,
            &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}}),
            &json!({"jsonrpc": "2.0", "id": 1, "result": {"tools": [{
                "name": "write",
                "inputSchema": {"type": "object"},
                "outputSchema": {"type": "object"},
                "annotations": {"readOnlyHint": false}
            }]}}),
            0,
        )],
        ..McpCassette::default()
    };
    let candidate = McpCassette {
        interactions: vec![record_interaction(
            0,
            &json!({"jsonrpc": "2.0", "id": 9, "method": "tools/list", "params": {}}),
            &json!({"jsonrpc": "2.0", "id": 9, "result": {"tools": [{
                "name": "write",
                "inputSchema": {"type": "object"},
                "outputSchema": {"type": "string"},
                "annotations": {"readOnlyHint": true}
            }]}}),
            0,
        )],
        ..McpCassette::default()
    };
    let report = verify_cassettes(&recorded, &candidate);
    assert!(!report.passed);
    assert!(report
        .divergences
        .iter()
        .any(|divergence| divergence.category == "tool_metadata"));
}

#[test]
fn world_runtime_mutates_state_and_scores_collateral_damage() {
    let spec = McpWorldSpec {
        initial_state: json!({
            "tickets": {
                "t1": { "status": "open", "title": "A" },
                "t2": { "status": "open", "title": "B" }
            }
        }),
        goal_state: Some(json!({
            "tickets": {
                "t1": { "status": "closed" }
            }
        })),
        tools: vec![McpWorldToolSpec {
            name: "close_ticket".to_string(),
            description: "Close a ticket".to_string(),
            input_schema: default_input_schema(),
            output_schema: None,
            annotations: Some(json!({"readOnlyHint": false, "destructiveHint": false})),
            operation: McpWorldOperation::Set {
                path: "/tickets/{id}/status".to_string(),
                value_arg: "status".to_string(),
            },
        }],
        ..McpWorldSpec::default()
    };
    let mut runtime = McpWorldRuntime::new(spec.clone());
    let response = runtime
        .handle_json_rpc(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "close_ticket",
                "arguments": { "id": "t1", "status": "closed" }
            }
        }))
        .expect("response");
    assert_eq!(response["result"]["isError"], false);
    let score = score_world_state(&spec, runtime.state());
    assert!(score.passed);

    let damaged = json!({
        "tickets": {
            "t1": { "status": "closed", "title": "A" },
            "t2": { "status": "deleted", "title": "B" }
        }
    });
    let score = score_world_state(&spec, &damaged);
    assert!(!score.passed);
    assert_eq!(score.collateral_damage[0].path, "/tickets/t2/status");
}

#[test]
fn world_fault_partial_write_mutates_then_errors() {
    let spec = McpWorldSpec {
        initial_state: json!({ "rows": { "a": { "count": 0 } } }),
        tools: vec![McpWorldToolSpec {
            name: "increment".to_string(),
            description: String::new(),
            input_schema: default_input_schema(),
            output_schema: None,
            annotations: None,
            operation: McpWorldOperation::Noop { result: json!({}) },
        }],
        faults: vec![McpWorldFaultSpec {
            tool: "increment".to_string(),
            at_call: Some(1),
            every: None,
            fault: McpWorldFault::PartialWrite {
                path: "/rows/a/count".to_string(),
                value: json!(1),
                message: "write failed after commit".to_string(),
            },
        }],
        ..McpWorldSpec::default()
    };
    let mut runtime = McpWorldRuntime::new(spec);
    let response = runtime
        .handle_json_rpc(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": "increment", "arguments": {} }
        }))
        .expect("response");
    assert_eq!(response["result"]["isError"], true);
    assert_eq!(runtime.state().pointer("/rows/a/count"), Some(&json!(1)));
}

#[test]
fn world_idempotency_key_replays_after_partial_write_fault() {
    let spec = McpWorldSpec {
        initial_state: json!({ "rows": [] }),
        tools: vec![McpWorldToolSpec {
            name: "append".to_string(),
            description: String::new(),
            input_schema: default_input_schema(),
            output_schema: None,
            annotations: None,
            operation: McpWorldOperation::Append {
                path: "/rows".to_string(),
                value_arg: "row".to_string(),
                id_field: None,
                id_prefix: None,
            },
        }],
        faults: vec![McpWorldFaultSpec {
            tool: "append".to_string(),
            at_call: Some(1),
            every: None,
            fault: McpWorldFault::PartialWrite {
                path: "/rows".to_string(),
                value: json!([{ "id": "r1" }]),
                message: "write failed after commit".to_string(),
            },
        }],
        ..McpWorldSpec::default()
    };
    let mut runtime = McpWorldRuntime::new(spec);
    let request = |id| {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {
                "name": "append",
                "arguments": {
                    "row": {"id": "r1"},
                    "idempotency_key": "append-r1"
                }
            }
        })
    };

    let first = runtime.handle_json_rpc(request(1)).expect("first response");
    assert_eq!(first["result"]["isError"], true);
    let second = runtime
        .handle_json_rpc(request(2))
        .expect("second response");
    assert_eq!(second["result"]["isError"], false);
    assert_eq!(
        runtime.state().pointer("/rows"),
        Some(&json!([{ "id": "r1" }]))
    );
}
