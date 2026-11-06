use super::*;
use crate::run_tape::{
    self, BranchPlan, EffectScan, RecordingLevel, TapeWorkspace, ToolEffectObserver,
};
use crate::testbench::tape::EventTape;

struct NoFileEffects;

impl ToolEffectObserver for NoFileEffects {
    fn begin(&self, _root: &std::path::Path, _call_id: &str) {}
    fn end(&self, _root: &std::path::Path, _call_id: &str) -> EffectScan {
        EffectScan {
            changes: Vec::new(),
            incomplete: false,
        }
    }
}

async fn dispatch(tools: VmValue) -> VmValue {
    let mut vm = crate::new_vm();
    crate::register_vm_stdlib(&mut vm);
    vm.call_named_builtin("__host_agent_dispatch_tool_call", vec![
        crate::json_to_vm_value(&serde_json::json!({
            "id": "declared-host", "name": "request_session_input", "arguments": {"name": "fixture"}
        })), tools,
    ]).await.unwrap()
}

#[test]
fn recorded_declared_host_tools_replay_without_a_second_host_dispatch() {
    let _lock = run_tape::TEST_PROCESS_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    crate::test_support::current_thread_runtime().block_on(async {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
    let _bridge = crate::install_host_call_bridge(Arc::new(CapabilityBridge(calls.clone())));
    let mut entry = crate::value::DictMap::new();
    entry.put_str("executor", "host_bridge");
    entry.put_str("host_capability", "session_inputs.request");
    let tools = tools_dict(vec![("request_session_input", entry)]);
    let recording = run_tape::start_recording(
        &dir.path().join("tape"),
        RecordingLevel::Full,
        Some(TapeWorkspace {
            root: workspace.clone(),
            observer: Box::new(NoFileEffects),
            state_roots: Vec::new(),
        }),
    )
    .unwrap()
    .unwrap();
    let request = crate::llm::api::LlmRequestPayload {
        provider: "mock".into(),
        model: "fixture".into(),
        ..Default::default()
    };
    run_tape::record_llm_call(
        &request,
        &crate::llm::api::LlmResult {
            provider: "mock".into(),
            model: "fixture".into(),
            tool_calls: vec![serde_json::json!({
                "id": "declared-host", "name": "request_session_input", "arguments": {"name": "fixture"}
            })],
            ..Default::default()
        },
    );
    let live = dispatch(tools.clone()).await;
    assert_eq!(
        live.as_dict().unwrap().get("result").unwrap().display(),
        "input requested"
    );
    assert_eq!(
        calls.lock().unwrap().len(),
        1,
        "the recording reached the host"
    );
    assert!(recording.error().is_none());
    let path = recording.path().to_path_buf();
    drop(recording);
    let branch = run_tape::start_branch(BranchPlan {
        tape: EventTape::load(&path).unwrap(),
        live_after_calls: 1,
        workspace_root: workspace,
        state_root: dir.path().join("branch-state"),
        host_state_roots: Vec::new(),
        prompt_edits: Vec::new(),
    })
    .unwrap();
    let scope = branch.scope().enter();
    run_tape::serve_llm_call(&request).unwrap().unwrap();
    let replayed = dispatch(tools).await;
    assert_eq!(
        crate::llm::vm_value_to_json(&replayed),
        crate::llm::vm_value_to_json(&live)
    );
    assert_eq!(
        calls.lock().unwrap().len(),
        1,
        "the prefix must not call the host again"
    );
    drop(scope);
    let report = branch.finish();
    assert_eq!(report.served_tool_calls, 1);
    assert!(report.divergence.is_none());
    });
}
