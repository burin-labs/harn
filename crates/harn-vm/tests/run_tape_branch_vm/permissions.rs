use super::*;

#[test]
fn dynamic_redirects_keep_approval_risk_without_losing_execution_syntax() {
    for (command, uncertain_redirect) in [
        ("target=$OUTPUT && printf verifier-ok > \"$target\"", true),
        ("printf verifier-ok > proof.txt", false),
        ("printf '%s' \"$VALUE\"", false),
    ] {
        let scan = harn_vm::orchestration::command_risk_scan_json(
            &serde_json::json!({
                "request": {
                    "mode": "shell", "command": command, "cwd": "/tmp/work",
                    "shell": {"id": "sh", "platform": "unix"},
                },
                "workspace_roots": ["/tmp/work"],
            }),
            None,
        );
        assert_eq!(scan["execution_syntax_complete"], true, "{scan}");
        assert_eq!(
            scan["risk_labels"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("execution_semantics_unresolved")),
            uncertain_redirect,
            "{scan}"
        );
        if uncertain_redirect {
            assert_eq!(scan["shell_io_complete"], false, "{scan}");
            assert_eq!(scan["recommended_action"], "require_approval", "{scan}");
        }
    }
}

#[test]
fn dynamic_redirects_preserve_reader_cwd_without_allowing_directory_changes() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let root = dirs();
    enter_run(&root.workspace, &root.workspace, &[], &[]);
    let policy = harn_vm::orchestration::ToolApprovalPolicy::default();
    let commands = [
        ("if echo x >> \"$(cat WWS.path)\" 2>/dev/null; then echo WWS-R$((1+1)); else echo WWS-W$((1+1)); fi", true),
        ("cd \"$TARGET\"; cat WWS.path", false),
        ("eval \"$NEXT\"; cat WWS.path", false),
        ("$NEXT; cat WWS.path", false),
        ("echo x >> \"$(cat .env)\"", false),
    ];
    let results: Vec<_> = commands
        .iter()
        .map(|(command, allowed)| {
            (
                *command,
                *allowed,
                policy.evaluate_detailed("run", &serde_json::json!({"command": command})),
            )
        })
        .collect();
    leave_run();
    for (command, allowed, result) in results {
        assert_eq!(result.is_allow(), allowed, "{command}: {result:?}");
    }
}

#[test]
fn tool_permission_paths_follow_the_branch_workspace_before_execution() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let recorded = dirs();
    let isolated = dirs();
    let outside = dirs();
    record(&source("once"), &recorded.tapes, &recorded.workspace);
    std::fs::write(isolated.workspace.join("input.txt"), "branch input").unwrap();
    std::os::unix::fs::symlink(&outside.workspace, isolated.workspace.join("escape")).unwrap();
    let branch = start_branch(BranchPlan {
        tape: EventTape::load(&recorded.tapes.join(TAPE_FILE_NAME)).unwrap(),
        live_after_calls: 0,
        workspace: location(&isolated.workspace),
        state_root: isolated.markers.join("permission-state"),
        host_state_roots: Vec::new(),
        prompt_edits: Vec::new(),
    })
    .unwrap();
    let scope = branch.scope().enter();
    enter_run(&isolated.workspace, &isolated.workspace, &[], &[]);
    let policy = harn_vm::orchestration::ToolApprovalPolicy::default();
    let evaluate = |tool, arguments| policy.evaluate_detailed(tool, &arguments);
    let command = evaluate(
        "run",
        serde_json::json!({"command": "python3 verifier.py", "cwd": recorded.workspace}),
    );
    let read = evaluate(
        "read_file",
        serde_json::json!({"path": recorded.workspace.join("input.txt")}),
    );
    let reader = evaluate(
        "run",
        serde_json::json!({"command": "cat input.txt", "cwd": recorded.workspace}),
    );
    let sensitive = evaluate(
        "run",
        serde_json::json!({"command": "cat .env", "cwd": recorded.workspace}),
    );
    let unrelated = evaluate(
        "read_file",
        serde_json::json!({"path": outside.workspace.join("input.txt")}),
    );
    let escape = evaluate(
        "read_file",
        serde_json::json!({"path": recorded.workspace.join("escape/input.txt")}),
    );
    leave_run();
    drop(scope);
    branch.finish();
    assert!(command.is_allow(), "recorded command cwd: {command:?}");
    assert!(read.is_allow(), "recorded input path: {read:?}");
    assert!(reader.is_allow(), "reader in recorded cwd: {reader:?}");
    assert!(sensitive.is_deny(), "sensitive reader: {sensitive:?}");
    assert!(unrelated.is_deny(), "unrelated workspace: {unrelated:?}");
    assert!(escape.is_deny(), "branch symlink escape: {escape:?}");
}

fn source(scope: &str) -> String {
    r#"
import { agent_loop } from "std/agent/loop"
import { llm_text, with_llm_script } from "std/testing"

fn main(harness: Harness) {
  harness.runtime.store_set("tool_runs", 0)
  const tools = tool_define(tool_registry(), "mark", "Count an approved write.", {
    parameters: {},
    handler: { _args ->
      const before = harness.runtime.store_get("tool_runs") ?? 0
      harness.runtime.store_set("tool_runs", before + 1)
      return "marked"
    },
  })
  const result = with_llm_script(harness.llm, [
    {tool_calls: [{id: "mark-1", name: "mark", arguments: {}}]},
    {tool_calls: [{id: "mark-2", name: "mark", arguments: {}}]},
    llm_text("<user_response>Marked twice.</user_response><done>##DONE##</done>"),
  ], { ->
    return agent_loop(harness, "Mark twice.", nil, {
      provider: "mock",
      tools: tools,
      tool_format: "native",
      max_iterations: 4,
      loop_until_done: true,
      verify_completion: { _payload -> return true },
      session_id: "served-session-grant",
      permissions: {
        deny: ["mark"],
        on_escalation: { _request ->
          return {grant: "GRANT_SCOPE", approver: "fixture", reason: "same operation approved"}
        },
      },
    })
  })
  const escalations = len(transcript_events_by_kind(result.transcript, "PermissionEscalation"))
  const grants = len(transcript_events_by_kind(result.transcript, "PermissionGrant"))
  const runs = harness.runtime.store_get("tool_runs")
  harness.stdio.println("escalations=${escalations}")
  harness.stdio.println("grants=${grants}")
  harness.stdio.println("runs=${runs}")
}
"#
    .replace("GRANT_SCOPE", scope)
}

#[test]
fn a_session_grant_dispatch_is_refused_before_replaying_its_writes() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let dirs = dirs();
    let source = source("session");
    let recorded = record(&source, &dirs.tapes, &dirs.workspace);
    assert!(recorded.contains("escalations=1"), "{recorded}");
    assert!(recorded.contains("grants=2"), "{recorded}");
    let store_path = harn_vm::runtime_paths::store_path(&dirs.workspace);
    let original = std::fs::read(&store_path).unwrap();
    let stored: serde_json::Value = serde_json::from_slice(&original).unwrap();
    assert_eq!(stored["tool_runs"], 2);

    let replay = branch(&source, &dirs.tapes, &dirs.workspace, 1);
    assert!(replay.output.is_err(), "{:?}", replay.output);
    let divergence = replay
        .report
        .divergence
        .as_ref()
        .expect("an explicit refusal");
    assert!(
        divergence
            .detail
            .contains("session-scoped permission grants"),
        "{divergence:?}"
    );
    assert_eq!(replay.report.served_tool_calls, 0);
    assert_eq!(replay.report.live_calls, 0);
    assert_eq!(store_at(&replay.state)["tool_runs"], 0);
    assert_eq!(std::fs::read(&store_path).unwrap(), original,);
}

#[test]
fn a_once_only_grant_dispatch_can_serve_before_a_new_live_approval() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let dirs = dirs();
    let source = source("once");
    let recorded = record(&source, &dirs.tapes, &dirs.workspace);
    assert!(recorded.contains("escalations=2"), "{recorded}");
    assert!(recorded.contains("grants=2"), "{recorded}");
    let store_path = harn_vm::runtime_paths::store_path(&dirs.workspace);
    let original = std::fs::read(&store_path).unwrap();
    let stored: serde_json::Value = serde_json::from_slice(&original).unwrap();
    assert_eq!(stored["tool_runs"], 2);

    let replay = branch(&source, &dirs.tapes, &dirs.workspace, 1);
    let output = replay
        .output
        .expect("once-only approval has no grant to restore");
    assert_eq!(replay.report.divergence, None, "{:?}", replay.report);
    assert_eq!(replay.report.served_tool_calls, 1);
    assert!(replay.report.live_calls > 0, "{:?}", replay.report);
    assert_eq!(store_at(&replay.state)["tool_runs"], 2);
    assert_eq!(std::fs::read(&store_path).unwrap(), original);
    assert!(output.contains("grants=2"), "{output}");
    assert!(output.contains("escalations=2"), "{output}");
}

#[test]
fn legacy_tapes_without_session_grant_evidence_cannot_branch() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let dirs = dirs();
    let source = source("session");
    let recorded = record(&source, &dirs.tapes, &dirs.workspace);
    assert!(recorded.contains("escalations=1"), "{recorded}");
    assert!(recorded.contains("grants=2"), "{recorded}");
    let path = dirs.tapes.join(TAPE_FILE_NAME);
    let mut tape = EventTape::load(&path).unwrap();
    let removed_refusals = tape
        .records
        .iter_mut()
        .filter_map(|record| match &mut record.kind {
            harn_vm::testbench::tape::TapeRecordKind::ToolCall { unservable, .. } => {
                unservable.take()
            }
            _ => None,
        })
        .count();
    assert_eq!(
        removed_refusals, 2,
        "both grant-dependent calls were recorded"
    );
    // Older writers had no refusal for this permission-cache dependency.
    for legacy_version in [6, 7] {
        tape.header.version = legacy_version;
        tape.persist(&path).unwrap();
        let legacy = EventTape::load(&path).expect("older recordings stay inspectable");
        assert_eq!(legacy.header.version, legacy_version);
        let admission = start_branch(BranchPlan {
            tape: legacy,
            live_after_calls: 1,
            workspace: location(&dirs.workspace),
            state_root: dirs.markers.join(format!("legacy-{legacy_version}")),
            host_state_roots: Vec::new(),
            prompt_edits: Vec::new(),
        });
        let Err(error) = admission else {
            panic!("legacy format {legacy_version} cannot prove grant fidelity but was admitted");
        };
        assert!(error.contains("re-record"), "{error}");
    }
}
