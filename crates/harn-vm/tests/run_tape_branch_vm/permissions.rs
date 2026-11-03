use super::*;

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
            workspace_root: dirs.workspace.clone(),
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
