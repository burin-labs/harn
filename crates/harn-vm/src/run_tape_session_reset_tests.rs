use super::*;
use crate::testbench::tape::TranscriptLifecycle;

#[test]
fn bulk_reset_records_every_closed_session_before_replay_admission() {
    let _lock = PROCESS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    for (bulk_reset, preexisting) in [(false, false), (true, false), (true, true)] {
        crate::agent_sessions::reset_session_store();
        let tmp = tempfile::tempdir().unwrap();
        let args = serde_json::json!({});
        if preexisting {
            crate::agent_sessions::open_or_create(Some("existing".to_string())).unwrap();
        }
        {
            let guard = record_full(tmp.path());
            record_llm_call(&request("first"), &result("calling", Some("reset")));
            let mut tool = begin_tool_call("reset", "reset_sessions", &args, false);
            assert!(tool.is_some());
            runtime.block_on(capture_transcript_changes(&mut tool, async {
                crate::agent_sessions::open_or_create(Some("temporary".to_string())).unwrap();
                if bulk_reset {
                    let (_, _, refusal) = transcript::capture(async {
                        crate::agent_sessions::reset_session_store();
                    })
                    .await;
                    assert!(refusal.is_some());
                } else {
                    assert!(crate::agent_sessions::close("temporary"));
                }
            }));
            assert!(!crate::agent_sessions::exists("temporary"));
            assert!(!crate::agent_sessions::exists("existing"));
            finish_tool_call(tool.unwrap(), Ok(ok(serde_json::json!("reset"))));
            assert_eq!(guard.error(), None);
        }
        let tape = EventTape::load(&tmp.path().join(TAPE_FILE_NAME)).unwrap();
        let (changes, unservable) = tape
            .records
            .iter()
            .find_map(|record| match &record.kind {
                TapeRecordKind::ToolCall {
                    transcript_changes,
                    unservable,
                    ..
                } => Some((transcript_changes, unservable)),
                _ => None,
            })
            .expect("the reset dispatch must be recorded");
        let closed: std::collections::BTreeSet<_> = changes
            .iter()
            .filter_map(|change| match change {
                TranscriptChange::Lifecycle {
                    session_id,
                    lifecycle: TranscriptLifecycle::Closed,
                } => Some(session_id.as_str()),
                _ => None,
            })
            .collect();
        let expected = if preexisting {
            vec!["existing", "temporary"]
        } else {
            vec!["temporary"]
        };
        assert_eq!(closed, expected.into_iter().collect());
        assert_eq!(unservable.is_some(), bulk_reset, "{unservable:?}");
        if bulk_reset {
            assert!(unservable
                .as_ref()
                .unwrap()
                .contains("resets session runtime state"));
        }

        if preexisting {
            crate::agent_sessions::open_or_create(Some("existing".to_string())).unwrap();
        }
        let workspace = tmp.path().join("branch");
        let replay = branch(tmp.path(), 1, &workspace);
        serve_llm_call(&request("first")).unwrap().unwrap();
        let served = serve_tool_call("reset", "reset_sessions", &args);
        if bulk_reset {
            assert!(matches!(served, ToolServe::Diverged(_)));
            assert_eq!(crate::agent_sessions::exists("existing"), preexisting);
            assert!(!workspace.join("src/reset.txt").exists());
            assert_eq!(replay.report().applied_file_effects, 0);
        } else {
            assert!(matches!(served, ToolServe::Served(_, _)));
            assert_eq!(
                std::fs::read_to_string(workspace.join("src/reset.txt")).unwrap(),
                "written"
            );
            assert_eq!(replay.report().applied_file_effects, 1);
            assert!(replay.report().divergence.is_none());
        }
        drop(replay);
        crate::agent_sessions::reset_session_store();
    }
}
