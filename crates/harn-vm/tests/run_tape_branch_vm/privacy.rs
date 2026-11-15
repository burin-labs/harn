use super::*;

#[test]
fn actual_vm_replays_secret_bearing_requests_without_live_inference() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    for padding in [String::new(), "public context ".repeat(700)] {
        let dirs = dirs();
        let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123456789ABCD";
        let prompt = serde_json::to_string(&format!("{padding}inspect {secret}")).unwrap();
        let source = format!(
            r#"
import {{ with_llm_script }} from "std/testing"
fn main(harness: Harness) {{
  with_llm_script(harness.llm, [{{text: "recorded response", model: "mock"}}], {{ ->
    const response = harness.llm.call({prompt}, nil, {{provider: "mock", model: "mock"}})
    harness.stdio.println(response.text)
  }})
}}
"#
        );
        assert_eq!(
            record(&source, &dirs.tapes, &dirs.workspace),
            "recorded response\n"
        );
        let stored = std::fs::read_to_string(dirs.tapes.join(TAPE_FILE_NAME)).unwrap();
        assert!(!stored.contains(secret));
        let tape = EventTape::load(&dirs.tapes.join(TAPE_FILE_NAME)).unwrap();
        let mut messages = 0;
        for record in &tape.records {
            if let TapeRecordKind::LlmCall {
                messages: payloads, ..
            } = &record.kind
            {
                for payload in payloads {
                    messages += 1;
                    let bytes = tape.resolve_payload(payload).unwrap();
                    assert!(!String::from_utf8_lossy(&bytes).contains(secret));
                }
            }
        }
        assert!(messages > 0);
        let replay = branch(&source, &dirs.tapes, &dirs.workspace, 1);
        assert_eq!(replay.output.unwrap(), "recorded response\n");
        assert_eq!(
            (replay.report.served_calls, replay.report.live_calls),
            (1, 0)
        );
        assert_eq!(replay.report.divergence, None);
    }
}

#[test]
fn actual_agent_dispatch_replays_redacted_arguments_without_running_the_tool() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let dirs = dirs();
    let marker = dirs.workspace.join("tool-fired.txt");
    let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123456789ABCD";
    let source = format!(
        r#"
import {{ agent_loop }} from "std/agent/loop"
import {{ llm_text, with_llm_script }} from "std/testing"
fn main(harness: Harness) {{
  const tools = tool_define(tool_registry(), "mark", "Record a marker.", {{
    parameters: {{message: {{type: "string"}}}},
    handler: {{ args ->
      harness.fs.write_text({}, args.message)
      return "marked"
    }},
  }})
  const result = with_llm_script(harness.llm, [
    {{tool_calls: [{{id: "mark-1", name: "mark", arguments: {{message: "{secret}"}}}}]}},
    llm_text("<user_response>Marked.</user_response><done>##DONE##</done>"),
  ], {{ ->
    return agent_loop(harness, "inspect {secret}", nil, {{
      provider: "mock", tools: tools, tool_format: "native",
      max_iterations: 3, loop_until_done: true,
      verify_completion: {{ _payload -> return true }},
    }})
  }})
  harness.stdio.println(result.status)
}}
"#,
        quote(&marker)
    );
    assert!(record(&source, &dirs.tapes, &dirs.workspace).contains("done"));
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), secret);
    std::fs::remove_file(&marker).unwrap();
    let tape = EventTape::load(&dirs.tapes.join(TAPE_FILE_NAME)).unwrap();
    assert_eq!(
        tape.records
            .iter()
            .filter(|record| matches!(record.kind, TapeRecordKind::LlmCall { .. }))
            .count(),
        2
    );
    assert_eq!(
        tape.records
            .iter()
            .filter(|record| matches!(record.kind, TapeRecordKind::ToolCall { .. }))
            .count(),
        1
    );
    let persisted = std::fs::read_to_string(dirs.tapes.join(TAPE_FILE_NAME)).unwrap();
    assert!(!persisted.contains(secret));
    for entry in std::fs::read_dir(dirs.tapes.join(format!("{TAPE_FILE_NAME}.cas"))).unwrap() {
        let bytes = std::fs::read(entry.unwrap().path()).unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains(secret));
    }
    let replay = branch(&source, &dirs.tapes, &dirs.workspace, 2);
    assert!(replay.output.unwrap().contains("done"));
    assert!(
        !marker.exists(),
        "the served tool must not run its live handler"
    );
    assert_eq!(
        (
            replay.report.served_calls,
            replay.report.served_tool_calls,
            replay.report.live_calls
        ),
        (2, 1, 0)
    );
    assert_eq!(replay.report.divergence, None);
}
