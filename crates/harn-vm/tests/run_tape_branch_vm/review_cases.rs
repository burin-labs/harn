use super::*;
use harn_vm::testbench::tape::{
    FieldChange, TapeRecordKind, TranscriptChange, TranscriptLifecycle,
};

fn git(root: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgSign=false",
        ])
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_AUTHOR_NAME", "Replay fixture")
        .env("GIT_AUTHOR_EMAIL", "replay@example.invalid")
        .env("GIT_COMMITTER_NAME", "Replay fixture")
        .env("GIT_COMMITTER_EMAIL", "replay@example.invalid")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
}

#[test]
fn readonly_git_methods_serve_recorded_commands_without_a_repository() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let dirs = dirs();
    git(&dirs.workspace, &["init", "--initial-branch=main"]);
    git(
        &dirs.workspace,
        &["commit", "--allow-empty", "-m", "fixture"],
    );
    let source = format!(
        r#"
import {{ with_llm_script }} from "std/testing"
fn main(harness: Harness) {{
  const conflicts = harness.process.git_conflicts({root})
  assert(conflicts.success)
  const remote = harness.process.git_ls_remote({root}, {root}, {{}})
  assert(remote.success)
  harness.stdio.println(json_stringify(remote.data))
  with_llm_script(harness.llm, [{{text: "first"}}, {{text: "second"}}], {{ ->
    harness.stdio.println(harness.llm.call("first", nil, {{provider: "mock", model: "mock"}}).text)
    harness.stdio.println(harness.llm.call("second", nil, {{provider: "mock", model: "mock"}}).text)
  }})
}}
"#,
        root = quote(&dirs.workspace),
    );
    let recorded = record(&source, &dirs.tapes, &dirs.workspace);
    assert!(recorded.contains("refs/heads/main"), "{recorded}");
    let tape = EventTape::load(&dirs.tapes.join(TAPE_FILE_NAME)).unwrap();
    let commands = tape.records.iter().filter(|record| {
        matches!(&record.kind, TapeRecordKind::ToolCall { tool, unservable: None, .. } if tool.starts_with("process:"))
    }).count();
    assert_eq!(commands, 2, "both real Git commands must be recorded");
    std::fs::remove_dir_all(dirs.workspace.join(".git")).unwrap();
    let replay = branch(&source, &dirs.tapes, &dirs.workspace, 1);
    let output = replay
        .output
        .expect("Git reads are served without executing Git");
    assert_eq!(output, recorded);
    assert_eq!(replay.report.served_tool_calls, 2);
    assert_eq!(replay.report.served_calls, 1);
    assert_eq!(replay.report.live_calls, 1);
    assert_eq!(replay.report.divergence, None);
    assert!(!dirs.workspace.join(".git").exists());
}

#[test]
fn a_served_dispatch_preserves_a_closed_child_transcript() {
    assert_closed_child_replays("harness.agent.open(\"ephemeral-review-child\")");
}

#[test]
fn a_served_dispatch_preserves_a_closed_fork_transcript() {
    assert_closed_child_replays("harness.agent.fork(sid, \"ephemeral-review-child\")");
}

fn assert_closed_child_replays(create: &str) {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let dirs = dirs();
    let hook_marker = dirs.markers.join("hook-marker");
    let exec_marker = dirs.markers.join("exec-marker");
    let change = format!(
        r#"
        const child = {create}
        harness.agent.inject(child, {{role: "user", content: "closed child evidence"}})
        harness.agent.close(child)
    "#
    );
    let source = transcript_change_script(&hook_marker, &exec_marker, &change);
    record(&source, &dirs.tapes, &dirs.workspace);
    let tape = EventTape::load(&dirs.tapes.join(TAPE_FILE_NAME)).unwrap();
    assert!(
        tape.records.iter().any(|record| {
            matches!(&record.kind, TapeRecordKind::ToolCall { transcript_changes, .. }
            if transcript_changes.iter().any(|change| matches!(change,
                TranscriptChange::Fields { session_id, fields }
                    if session_id == "ephemeral-review-child" && !fields.is_empty())))
        }),
        "the closed child's changes must be captured"
    );
    remove_marker(&hook_marker, &dirs.tapes);
    remove_marker(&exec_marker, &dirs.tapes);
    let replay = branch(&source, &dirs.tapes, &dirs.workspace, 2);
    replay
        .output
        .expect("the closed child does not prevent replay");
    assert_eq!(replay.report.divergence, None);
    assert_eq!(replay.report.served_tool_calls, 1);
    assert!(
        !exec_marker.exists(),
        "the live dispatch must not run again"
    );
    assert!(!hook_marker.exists(), "the live hook must not run again");

    for (case, expected) in [
        ("missing-open", "is not open"),
        ("invalid-field", "is not a list to extend"),
        ("missing-close", "1 recorded new session(s) remain open"),
    ] {
        let mut changed = tape.clone();
        let mut edited = 0;
        for record in &mut changed.records {
            let TapeRecordKind::ToolCall {
                transcript_changes, ..
            } = &mut record.kind
            else {
                continue;
            };
            transcript_changes.retain_mut(|change| match change {
                TranscriptChange::Lifecycle {
                    session_id,
                    lifecycle: TranscriptLifecycle::Opened { .. },
                } if session_id == "ephemeral-review-child" && case == "missing-open" => {
                    edited += 1;
                    false
                }
                TranscriptChange::Lifecycle {
                    session_id,
                    lifecycle: TranscriptLifecycle::Closed,
                } if session_id == "ephemeral-review-child" && case == "missing-close" => {
                    edited += 1;
                    false
                }
                TranscriptChange::Fields { session_id, fields }
                    if session_id == "ephemeral-review-child" && case == "invalid-field" =>
                {
                    fields.insert(
                        "absent-field".to_owned(),
                        FieldChange::Append(vec![serde_json::json!("invalid")]),
                    );
                    edited += 1;
                    true
                }
                _ => true,
            });
        }
        assert!(
            edited > 0,
            "the negative control must change the recorded lifecycle"
        );
        name_session_store(&dirs.workspace);
        let guard = start_branch(BranchPlan {
            tape: changed,
            live_after_calls: 2,
            workspace_root: dirs.workspace.clone(),
            state_root: dirs.workspace.parent().unwrap().join(case),
            host_state_roots: Vec::new(),
            prompt_edits: Vec::new(),
        })
        .unwrap();
        assert!(run_in(&source, &dirs.workspace, Some(guard.scope())).is_err());
        let report = guard.finish();
        let divergence = report.divergence.expect("invalid lifecycle must refuse");
        assert!(
            divergence.detail.contains(expected),
            "{case}: {divergence:?}"
        );
        assert!(!exec_marker.exists());
    }
}
