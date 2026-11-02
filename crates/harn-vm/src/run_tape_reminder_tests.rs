use super::*;
use crate::llm::helpers::{ReminderSource, SystemReminder};

#[derive(Clone, Copy)]
enum ReplayCase {
    Normal,
    MissingBaseline,
    ErasedHistory,
    Legacy,
    Uncaptured,
    Rerecorded,
}

fn reminder(body: &str, ttl_turns: Option<i64>) -> SystemReminder {
    let mut reminder = SystemReminder::new(body, ReminderSource::Hook, 0);
    reminder.ttl_turns = ttl_turns;
    reminder
}

fn seed_history(session: &str, revoked: &SystemReminder, expired: &SystemReminder) {
    crate::agent_sessions::inject_reminder(session, revoked.clone()).unwrap();
    assert_eq!(
        crate::agent_sessions::revoke_reminder(session, &revoked.id).unwrap(),
        "revoked"
    );
    crate::agent_sessions::inject_reminder(session, expired.clone()).unwrap();
    let report = crate::agent_sessions::apply_reminder_post_turn(session, 0).unwrap();
    assert_eq!(report["expired_count"], 1);
    assert_eq!(
        crate::agent_sessions::revoke_reminder(session, &expired.id).unwrap(),
        "already_delivered"
    );
}

fn assert_retirement(kind: ReminderRetirement, case: ReplayCase) {
    let _lock = PROCESS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let session = crate::agent_sessions::open_or_create(None).unwrap();
    let prior_revoked = reminder("already revoked", None);
    let prior_expired = reminder("already expired", Some(1));
    seed_history(&session, &prior_revoked, &prior_expired);
    let target = reminder("retire this", Some(1));
    crate::agent_sessions::inject_reminder(&session, target.clone()).unwrap();
    let initial = crate::agent_sessions::transcript(&session).unwrap();
    let expected_status = match kind {
        ReminderRetirement::Revoked => "already_revoked",
        ReminderRetirement::Expired => "already_delivered",
    };
    let args = serde_json::json!({"reminder_id": target.id});
    let recorded_outcome;
    {
        let recording = record_full(tmp.path());
        let mut response = result("calling", Some("retire"));
        response.tool_calls[0]["name"] = serde_json::json!("retire_reminder");
        response.tool_calls[0]["arguments"] = args.clone();
        record_llm_call(&request("first"), &response);
        let mut tool = begin_tool_call("retire", "retire_reminder", &args, false);
        assert!(tool.is_some());
        let retirement = async {
            match kind {
                ReminderRetirement::Revoked => serde_json::json!(
                    crate::agent_sessions::revoke_reminder(&session, &target.id).unwrap()
                ),
                ReminderRetirement::Expired => {
                    let report =
                        crate::agent_sessions::apply_reminder_post_turn(&session, 0).unwrap();
                    assert_eq!(report["expired_count"], 1);
                    report
                }
            }
        };
        recorded_outcome = if matches!(case, ReplayCase::Uncaptured) {
            runtime.block_on(retirement)
        } else {
            runtime.block_on(capture_transcript_changes(&mut tool, retirement))
        };
        assert_eq!(
            crate::agent_sessions::revoke_reminder(&session, &target.id).unwrap(),
            expected_status
        );
        finish_tool_call(tool.unwrap(), Ok(ok(recorded_outcome.clone())));
        record_llm_call(&request("second"), &result("finished", None));
        assert_eq!(recording.error(), None);
    }
    assert!(crate::agent_sessions::close(&session));
    crate::agent_sessions::open_or_create(Some(session.clone())).unwrap();
    if !matches!(case, ReplayCase::MissingBaseline) {
        seed_history(&session, &prior_revoked, &prior_expired);
    }
    crate::agent_sessions::store_transcript(&session, initial.clone()).unwrap();
    let initial_state = crate::agent_sessions::replay_reminder_snapshot(&session).unwrap();

    let path = tmp.path().join(TAPE_FILE_NAME);
    let mut tape = EventTape::load(&path).unwrap();
    if matches!(case, ReplayCase::ErasedHistory) {
        let after = tape
            .records
            .iter_mut()
            .find_map(|record| match &mut record.kind {
                TapeRecordKind::ToolCall {
                    transcript_changes, ..
                } => transcript_changes
                    .iter_mut()
                    .find_map(|change| match change {
                        TranscriptChange::Reminders {
                            after_reminders, ..
                        } => Some(after_reminders),
                        _ => None,
                    }),
                _ => None,
            })
            .expect("the retirement must be recorded");
        assert!(after.revoked_ids.remove(&prior_revoked.id));
        tape.persist(&path).unwrap();
    }
    let workspace = tmp.path().join("branch");
    if matches!(case, ReplayCase::Legacy) {
        let mut removed = 0;
        for record in &mut tape.records {
            if let TapeRecordKind::ToolCall {
                transcript_changes, ..
            } = &mut record.kind
            {
                let before = transcript_changes.len();
                transcript_changes
                    .retain(|change| !matches!(change, TranscriptChange::Reminders { .. }));
                removed += before - transcript_changes.len();
            }
        }
        assert_eq!(
            removed, 1,
            "the legacy fixture must omit the new state record"
        );
        tape.header.version = 6;
        tape.persist(&path).unwrap();
        let loaded = EventTape::load(&path).expect("format six stays inspectable");
        std::fs::create_dir(&workspace).unwrap();
        let result = start_branch(BranchPlan {
            tape: loaded,
            live_after_calls: 1,
            workspace_root: workspace.clone(),
            state_root: fresh_branch_state(&workspace),
            host_state_roots: Vec::new(),
            prompt_edits: Vec::new(),
        });
        let Err(error) = result else {
            panic!("legacy tape cannot prove reminder state")
        };
        assert!(error.contains("format 6"), "{error}");
        assert!(!workspace.join("src/retire.txt").exists());
        assert!(crate::agent_sessions::close(&session));
        return;
    }
    let replay = branch(tmp.path(), 1, &workspace);
    let own_tapes = matches!(case, ReplayCase::Rerecorded).then(|| tempfile::tempdir().unwrap());
    let own_recording = own_tapes.as_ref().map(|dir| {
        start_recording(
            dir.path(),
            RecordingLevel::Full,
            Some(TapeWorkspace {
                root: workspace.clone(),
                observer: Box::new(FixedObserver),
                state_roots: Vec::new(),
            }),
        )
        .unwrap()
        .unwrap()
    });
    let response = serve_llm_call(&request("first")).unwrap().unwrap();
    if own_recording.is_some() {
        record_llm_call(&request("first"), &response);
    }
    let own_tool = own_recording.as_ref().map(|_| {
        begin_tool_call("retire", "retire_reminder", &args, false)
            .expect("the branch's own recording must observe the served dispatch")
    });
    let served = serve_tool_call("retire", "retire_reminder", &args);
    if matches!(
        case,
        ReplayCase::MissingBaseline | ReplayCase::ErasedHistory | ReplayCase::Uncaptured
    ) {
        assert_eq!(replay.report().applied_file_effects, 0);
        let ToolServe::Diverged(error) = served else {
            panic!("invalid reminder state must refuse")
        };
        assert!(format!("{error:?}").contains("reminder"), "{error:?}");
        assert_eq!(replay.report().served_tool_calls, 0);
        assert!(!workspace.join("src/retire.txt").exists());
        assert!(crate::values_equal(
            &initial,
            &crate::agent_sessions::transcript(&session).unwrap()
        ));
        assert_eq!(
            crate::agent_sessions::replay_reminder_snapshot(&session).unwrap(),
            initial_state
        );
        assert!(crate::agent_sessions::close(&session));
        return;
    }
    let ToolServe::Served(outcome, changes, store_changes) = served else {
        panic!("the retirement must be served")
    };
    if let Some(mut recording) = own_tool {
        recording.transcript_changes = changes;
        recording.store_changes = store_changes;
        finish_tool_call(recording, Ok(outcome.clone()));
    }
    assert_eq!(outcome, ok(recorded_outcome.clone()));
    assert_eq!(replay.report().served_tool_calls, 1);
    assert_eq!(
        std::fs::read_to_string(workspace.join("src/retire.txt")).unwrap(),
        "written"
    );
    assert!(serve_llm_call(&request("second")).is_none());
    let following_status = crate::agent_sessions::revoke_reminder(&session, &target.id).unwrap();
    assert_eq!(
        crate::agent_sessions::revoke_reminder(&session, &prior_revoked.id).unwrap(),
        "already_revoked"
    );
    assert_eq!(
        crate::agent_sessions::revoke_reminder(&session, &prior_expired.id).unwrap(),
        "already_delivered"
    );
    assert!(crate::agent_sessions::close(&session));
    assert_eq!(following_status, expected_status);
    if let Some(recording) = own_recording {
        assert_eq!(recording.error(), None);
        drop(recording);
        drop(replay);
        crate::agent_sessions::open_or_create(Some(session.clone())).unwrap();
        seed_history(&session, &prior_revoked, &prior_expired);
        crate::agent_sessions::store_transcript(&session, initial).unwrap();
        let workspace = tmp.path().join("rerecorded-branch");
        let replay = branch(own_tapes.as_ref().unwrap().path(), 1, &workspace);
        serve_llm_call(&request("first")).unwrap().unwrap();
        let served = serve_tool_call("retire", "retire_reminder", &args);
        let ToolServe::Served(outcome, _, _) = served else {
            panic!("the branch's own tape must preserve reminder retirement")
        };
        assert_eq!(outcome, ok(recorded_outcome));
        assert_eq!(replay.report().served_tool_calls, 1);
        assert_eq!(
            std::fs::read_to_string(workspace.join("src/retire.txt")).unwrap(),
            "written"
        );
        let status = crate::agent_sessions::revoke_reminder(&session, &target.id).unwrap();
        assert!(crate::agent_sessions::close(&session));
        assert_eq!(status, expected_status);
    }
}

#[test]
fn a_served_reminder_revocation_preserves_the_following_revoke_status() {
    assert_retirement(ReminderRetirement::Revoked, ReplayCase::Normal);
}

#[test]
fn a_served_reminder_expiry_preserves_the_following_revoke_status() {
    assert_retirement(ReminderRetirement::Expired, ReplayCase::Normal);
}

#[test]
fn reminder_state_mismatch_refuses_before_file_or_transcript_effects() {
    assert_retirement(ReminderRetirement::Revoked, ReplayCase::MissingBaseline);
}

#[test]
fn reminder_state_cannot_erase_previously_retired_ids() {
    assert_retirement(ReminderRetirement::Revoked, ReplayCase::ErasedHistory);
}

#[test]
fn reminder_retirement_outside_the_capture_scope_is_unservable() {
    assert_retirement(ReminderRetirement::Revoked, ReplayCase::Uncaptured);
}

#[test]
fn a_served_reminder_state_can_be_recorded_and_served_again() {
    assert_retirement(ReminderRetirement::Expired, ReplayCase::Rerecorded);
}

#[test]
fn format_six_remains_readable_but_cannot_serve_reminder_state() {
    assert_retirement(ReminderRetirement::Revoked, ReplayCase::Legacy);
}

#[test]
fn reminder_state_in_a_closed_child_is_validated_without_reopening_it() {
    let _lock = PROCESS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let child = format!("reminder-child-{}", uuid::Uuid::now_v7());
    let args = serde_json::json!({});
    {
        let recording = record_full(tmp.path());
        record_llm_call(&request("first"), &result("calling", Some("child")));
        let mut tool = begin_tool_call("child", "child_reminders", &args, false);
        assert!(tool.is_some());
        runtime.block_on(capture_transcript_changes(&mut tool, async {
            crate::agent_sessions::open_or_create(Some(child.clone())).unwrap();
            let revoked = reminder("revoke in child", None);
            let expired = reminder("expire in child", Some(1));
            seed_history(&child, &revoked, &expired);
            assert!(crate::agent_sessions::close(&child));
        }));
        finish_tool_call(tool.unwrap(), Ok(ok(serde_json::json!("closed"))));
        record_llm_call(&request("second"), &result("finished", None));
        assert_eq!(recording.error(), None);
    }
    assert!(!crate::agent_sessions::exists(&child));
    let workspace = tmp.path().join("branch");
    let replay = branch(tmp.path(), 1, &workspace);
    serve_llm_call(&request("first")).unwrap().unwrap();
    let served = serve_tool_call("child", "child_reminders", &args);
    let ToolServe::Served(outcome, _, _) = served else {
        panic!("the closed child's reminder changes must be served")
    };
    assert_eq!(outcome, ok(serde_json::json!("closed")));
    assert_eq!(replay.report().served_tool_calls, 1);
    assert_eq!(
        std::fs::read_to_string(workspace.join("src/child.txt")).unwrap(),
        "written"
    );
    assert!(!crate::agent_sessions::exists(&child));
    assert!(serve_llm_call(&request("second")).is_none());
}
