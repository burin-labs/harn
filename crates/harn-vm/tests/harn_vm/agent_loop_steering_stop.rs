use super::*;

/// Build the control row an ACP `session/cancel` writes at acceptance.
fn accepted_stop_row() -> harn_session_store::ControlEvent {
    harn_session_store::ControlEvent {
        schema: harn_session_store::CONTROL_EVENT_SCHEMA.to_string(),
        kind: harn_session_store::CONTROL_EVENT_KIND.to_string(),
        action: harn_session_store::ControlAction::Stop,
        method: "session/cancel".to_string(),
        control_id: "ctl-stop-e2e".to_string(),
        status: "cancelled".to_string(),
        requested_mode: None,
        delivery_mode: None,
        message_id: None,
        text: None,
        goal: None,
        actor: serde_json::Value::Null,
        provenance: harn_session_store::ControlProvenance::Recorded,
    }
}

/// One script, two variants, so the reads are directly comparable:
///
///   0. final status
///   1. completion-judge call count
///   2. `stop_in_judge_prompt` iff the accepted stop reached the judge's own
///      stable prefix, else `no_stop_block`
///   3. the `source` the obligation snapshot reports, which must say
///      `control_record` exactly when a row was there to read
fn stop_obligations_pipeline(session_id: &str) -> String {
    format!(
        r###"
import {{ llm_text, with_llm_script }} from "std/testing"

pipeline main(harness: Harness, task: unknown) {{
  with_llm_script(
    harness.llm,
    [
      llm_text("Added the retry. I was asked to stop, so I am reporting where I got to. ##DONE##"),
      {{text: json_stringify({{verdict: "done", detail: "The run reported its state.", goal_assessment: {{kind: "unknown", evidence_refs: []}}}})}},
    ],
    {{ ->
      const session = "{session_id}"
      const seen = harness.runtime.shared_cell(
        {{scope: "task_group", key: "stop-obl-{session_id}", initial: ""}},
      )
      const result = agent_loop(
        harness,
        "Add the retry, then run the suite and report the failing names.",
        nil,
        {{
          provider: "mock",
          session_id: session,
          root: "__HARN_TEST_SESSION_STORE_ROOT__",
          loop_until_done: true,
          done_sentinel: "##DONE##",
          max_iterations: 2,
          verify_completion_judge: {{provider: "mock", max_invocations: 2}},
          verify_completion: {{ info ->
            harness.runtime.shared_set(seen, to_string(info?.obligations?.source ?? "unset"))
            return nil
          }},
        }},
      )
      harness.stdio.log(result.status)

      const judged = harness.llm.mock_calls()
        .filter({{ call -> contains(to_string(call?.system ?? ""), "Stable completion goal") }})
        .to_list()
      harness.stdio.log(len(judged))
      if len(judged) == 0 {{
        // Absence must not read as success.
        harness.stdio.log("no_judge_call")
      }} else {{
        const prefix = to_string(judged[0].system)
        if contains(prefix, "STOPPED this run before it finished")
          && contains(prefix, "Remaining work is no longer owed") {{
          harness.stdio.log("stop_in_judge_prompt")
        }} else {{
          harness.stdio.log("no_stop_block")
        }}
      }}
      harness.stdio.log(harness.runtime.shared_snapshot(seen).value)
      // `with_llm_script` restores the previous call log when it exits, so
      // measure the physical main+judge calls while the script scope is live.
      harness.stdio.log(len(harness.llm.mock_calls()))
    }},
  )
}}
"###
    )
}

/// RED ON MAIN: a stop delivers no user message, so the transcript-only
/// derivation cannot see one and the judge holds a stopped run to the whole
/// original task — including the suite run the stop cut off.
///
/// This asserts the decision, not the code: it records the stop through the
/// same `record_control_event` seam the ACP cancel path uses, runs a real
/// bridge-backed loop on that session, and reads the system prompt the judge
/// was actually handed.
#[test]
fn an_accepted_stop_reaches_the_completion_judge() {
    let session_id = fresh_session_id("stop-obligations");
    let recorded = std::cell::Cell::new(false);
    let raw = run_with_bridge_after(&stop_obligations_pipeline(&session_id), || {
        harn_vm::agent_sessions::open_or_create(Some(session_id.clone()))
            .expect("session must open on this thread");
        recorded.set(
            harn_vm::agent_sessions::record_control_event(&session_id, &accepted_stop_row())
                .is_recorded(),
        );
    })
    .expect("script must run");
    assert!(
        recorded.get(),
        "the stop must actually land in the session's event stream, or this test \
         measures nothing"
    );

    let lines = out_lines(&raw);
    assert_eq!(lines[0], "done", "lines: {lines:?}");
    assert_eq!(
        lines[1], "1",
        "expected exactly one completion-judge call; lines: {lines:?}"
    );
    assert_eq!(
        lines[2], "stop_in_judge_prompt",
        "the judge's own system prompt must carry the accepted stop AND its \
         withdrawal framing; lines: {lines:?}"
    );
    assert_eq!(
        lines[3], "control_record",
        "the obligation snapshot must report that it read the typed record, not \
         that it fell back to the transcript; lines: {lines:?}"
    );
    assert_eq!(
        lines[4], "2",
        "only the witnessed main and completion-judge requests may run after an accepted stop; lines: {lines:?}"
    );
}

/// NEGATIVE CONTROL: the same script with no control row renders no stop block,
/// so an uninterrupted run's judge prefix is byte-identical to what it was
/// before this seam existed.
#[test]
fn a_run_with_no_control_row_renders_no_stop_block() {
    let session_id = fresh_session_id("stop-obligations-control");
    let raw = run_with_bridge(&stop_obligations_pipeline(&session_id)).expect("script must run");
    let lines = out_lines(&raw);
    assert_eq!(lines[0], "done", "lines: {lines:?}");
    assert_eq!(
        lines[1], "1",
        "control must make the same single judge call; lines: {lines:?}"
    );
    assert_eq!(
        lines[2], "no_stop_block",
        "an uninterrupted run must render no stop block; lines: {lines:?}"
    );
    assert_eq!(
        lines[3], "delivered_messages",
        "with no row to read, the snapshot must report the weaker fallback \
         reading rather than claim the run was not stopped; lines: {lines:?}"
    );
}
