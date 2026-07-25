//! Proof that the ACP `host_call` route shares harn-vm's per-turn memo.
//!
//! This adapter re-registers the `host_call` builtin, which means the memo
//! harn-vm applies inside its own dispatch path never sees an ACP-hosted
//! session. That gap was invisible for exactly as long as it existed: harn-vm's
//! own turn-cache test passes either way, because it exercises the dispatch path
//! this adapter replaces. These tests exercise the ACP route itself.
//!
//! Regression cover for burin-labs/burin-code#5432, where one agent turn made
//! 105 `runtime.pipeline_input` round-trips to the editor across 4 iterations —
//! the ~20-per-turn figure harn#5190 introduced the memo to eliminate.

use super::*;

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

#[tokio::test(flavor = "current_thread")]
async fn turn_stable_read_is_served_without_an_acp_round_trip() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let server =
        AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx.clone()));
    let bridge = test_bridge(tx, &server);

    let params = harn_vm::value::DictMap::new();
    let memoized = VmValue::String(arcstr::ArcStr::from("memoized-input"));
    harn_vm::host_turn_cache::store_by_name("runtime.pipeline_input", &params, &memoized);

    // Bounded on purpose. A memo hit must not touch the wire at all, so this
    // should complete instantly. If the memo is ever bypassed again the call
    // falls through to a real `call_client` with nothing to answer it, and the
    // 5-minute `host_call_timeout` would make this guard take five minutes to
    // report the regression it exists to catch — verified when the negative
    // control for this test failed in 300s. Fail in seconds instead.
    let args = VmValue::dict_map(Default::default());
    let value = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        super::super::builtins::host_call_via_bridge(&bridge, "runtime.pipeline_input", &args),
    )
    .await
    .expect("memo hit must not reach the wire; a timeout here means host_call bypassed the memo")
    .expect("memoized host_call");

    assert_eq!(
        value.display(),
        "memoized-input",
        "a warm memo must answer the call"
    );
    assert!(
        rx.try_recv().is_err(),
        "a memo hit must not emit a host/call round-trip to the editor"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn non_turn_stable_read_still_reaches_the_editor() {
    // The memo must not silently widen: a live read has to keep round-tripping,
    // or a mid-turn change on the host would be served stale.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let server =
        AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx.clone()));
    let bridge = test_bridge(tx, &server);

    let params = harn_vm::value::DictMap::new();
    harn_vm::host_turn_cache::store_by_name(
        "session.active_roots",
        &params,
        &VmValue::String(arcstr::ArcStr::from("stale-roots")),
    );

    // Bind the args: `tokio::pin!` keeps this future alive past the end of the
    // statement, so an inline temporary would be dropped while still borrowed.
    let args = VmValue::dict_map(Default::default());
    let call = super::super::builtins::host_call_via_bridge(&bridge, "session.active_roots", &args);
    tokio::pin!(call);

    // The call must still go out. Observe the outgoing frame rather than
    // completing the exchange: reaching the wire is the whole assertion.
    let outgoing = tokio::select! {
        message = recv_json(&mut rx) => message,
        result = &mut call => panic!("non-allowlisted read answered from the memo: {result:?}"),
    };
    assert_eq!(outgoing["method"], "host/call");
    assert_eq!(outgoing["params"]["name"], "session.active_roots");
}
