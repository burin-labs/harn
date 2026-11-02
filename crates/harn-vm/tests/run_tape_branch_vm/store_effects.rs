use super::*;

/// One `mark` tool call, then done: the hook and the tool's exec each touch a
/// marker, and the tool writes the store.
pub(super) fn mark_script(hook_marker: &Path, exec_marker: &Path) -> String {
    mark_script_with_store_operations(
        hook_marker,
        exec_marker,
        r#"harness.runtime.store_set("tool_key", "tool ran")"#,
    )
}

fn mark_script_with_store_operations(
    hook_marker: &Path,
    exec_marker: &Path,
    store_operations: &str,
) -> String {
    format!(
        r#"
import {{ agent_loop }} from "std/agent/loop"
import {{ llm_text, with_llm_script }} from "std/testing"

pipeline main(harness: Harness, task: unknown) {{
  const id = harness.random.uuid()
  const now = harness.clock.now_ms()
  harness.runtime.store_set("script_key", id)
  harness.tools.clear_hooks()
  harness.tools.register_hook(
    {{
      pattern: "mark",
      pre: {{ _event ->
        harness.fs.write_text({hook_marker}, "hook ran")
        return nil
      }},
    }},
  )
  let tools = tool_define(
    tool_registry(),
    "mark",
    "Touch the exec marker and write the store.",
    {{
      parameters: {{}},
      handler: {{ _args ->
        harness.process.run({{program: "touch", args: [{exec_marker}]}})
        {store_operations}
        return "marked"
      }},
    }},
  )
  const result = with_llm_script(
    harness.llm,
    [
      {{tool_calls: [{{id: "mark-1", name: "mark", arguments: {{}}}}]}},
      llm_text("<user_response>Marked.</user_response><done>##DONE##</done>"),
    ],
    {{ ->
      return agent_loop(
        harness,
        "mark run " + id + " at " + to_string(now),
        nil,
        {{
          provider: "mock",
          tools: tools,
          tool_format: "native",
          max_iterations: 3,
          loop_until_done: true,
          // Completion is not under test: a deterministic yes keeps the
          // terminal status independent of the runner's verifier facts.
          verify_completion: {{ _payload -> return true }},
          session_id: "branch-" + id,
        }},
      )
    }},
  )
  harness.tools.clear_hooks()
  harness.runtime.store_set("after_key", result.status)
  harness.stdio.log(result.status)
}}
"#,
        hook_marker = quote(hook_marker),
        exec_marker = quote(exec_marker),
    )
}

pub(super) fn assert_clear_and_delete() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dirs = dirs();
    let hook_marker = dirs.markers.join("hook-marker");
    let exec_marker = dirs.markers.join("exec-marker");
    let large = "stored value ".repeat(500);
    let operations = format!(
        r#"
        harness.runtime.store_set("temporary", "before clear")
        harness.runtime.store_clear()
        harness.runtime.store_set("removed", "before delete")
        harness.runtime.store_set("large", {})
        harness.runtime.store_delete("removed")
        "#,
        serde_json::to_string(&large).unwrap()
    );
    let source = mark_script_with_store_operations(&hook_marker, &exec_marker, &operations);
    record(&source, &dirs.tapes, &dirs.workspace);
    remove_marker(&hook_marker, &dirs.tapes);
    remove_marker(&exec_marker, &dirs.tapes);
    let Branched {
        output,
        report,
        state,
    } = branch(&source, &dirs.tapes, &dirs.workspace, 1);
    assert!(output.unwrap().contains("[harn] done"));
    assert_eq!(report.divergence, None, "{report:?}");
    assert_eq!(report.served_tool_calls, 1);
    assert_eq!(report.live_calls, 1);
    assert!(!hook_marker.exists());
    assert!(!exec_marker.exists());
    let store = store_at(&state);
    assert_eq!(store["large"], large);
    assert_eq!(store["after_key"], "done");
    for removed in ["script_key", "temporary", "removed"] {
        assert!(store.get(removed).is_none(), "{removed} survived replay");
    }
    let tape = EventTape::load(&dirs.tapes.join(TAPE_FILE_NAME)).unwrap();
    assert!(
        tape.records.iter().any(|record| matches!(
            &record.kind,
            harn_vm::testbench::tape::TapeRecordKind::ToolCall {
                store_changes: Some(harn_vm::testbench::tape::TapePayload::Cas { .. }),
                ..
            }
        )),
        "large store operations must reach the content-addressed payload path"
    );
}

pub(super) fn assert_served_tool_store(prefix: u64) {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dirs = dirs();
    let hook_marker = dirs.markers.join("hook-marker");
    let exec_marker = dirs.markers.join("exec-marker");
    let source = mark_script(&hook_marker, &exec_marker);

    let recorded = record(&source, &dirs.tapes, &dirs.workspace);
    assert!(
        recorded.contains("[harn] done"),
        "recorded output: {recorded}\ntool calls: {}",
        recorded_tool_calls(&dirs.tapes)
    );
    let store_path = harn_vm::runtime_paths::store_path(&dirs.workspace);
    assert!(
        hook_marker.exists(),
        "the recorded run ran the pre-tool hook"
    );
    assert!(exec_marker.exists(), "the recorded run ran the tool's exec");
    let store: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&store_path).unwrap()).unwrap();
    assert_eq!(store["tool_key"], "tool ran", "store: {store}");
    assert!(store["script_key"].is_string(), "store: {store}");

    remove_marker(&hook_marker, &dirs.tapes);
    remove_marker(&exec_marker, &dirs.tapes);
    let sentinel = "{\"sentinel\": true}";
    std::fs::write(&store_path, sentinel).unwrap();

    let Branched {
        output,
        report,
        state,
    } = branch(&source, &dirs.tapes, &dirs.workspace, prefix);
    let output = output.expect("the branched run succeeds");
    assert!(output.contains("[harn] done"), "branch output: {output}");
    assert_eq!(report.divergence, None, "report: {report:?}");
    assert_eq!(report.served_calls, prefix, "report: {report:?}");
    assert_eq!(report.served_tool_calls, 1, "report: {report:?}");
    assert!(
        report.served_reads >= 2,
        "uuid and clock served: {report:?}"
    );
    assert_eq!(report.live_calls, 2 - prefix, "report: {report:?}");
    assert!(!hook_marker.exists(), "the branch ran the pre-tool hook");
    assert!(!exec_marker.exists(), "the branch ran the tool's exec");
    assert_eq!(
        std::fs::read_to_string(&store_path).unwrap(),
        sentinel,
        "the branch wrote the recording's store file"
    );
    let branch_store = store_at(&state);
    assert_eq!(
        branch_store["tool_key"], store["tool_key"],
        "the served tool rebuilt the store in the branch's root: {branch_store}"
    );
    assert_eq!(
        branch_store["script_key"], store["script_key"],
        "the prefix rebuilt the store in the branch's root: {branch_store}"
    );
}
