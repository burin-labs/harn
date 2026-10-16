use super::*;
use harn_vm::event_log::{AnyEventLog, MemoryEventLog};
use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex, Weak};
use tokio::sync::mpsc;

type TestWorkerChannels = (
    Arc<AcpWorker>,
    mpsc::UnboundedReceiver<JsonValue>,
    mpsc::UnboundedReceiver<String>,
    mpsc::UnboundedReceiver<String>,
    mpsc::UnboundedReceiver<String>,
);

fn expected_acp_actor_chain(client_id: &str) -> JsonValue {
    json!({
        "sub": acp_operator_subject(client_id),
        "act": {
            "sub": acp_surface_actor_subject("websocket", client_id),
        },
    })
}

fn test_worker() -> TestWorkerChannels {
    let (request_tx, request_rx) = mpsc::unbounded_channel();
    let (owner_tx, owner_rx) = mpsc::unbounded_channel();
    let (controller_tx, controller_rx) = mpsc::unbounded_channel();
    let (observer_tx, observer_rx) = mpsc::unbounded_channel();
    let worker = Arc::new(AcpWorker {
        id: "worker-test".to_string(),
        request_tx: Mutex::new(Some(request_tx)),
        clients: Mutex::new(BTreeMap::from([
            (
                "owner".to_string(),
                AcpClient {
                    connection_id: "owner-conn".to_string(),
                    role: AcpAttachRole::HostOwner,
                    socket_tx: owner_tx,
                },
            ),
            (
                "controller".to_string(),
                AcpClient {
                    connection_id: "controller-conn".to_string(),
                    role: AcpAttachRole::Controller,
                    socket_tx: controller_tx,
                },
            ),
            (
                "observer".to_string(),
                AcpClient {
                    connection_id: "observer-conn".to_string(),
                    role: AcpAttachRole::Observer,
                    socket_tx: observer_tx,
                },
            ),
        ])),
        host_owner_client_id: Mutex::new(Some("owner".to_string())),
        pending_client_requests: Mutex::new(BTreeMap::new()),
        host_requests: Mutex::new(AcpHostRequests::default()),
        sessions: Mutex::new(BTreeMap::new()),
        replay_buffer: Mutex::new(VecDeque::new()),
        next_event_id: AtomicU64::new(1),
        detached_at: Mutex::new(None),
        event_log: Arc::new(AnyEventLog::Memory(MemoryEventLog::new(64))),
        hub: Weak::new(),
    });
    (worker, request_rx, owner_rx, controller_rx, observer_rx)
}

#[tokio::test(flavor = "current_thread")]
async fn acp_hub_arbitrates_permission_responses() {
    let (worker, mut request_rx, mut owner_rx, mut controller_rx, mut observer_rx) = test_worker();
    worker
        .handle_output(
            json!({
                "jsonrpc": "2.0",
                "id": 77,
                "method": "session/request_permission",
                "params": {
                    "sessionId": "session-1",
                    "toolCall": {
                        "sessionUpdate": "tool_call_update",
                        "toolCallId": "tool-1",
                        "title": "edit",
                        "kind": "other"
                    },
                    "options": [
                        {"optionId": "allow", "name": "Allow", "kind": "allow_once"},
                        {"optionId": "reject", "name": "Reject", "kind": "reject_once"}
                    ]
                },
            })
            .to_string(),
        )
        .await;

    let owner_request: JsonValue =
        serde_json::from_str(&owner_rx.recv().await.expect("owner request")).unwrap();
    let controller_request: JsonValue =
        serde_json::from_str(&controller_rx.recv().await.expect("controller request")).unwrap();
    assert_eq!(owner_request["method"], "session/request_permission");
    assert_eq!(controller_request["method"], "session/request_permission");
    assert!(observer_rx.try_recv().is_err());

    worker
        .send_client_request(
            "controller",
            AcpAttachRole::Controller,
            json!({
                "jsonrpc": "2.0",
                "id": 77,
                "result": {"outcome": {"outcome": "selected", "optionId": "allow"}},
            }),
        )
        .await
        .expect("first controller decision wins");
    let forwarded = request_rx.recv().await.expect("forwarded response");
    assert_eq!(forwarded["id"], 77);
    assert_eq!(forwarded["result"]["outcome"]["outcome"], "selected");
    assert_eq!(forwarded["result"]["outcome"]["optionId"], "allow");

    let controller_chain = expected_acp_actor_chain("controller");
    let control_events = worker
        .event_log
        .read_range(
            &Topic::new("acp.session.session-1").expect("session topic"),
            None,
            16,
        )
        .await
        .expect("control outcome event log");
    let accepted = control_events
        .iter()
        .find(|(_, event)| event.kind == "control_outcome")
        .expect("accepted control outcome");
    assert_eq!(accepted.1.payload["actor_chain"], controller_chain);
    assert_eq!(accepted.1.payload["actor"]["actorChain"], controller_chain);

    let duplicate = worker
        .send_client_request(
            "controller",
            AcpAttachRole::Controller,
            json!({
                "jsonrpc": "2.0",
                "id": 77,
                "result": {"outcome": {"outcome": "selected", "optionId": "allow"}},
            }),
        )
        .await
        .expect_err("same actor duplicate should be idempotent");
    assert!(matches!(
        duplicate,
        AcpClientRequestError::IdempotentHostDecision { .. }
    ));

    let conflict = worker
        .send_client_request(
            "owner",
            AcpAttachRole::HostOwner,
            json!({
                "jsonrpc": "2.0",
                "id": 77,
                "result": {"outcome": {"outcome": "selected", "optionId": "reject"}},
            }),
        )
        .await
        .expect_err("late conflicting decision should be rejected");
    match conflict {
        AcpClientRequestError::AlreadyDecided {
            decision,
            attempted_actor,
            attempted_payload,
        } => {
            assert_eq!(decision.actor.client_id, "controller");
            assert_eq!(
                decision.actor.actor_chain.current(),
                "acp:websocket:controller"
            );
            assert_eq!(attempted_actor.client_id, "owner");
            assert_eq!(attempted_actor.actor_chain.current(), "acp:websocket:owner");
            assert_eq!(attempted_payload["result"]["outcome"]["optionId"], "reject");
            let data = decision.error_data("already_decided", Some(attempted_actor.as_ref()));
            assert_eq!(
                data["decidedBy"]["actorChain"],
                expected_acp_actor_chain("controller")
            );
            assert_eq!(
                data["attemptedBy"]["actorChain"],
                expected_acp_actor_chain("owner")
            );
            worker
                .append_control_outcome_with_attempt(
                    &decision,
                    "rejected",
                    "already_decided",
                    Some(attempted_actor.as_ref()),
                    Some(&attempted_payload),
                )
                .await;
            let control_events = worker
                .event_log
                .read_range(
                    &Topic::new("acp.session.session-1").expect("session topic"),
                    None,
                    16,
                )
                .await
                .expect("control outcome event log");
            let rejected = control_events
                .iter()
                .find(|(_, event)| {
                    event.kind == "control_outcome"
                        && event.payload.get("status").and_then(JsonValue::as_str)
                            == Some("rejected")
                })
                .expect("rejected control outcome");
            assert_eq!(
                rejected.1.payload["attempted_actor_chain"],
                expected_acp_actor_chain("owner")
            );
            assert_eq!(
                rejected.1.payload["attempted_actor"]["actorChain"],
                expected_acp_actor_chain("owner")
            );
            assert_eq!(rejected.1.payload["attempted_decision"], attempted_payload);
        }
        other => panic!("expected AlreadyDecided, got {other:?}"),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn acp_hub_adds_actor_metadata_to_controller_controls() {
    let (worker, mut request_rx, _owner_rx, _controller_rx, _observer_rx) = test_worker();
    worker
        .send_client_request(
            "controller",
            AcpAttachRole::Controller,
            json!({
                "jsonrpc": "2.0",
                "id": 9,
                "method": "session/cancel",
                "params": {"sessionId": "session-1"},
            }),
        )
        .await
        .expect("controller cancel forwards");
    let forwarded = request_rx.recv().await.expect("forwarded cancel");
    let actor_chain = expected_acp_actor_chain("controller");
    assert_eq!(
        forwarded["params"]["_harn"]["actor"],
        json!({
            "clientId": "controller",
            "connectionId": "controller-conn",
            "role": "controller",
            "source": "websocket",
            "actorChain": actor_chain,
        })
    );
    assert_eq!(forwarded["params"]["_harn"]["actorChain"], actor_chain);
    let decoded = harn_vm::ActorChain::from_json_value(&forwarded["params"]["_harn"]["actorChain"])
        .expect("forwarded actorChain decodes");
    assert_eq!(decoded.current(), "acp:websocket:controller");
}

#[tokio::test(flavor = "current_thread")]
async fn acp_hub_rejects_duplicate_inflight_request_ids_from_same_client() {
    let (worker, mut request_rx, _owner_rx, _controller_rx, _observer_rx) = test_worker();
    worker
        .send_client_request(
            "controller",
            AcpAttachRole::Controller,
            json!({
                "jsonrpc": "2.0",
                "id": "dup",
                "method": "session/cancel",
                "params": {"sessionId": "session-1"},
            }),
        )
        .await
        .expect("first request forwards");
    let forwarded = request_rx.recv().await.expect("forwarded request");
    assert_eq!(forwarded["id"], "dup");

    let duplicate = worker
        .send_client_request(
            "controller",
            AcpAttachRole::Controller,
            json!({
                "jsonrpc": "2.0",
                "id": "dup",
                "method": "session/cancel",
                "params": {"sessionId": "session-1"},
            }),
        )
        .await
        .expect_err("same client cannot reuse an in-flight id");
    assert!(matches!(
        duplicate,
        AcpClientRequestError::DuplicateRequestId
    ));
}
