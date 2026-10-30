use super::*;
use harn_vm::run_tape::{
    process_recorder, start_branch, start_recording, BranchPlan, EffectScan, RecordingLevel,
    TapeWorkspace, ToolEffectObserver, TAPE_FILE_NAME,
};
use harn_vm::testbench::tape::{EventTape, TapeRecordKind};
use std::path::Path;

struct NoEffects;

impl ToolEffectObserver for NoEffects {
    fn begin(&self, _root: &Path, _call_id: &str) {}

    fn end(&self, _root: &Path, _call_id: &str) -> EffectScan {
        EffectScan {
            changes: Vec::new(),
            incomplete: false,
        }
    }
}

async fn open_session(root: &Path) -> String {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut server = AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));
    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "session/new",
            "params": {"cwd": root, "environmentPolicy": {"kind": "isolated", "grants": []}}
        }))
        .await;
    let response = recv_response_with_id(&mut rx, 1).await;
    response["result"]["sessionId"]
        .as_str()
        .unwrap_or_else(|| panic!("session/new failed: {response}"))
        .to_owned()
}

#[test]
fn a_branch_reuses_the_recorded_acp_session_identity() {
    let _lock = acp_env_lock().lock().unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let tmp = tempfile::tempdir().unwrap();
        let recorded_workspace = tmp.path().join("recorded");
        let branch_workspace = tmp.path().join("branch");
        std::fs::create_dir_all(&recorded_workspace).unwrap();
        std::fs::create_dir_all(&branch_workspace).unwrap();
        let tapes = tmp.path().join("tapes");
        let recording = start_recording(
            &tapes,
            RecordingLevel::Full,
            Some(TapeWorkspace {
                root: recorded_workspace.clone(),
                observer: Box::new(NoEffects),
                state_roots: Vec::new(),
            }),
        )
        .unwrap()
        .unwrap();
        let recorded_id = open_session(&recorded_workspace).await;

        // Arm a prefix without inference; admission must consume the recorded id
        // before the first model call.
        let recorder = process_recorder().unwrap();
        recorder.record(TapeRecordKind::LlmCall {
            request_digest: "unused-before-inference".to_owned(),
            response: recorder.payload_from_bytes(
                serde_json::to_vec(&serde_json::json!({
                    "text": "", "tool_calls": [], "input_tokens": 0, "output_tokens": 0,
                    "cache_read_tokens": 0, "cache_write_tokens": 0,
                    "model": "fixture", "provider": "fixture", "blocks": [], "logprobs": []
                }))
                .unwrap(),
            ),
            call_index: Some(1),
            request: None,
            messages: Vec::new(),
            attribution: None,
        });
        assert_eq!(recording.error(), None);
        drop(recording);
        harn_vm::agent_sessions::reset_session_store();

        let branch = start_branch(BranchPlan {
            tape: EventTape::load(&tapes.join(TAPE_FILE_NAME)).unwrap(),
            live_after_calls: 1,
            workspace_root: branch_workspace.clone(),
            state_root: tmp.path().join("branch-state"),
            host_state_roots: Vec::new(),
            prompt_edits: Vec::new(),
        })
        .unwrap();
        let replayed_id = branch.scope().run(open_session(&branch_workspace)).await;
        assert_eq!(replayed_id, recorded_id);
        assert_eq!(branch.report().divergence, None);
        // Admission alone must not be mistaken for a completed replay.
        assert_eq!(branch.finish().divergence.unwrap().what, "under-run prefix");
    });
}
