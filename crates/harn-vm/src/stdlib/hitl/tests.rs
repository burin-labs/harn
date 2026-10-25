use std::sync::OnceLock;

use tokio::sync::Mutex;

use super::{
    HITL_APPROVALS_TOPIC, HITL_DUAL_CONTROL_TOPIC, HITL_ESCALATIONS_TOPIC, HITL_QUESTIONS_TOPIC,
};
use crate::event_log::{
    install_active_event_log, install_memory_for_current_thread, EventLog, Topic,
};
use crate::{register_vm_stdlib, reset_thread_local_state, VmError};

fn hitl_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn compile_hitl_fixture(source: &str) -> crate::Chunk {
    let program =
        harn_parser::parse_source(source).expect("trusted HITL test fixture should parse");
    crate::Compiler::with_options(crate::CompilerOptions::privileged_wire())
        .compile(&program)
        .expect("trusted HITL test fixture should compile")
}

async fn execute_hitl_script(
    base_dir: &std::path::Path,
    source: &str,
) -> Result<(String, Vec<String>, Vec<String>, Vec<String>, Vec<String>), VmError> {
    reset_thread_local_state();
    let log = install_memory_for_current_thread(super::HITL_EVENT_LOG_QUEUE_DEPTH);
    let chunk = compile_hitl_fixture(source);
    let mut vm = crate::new_vm();
    register_vm_stdlib(&mut vm);
    vm.set_source_dir(base_dir);
    vm.execute(&chunk).await?;
    let output = vm.output().trim_end().to_string();
    let question_events = event_kinds(log.clone(), HITL_QUESTIONS_TOPIC).await;
    let approval_events = event_kinds(log.clone(), HITL_APPROVALS_TOPIC).await;
    let dual_control_events = event_kinds(log.clone(), HITL_DUAL_CONTROL_TOPIC).await;
    let escalation_events = event_kinds(log, HITL_ESCALATIONS_TOPIC).await;
    Ok((
        output,
        question_events,
        approval_events,
        dual_control_events,
        escalation_events,
    ))
}

async fn event_kinds(
    log: std::sync::Arc<crate::event_log::AnyEventLog>,
    topic: &str,
) -> Vec<String> {
    log.read_range(&Topic::new(topic).expect("valid topic"), None, usize::MAX)
        .await
        .expect("read topic")
        .into_iter()
        .map(|(_, event)| event.kind)
        .collect()
}

async fn event_payloads(
    log: std::sync::Arc<crate::event_log::AnyEventLog>,
    topic: &str,
) -> Vec<serde_json::Value> {
    log.read_range(&Topic::new(topic).expect("valid topic"), None, usize::MAX)
        .await
        .expect("read topic")
        .into_iter()
        .map(|(_, event)| event.payload)
        .collect()
}

#[tokio::test(flavor = "current_thread")]
async fn ask_user_coerces_to_default_type_and_logs_events() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let source = r#"
pipeline test(harness: Harness, task: unknown) {
  host_mock("hitl", "question", {answer: "9"})
  const answer: int = harness.interaction.ask_user("Pick a number", {default: 0})
  harness.stdio.println(answer)
}
"#;
            let (output, question_events, approval_events, dual_control_events, escalation_events) =
                execute_hitl_script(dir.path(), source)
                    .await
                    .expect("script succeeds");
            assert_eq!(output, "9");
            assert_eq!(
                question_events,
                vec![
                    "hitl.question_asked".to_string(),
                    "hitl.response_received".to_string()
                ]
            );
            assert!(approval_events.is_empty());
            assert!(dual_control_events.is_empty());
            assert!(escalation_events.is_empty());
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn request_approval_waits_for_quorum_and_emits_a_record() {
    let _guard = hitl_lock().lock().await;
    tokio::task::LocalSet::new()
        .run_until(async {
            reset_thread_local_state();
            let dir = tempfile::tempdir().expect("tempdir");
            let source = r#"
pipeline test(harness: Harness, task: unknown) {
  host_mock("hitl", "approval", [
    {approved: true, reviewer: "alice", reason: "ok"},
    {approved: true, reviewer: "bob", reason: "ship it"},
  ])
  const record = harness.interaction.request_approval(
    "deploy production",
    {quorum: 2, reviewers: ["alice", "bob", "carol"]},
  )
  harness.stdio.println(record.approved)
  harness.stdio.println(len(record.reviewers))
  harness.stdio.println(record.reviewers[0])
  harness.stdio.println(record.reviewers[1])
}
"#;
            let (_, _, approval_events, _, _) = execute_hitl_script(dir.path(), source)
                .await
                .expect("script succeeds");
            assert_eq!(
                approval_events,
                vec![
                    "hitl.approval_requested".to_string(),
                    "hitl.response_received".to_string(),
                    "hitl.response_received".to_string(),
                    "hitl.approval_approved".to_string(),
                ]
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn request_approval_emits_canonical_approval_request_payload() {
    let _guard = hitl_lock().lock().await;
    tokio::task::LocalSet::new()
        .run_until(async {
            reset_thread_local_state();
            let dir = tempfile::tempdir().expect("tempdir");
            let log = install_memory_for_current_thread(super::HITL_EVENT_LOG_QUEUE_DEPTH);
            let source = r#"
pipeline test(harness: Harness, task: unknown) {
  host_mock("hitl", "approval", {approved: true, reviewer: "alice", reason: "ok"})
  harness.interaction.request_approval("deploy production", {
    args: {environment: "prod"},
    quorum: 1,
    reviewers: ["alice"],
    evidence_refs: [{kind: "run", uri: "run_123"}],
    undo_metadata: {strategy: "rollback"},
    capabilities_requested: ["deploy.production"],
  })
}
"#;
            let chunk = compile_hitl_fixture(source);
            let mut vm = crate::new_vm();
            register_vm_stdlib(&mut vm);
            vm.set_source_dir(dir.path());
            vm.execute(&chunk).await.expect("script succeeds");

            let payloads = event_payloads(log, HITL_APPROVALS_TOPIC).await;
            let request_payload = &payloads[0]["payload"];
            let approval_request = &request_payload["approval_request"];
            assert_eq!(approval_request["id"], request_payload["id"]);
            assert_eq!(approval_request["action"], "deploy production");
            assert_eq!(approval_request["args"]["environment"], "prod");
            assert_eq!(approval_request["approvers_required"], 1);
            assert_eq!(approval_request["evidence_refs"][0]["uri"], "run_123");
            assert_eq!(approval_request["undo_metadata"]["strategy"], "rollback");
            assert_eq!(
                approval_request["capabilities_requested"][0],
                "deploy.production"
            );
            assert!(approval_request["requested_at"].as_str().is_some());
            assert!(approval_request["deadline"].as_str().is_some());
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn request_approval_surfaces_denials_as_typed_errors() {
    let _guard = hitl_lock().lock().await;
    tokio::task::LocalSet::new()
        .run_until(async {
            reset_thread_local_state();
            let dir = tempfile::tempdir().expect("tempdir");
            let source = r#"
pipeline test(harness: Harness, task: unknown) {
  host_mock("hitl", "approval", {approved: false, reviewer: "alice", reason: "unsafe"})
  const denied = try {
    harness.interaction.request_approval("drop table", {reviewers: ["alice"]})
  }
  harness.stdio.println(is_err(denied))
  harness.stdio.println(unwrap_err(denied).name)
  harness.stdio.println(unwrap_err(denied).reason)
}
"#;
            let (output, _, approval_events, _, _) = execute_hitl_script(dir.path(), source)
                .await
                .expect("script succeeds");
            assert_eq!(output, "true\nApprovalDeniedError\nunsafe");
            assert_eq!(
                approval_events,
                vec![
                    "hitl.approval_requested".to_string(),
                    "hitl.response_received".to_string(),
                    "hitl.approval_denied".to_string(),
                ]
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn dual_control_executes_action_after_quorum() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let source = r#"
pipeline test(harness: Harness, task: unknown) {
  host_mock("hitl", "dual_control", [
    {approved: true, reviewer: "alice"},
    {approved: true, reviewer: "bob"},
  ])
  const result = harness.interaction.dual_control(
    2,
    3,
    { -> "launched" },
    ["alice", "bob", "carol"],
  )
  harness.stdio.println(result)
}
"#;
            let (output, _, _, dual_control_events, _) = execute_hitl_script(dir.path(), source)
                .await
                .expect("script succeeds");
            assert_eq!(output, "launched");
            assert_eq!(
                dual_control_events,
                vec![
                    "hitl.dual_control_requested".to_string(),
                    "hitl.response_received".to_string(),
                    "hitl.response_received".to_string(),
                    "hitl.dual_control_approved".to_string(),
                    "hitl.dual_control_executed".to_string(),
                ]
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn escalate_to_waits_for_acceptance_event() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let source = r#"
pipeline test(harness: Harness, task: unknown) {
  host_mock("hitl", "escalation", {accepted: true, reviewer: "lead", reason: "taking over"})
  const handle = harness.interaction.escalate_to("admin", "need override")
  harness.stdio.println(handle.status)
  harness.stdio.println(handle.reviewer)
}
"#;
            let (output, _, _, _, escalation_events) = execute_hitl_script(dir.path(), source)
                .await
                .expect("script succeeds");
            assert_eq!(output, "accepted\nlead");
            assert_eq!(
                escalation_events,
                vec![
                    "hitl.escalation_issued".to_string(),
                    "hitl.escalation_accepted".to_string(),
                ]
            );
        })
        .await;
}

/// `harn-serve` adapters (A2A `input-required`, ACP `hitl_request`)
/// rely on the canonical `AgentEvent::HitlRequested` /
/// `AgentEvent::HitlResolved` pair to bracket every HITL pause.
/// Pin the contract here so future HITL primitives keep emitting
/// the event around their waitpoint blocks.
#[tokio::test(flavor = "current_thread")]
async fn ask_user_emits_hitl_request_and_resolution_to_agent_event_sinks() {
    use std::sync::Mutex as StdMutex;

    tokio::task::LocalSet::new()
        .run_until(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let session_id = "hitl-session".to_string();
            let captured: std::sync::Arc<StdMutex<Vec<crate::agent_events::AgentEvent>>> =
                std::sync::Arc::new(StdMutex::new(Vec::new()));

            struct CaptureSink(std::sync::Arc<StdMutex<Vec<crate::agent_events::AgentEvent>>>);
            impl crate::agent_events::AgentEventSink for CaptureSink {
                fn handle_event(&self, event: &crate::agent_events::AgentEvent) {
                    self.0.lock().expect("captured").push(event.clone());
                }
            }

            // Inline the script setup rather than using the
            // `execute_hitl_script` helper: that helper calls
            // `reset_thread_local_state` (which wipes the session
            // store), so any session pushed before it would be
            // gone by the time `ask_user` runs.
            crate::reset_thread_local_state();
            let log = std::sync::Arc::new(crate::event_log::AnyEventLog::Memory(
                crate::event_log::MemoryEventLog::new(super::HITL_EVENT_LOG_QUEUE_DEPTH),
            ));
            install_active_event_log(log);

            crate::agent_events::reset_all_sinks();
            let sink: std::sync::Arc<dyn crate::agent_events::AgentEventSink> =
                std::sync::Arc::new(CaptureSink(captured.clone()));
            crate::agent_events::register_sink(session_id.clone(), sink);
            crate::agent_sessions::open_or_create_for_test(Some(session_id.clone()));
            let _guard = crate::agent_sessions::enter_current_session(session_id.clone());

            let source = r#"
pipeline test(harness: Harness, task: unknown) {
  host_mock("hitl", "question", {answer: "ok"})
  const answer: string = harness.interaction.ask_user("Are you sure?", {default: "no"})
  harness.stdio.println(answer)
}
"#;
            let chunk = compile_hitl_fixture(source);
            let mut vm = crate::new_vm();
            register_vm_stdlib(&mut vm);
            vm.set_source_dir(dir.path());
            vm.execute(&chunk).await.expect("script runs");
            assert_eq!(vm.output().trim_end(), "ok");

            let events = captured.lock().expect("captured");
            let mut iter = events.iter().filter(|event| {
                matches!(
                    event,
                    crate::agent_events::AgentEvent::HitlRequested { .. }
                        | crate::agent_events::AgentEvent::HitlResolved { .. }
                )
            });
            let requested = iter.next().expect("HitlRequested emitted");
            let resolved = iter.next().expect("HitlResolved emitted");
            assert!(iter.next().is_none(), "exactly one pair: {events:?}");

            let crate::agent_events::AgentEvent::HitlRequested {
                session_id: req_session,
                request_id: req_id,
                kind: req_kind,
                payload,
            } = requested
            else {
                panic!("expected HitlRequested, got: {requested:?}");
            };
            assert_eq!(req_session, &session_id);
            assert_eq!(req_kind, "question");
            assert!(req_id.starts_with("hitl_question_"));
            assert_eq!(payload["prompt"], "Are you sure?");

            let crate::agent_events::AgentEvent::HitlResolved {
                request_id: res_id,
                kind: res_kind,
                outcome,
                ..
            } = resolved
            else {
                panic!("expected HitlResolved, got: {resolved:?}");
            };
            assert_eq!(res_id, req_id);
            assert_eq!(res_kind, "question");
            assert_eq!(outcome, "answered");

            drop(_guard);
            crate::agent_events::reset_all_sinks();
        })
        .await;
}
