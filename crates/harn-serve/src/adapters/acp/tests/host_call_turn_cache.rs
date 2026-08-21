//! Proof that the ACP `host_call` route shares harn-vm's per-turn memo via
//! canonical dispatch — not a second memo bolted onto a replaced builtin.
//!
//! Before harn#5523, this adapter re-registered `host_call` and the memo
//! inside `dispatch_host_operation_with_ctx` never saw editor sessions.
//! These tests install the ACP [`HostCallBridge`] the same way
//! `register_acp_builtins` does, then drive the public canonical entry so a
//! regression that reintroduces a shadowed builtin fails here.

use super::*;
use harn_vm::{dispatch_host_operation, set_host_call_bridge};

fn test_bridge(
    tx: tokio::sync::mpsc::UnboundedSender<String>,
    server: &AcpServer,
) -> Arc<AcpBridge> {
    Arc::new(AcpBridge {
        session_id: "turn-cache-session".to_string(),
        output: AcpOutput::Channel(tx),
        pending: server.pending.clone(),
        next_id_counter: AtomicU64::new(1),
        cancellation: SessionCancellation::default(),
        script_name: Mutex::new(String::new()),
        assistant_state: Mutex::new(VisibleTextState::default()),
    })
}

fn install_acp_host_bridge(bridge: Arc<AcpBridge>) {
    set_host_call_bridge(Arc::new(super::super::builtins::AcpHostCallBridge::new(
        bridge,
        VmValue::List(Arc::new(Vec::new())),
    )));
}

#[tokio::test(flavor = "current_thread")]
async fn turn_stable_read_is_served_without_an_acp_round_trip() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let server =
        AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx.clone()));
    let bridge = test_bridge(tx, &server);
    install_acp_host_bridge(bridge);

    let params = harn_vm::value::DictMap::new();
    let memoized = VmValue::String(arcstr::ArcStr::from("memoized-input"));
    harn_vm::host_turn_cache::store("runtime", "pipeline_input", &params, &memoized);

    // Bounded on purpose. A memo hit must not touch the wire at all, so this
    // should complete instantly. If the memo is ever bypassed again the call
    // falls through to a real `host/call` with nothing to answer it, and the
    // 5-minute host_call timeout would make this guard take five minutes to
    // report the regression it exists to catch. Fail in seconds instead.
    let value = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        dispatch_host_operation("runtime", "pipeline_input", &params),
    )
    .await
    .expect("memo hit must not reach the wire; a timeout here means ACP bypassed canonical memo")
    .expect("memoized host_call");

    assert_eq!(
        value.display(),
        "memoized-input",
        "a warm memo must answer the call through canonical dispatch"
    );
    assert!(
        rx.try_recv().is_err(),
        "a memo hit must not emit a host/call round-trip to the editor"
    );
    harn_vm::clear_host_call_bridge();
}

#[tokio::test(flavor = "current_thread")]
async fn metadata_write_invalidates_a_memoized_acp_read_before_the_next_dispatch() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut server =
        AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx.clone()));
    let bridge = test_bridge(tx, &server);
    install_acp_host_bridge(bridge);

    let read_params = harn_vm::value::DictMap::from_iter([
        (
            harn_vm::value::intern_key("dir"),
            VmValue::String(arcstr::ArcStr::from("src/nested")),
        ),
        (
            harn_vm::value::intern_key("namespace"),
            VmValue::String(arcstr::ArcStr::from("facts")),
        ),
    ]);
    harn_vm::host_turn_cache::store(
        "project",
        "metadata_get",
        &read_params,
        &VmValue::String(arcstr::ArcStr::from("stale")),
    );

    let cached = dispatch_host_operation("project", "metadata_get", &read_params)
        .await
        .expect("memoized metadata read");
    assert_eq!(cached.display(), "stale");
    assert!(
        rx.try_recv().is_err(),
        "a metadata memo hit must not emit an ACP host/call frame"
    );

    let write_params = harn_vm::value::DictMap::from_iter([
        (
            harn_vm::value::intern_key("dir"),
            VmValue::String(arcstr::ArcStr::from("src")),
        ),
        (
            harn_vm::value::intern_key("namespace"),
            VmValue::String(arcstr::ArcStr::from("facts")),
        ),
        (
            harn_vm::value::intern_key("value"),
            VmValue::dict(harn_vm::value::DictMap::new()),
        ),
    ]);
    let write = dispatch_host_operation("project", "metadata_set", &write_params);
    tokio::pin!(write);
    let outgoing_write = tokio::select! {
        message = recv_json(&mut rx) => message,
        result = &mut write => panic!("metadata_set completed before host response: {result:?}"),
    };
    assert_eq!(outgoing_write["method"], "host/call");
    assert_eq!(outgoing_write["params"]["name"], "project.metadata_set");
    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": outgoing_write["id"].clone(),
            "result": null
        }))
        .await;
    write.await.expect("metadata_set response");

    let refreshed = dispatch_host_operation("project", "metadata_get", &read_params);
    tokio::pin!(refreshed);
    let outgoing_read = tokio::select! {
        message = recv_json(&mut rx) => message,
        result = &mut refreshed => panic!("stale metadata memo survived mutation: {result:?}"),
    };
    assert_eq!(outgoing_read["method"], "host/call");
    assert_eq!(outgoing_read["params"]["name"], "project.metadata_get");
    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": outgoing_read["id"].clone(),
            "result": "fresh"
        }))
        .await;
    assert_eq!(
        refreshed.await.expect("fresh metadata read").display(),
        "fresh"
    );

    harn_vm::clear_host_call_bridge();
}

#[tokio::test(flavor = "current_thread")]
async fn non_turn_stable_read_still_reaches_the_editor() {
    // The memo must not silently widen: a live read has to keep round-tripping,
    // or a mid-turn change on the host would be served stale.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let server =
        AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx.clone()));
    let bridge = test_bridge(tx, &server);
    install_acp_host_bridge(bridge);

    let params = harn_vm::value::DictMap::new();
    harn_vm::host_turn_cache::store(
        "session",
        "active_roots",
        &params,
        &VmValue::String(arcstr::ArcStr::from("stale-roots")),
    );

    let call = dispatch_host_operation("session", "active_roots", &params);
    tokio::pin!(call);

    // The call must still go out. Observe the outgoing frame rather than
    // completing the exchange: reaching the wire is the whole assertion.
    let outgoing = tokio::select! {
        message = recv_json(&mut rx) => message,
        result = &mut call => panic!("non-allowlisted read answered from the memo: {result:?}"),
    };
    assert_eq!(outgoing["method"], "host/call");
    assert_eq!(outgoing["params"]["name"], "session.active_roots");
    harn_vm::clear_host_call_bridge();
}

#[tokio::test(flavor = "current_thread")]
async fn hypothesis_attestation_round_trip_becomes_a_vm_local_resource_only_in_the_scoped_builtin()
{
    harn_vm::reset_thread_local_state();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut server =
        AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx.clone()));
    let bridge = test_bridge(tx, &server);
    install_acp_host_bridge(bridge);

    let event_fingerprint = format!("sha256:{}", "a".repeat(64));
    let plan_fingerprint = format!("sha256:{}", "b".repeat(64));
    let source = format!(
        r#"
pipeline main(harness: Harness) {{
  const proof = harness.obs.hypothesis_event_authority_request(
    "plan_admission",
    "{event_fingerprint}",
    "{plan_fingerprint}",
    "hyp-1",
    "receipt-1",
    nil,
  )
  return type_of(proof)
}}
"#
    );
    let chunk = harn_vm::compile_source(&source).expect("compile authority request");
    let mut vm = harn_vm::Vm::new();
    harn_vm::register_vm_stdlib(&mut vm);
    let (harness, _clock) = harn_vm::Harness::test();
    vm.set_harness(harness);
    let execution = vm.execute(&chunk);
    tokio::pin!(execution);

    let outgoing = tokio::select! {
        message = recv_json(&mut rx) => message,
        result = &mut execution => panic!("authority request completed before host response: {result:?}"),
    };
    assert_eq!(outgoing["method"], "host/call");
    assert_eq!(outgoing["params"]["name"], "hypothesis.attest_event");
    assert_eq!(
        outgoing["params"]["args"]["operation_receipt_id"],
        "receipt-1"
    );
    assert_eq!(
        outgoing["params"]["args"]["authority_kind"],
        "plan_admission"
    );
    assert_eq!(
        outgoing["params"]["args"]["event_fingerprint"],
        event_fingerprint
    );
    assert_eq!(
        outgoing["params"]["args"]["plan_fingerprint"],
        plan_fingerprint
    );
    assert_eq!(outgoing["params"]["args"]["hypothesis_id"], "hyp-1");
    assert!(outgoing["params"]["args"]["run_id"].is_null());

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": outgoing["id"].clone(),
            "result": {
                "_meta": {
                    "harn": {
                        "hostResult": {
                            "schema": "harn.host-result.v1",
                            "kind": "hypothesis_native_attestation"
                        }
                    }
                }
            }
        }))
        .await;
    let result = execution
        .await
        .expect("tagged native success should mint a resource");
    assert_eq!(result.display(), "resource");
    harn_vm::clear_host_call_bridge();
}

#[tokio::test(flavor = "current_thread")]
async fn process_exec_host_call_is_gated_by_command_policy_on_acp_bridge() {
    // Acceptance for #5523: installing the ACP bridge must not let
    // host_call("process.exec", ...) bypass harn's deny-patterns.
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let server =
        AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx.clone()));
    let bridge = test_bridge(tx, &server);
    install_acp_host_bridge(bridge);

    harn_vm::orchestration::clear_command_policies();
    harn_vm::orchestration::push_command_policy(harn_vm::orchestration::CommandPolicy {
        tools: vec!["run".to_string()],
        workspace_roots: Vec::new(),
        default_shell_mode: "shell".to_string(),
        deny_patterns: vec!["cat *".to_string()],
        require_approval: Default::default(),
        deny_labels: Default::default(),
        pre: None,
        post: None,
        consent: None,
        allow_recursive: false,
    });

    let result = dispatch_host_operation(
        "process",
        "exec",
        &harn_vm::value::DictMap::from_iter([
            (
                harn_vm::value::intern_key("mode"),
                VmValue::String(arcstr::ArcStr::from("shell")),
            ),
            (
                harn_vm::value::intern_key("command"),
                VmValue::String(arcstr::ArcStr::from("cat Cargo.toml")),
            ),
        ]),
    )
    .await
    .expect("process.exec should return a structured denial, not a thrown error");

    harn_vm::orchestration::clear_command_policies();
    harn_vm::clear_host_call_bridge();

    let dict = result.as_dict().expect("process.exec returns a dict");
    assert_eq!(
        dict.get("status").map(VmValue::display).unwrap_or_default(),
        "blocked",
        "ACP-bridged host_call(process.exec) must observe command policy; got {result:?}"
    );
    assert!(
        dict.get("reason")
            .map(VmValue::display)
            .unwrap_or_default()
            .contains("cat *"),
        "blocked result should name the matched policy pattern; got {result:?}"
    );
}
