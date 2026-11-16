//! Workspace facts are standing context, not a fresh directive every turn.

use super::{fresh_session_id, out_lines, run_with_bridge};

fn run_case(body: &str) {
    let source = r#"
import { with_llm_script, llm_text } from "std/testing"

fn run_turn(harness: Harness, session: string) {
  return agent_loop(harness, "Continue the task.", "Base system.", {
    provider: "mock", model: "o3", session_id: session,
    root: "__HARN_TEST_SESSION_STORE_ROOT__", max_iterations: 1,
    auto_context_profile: false,
  })
}

fn anchor_count(call: dict, text: string) -> int {
  let count = 0
  for message in call.messages {
    count = count + len(to_string(message.content).split(text)) - 1
  }
  return count
}

fn standing(harness: Harness, session: string) -> list {
  return transcript_events_by_kind(harness.agent.snapshot(session), "system_reminder")
    .filter({ event -> event.reminder.dedupe_key == "workspace_anchor" }).to_list()
}

pipeline main(harness: Harness) {
  with_llm_script(harness.llm,
    [llm_text("one ##DONE##"), llm_text("two ##DONE##"), llm_text("three ##DONE##"),
     llm_text("four ##DONE##"), llm_text("five ##DONE##"), llm_text("six ##DONE##")],
    { ->
      const session = harness.agent.open("__SESSION__", {
        workspace_anchor: {primary: "/workspace/a", anchored_at: "stable-time"},
      })
      __BODY__
      harness.stdio.log("workspace anchor delivery verified")
    })
}
"#
    .replace("__SESSION__", &fresh_session_id("workspace-delivery"))
    .replace("__BODY__", body);
    let output = run_with_bridge(&source).expect("canonical agent loop must execute");
    assert_eq!(out_lines(&output), ["workspace anchor delivery verified"]);
}

#[test]
fn unchanged_anchor_is_committed_once_across_three_real_requests() {
    run_case(
        r#"
      run_turn(harness, session)
      const first_id = standing(harness, session)[0].reminder.id
      run_turn(harness, session)
      run_turn(harness, session)
      const calls = harness.llm.mock_calls()
      assert_eq(len(calls), 3, "three actual model requests are required")
      for call in calls {
        assert_eq(anchor_count(call, "primary: /workspace/a"), 1,
          "unchanged workspace facts occur once in each served history")
      }
      const active = standing(harness, session)
      assert_eq(len(active), 1)
      assert_eq(active[0].reminder.id, first_id, "the original standing record is retained")
      assert_eq(active[0].reminder.ttl_turns, nil)
      assert(active[0].reminder.preserve_on_compact)
      assert_eq(calls[0].messages, calls[1].messages.slice(0, len(calls[0].messages)),
        "later requests retain the exact committed prefix")
    "#,
    );
}

#[test]
fn root_mount_and_clear_updates_replace_only_the_active_record() {
    run_case(
        r#"
      run_turn(harness, session)
      assert(harness.agent.set_workspace_anchor(session, {
        primary: "/workspace/b", anchored_at: "changed-time",
        additional_roots: [{path: "/workspace/lib", mount_mode: "read_only", mounted_at: "mount-time"}],
      }))
      run_turn(harness, session)
      const changed = standing(harness, session)
      assert_eq(len(changed), 1)
      assert(contains(changed[0].reminder.body, "primary: /workspace/b"))
      assert(!contains(changed[0].reminder.body, "primary: /workspace/a"))
      assert(harness.agent.set_workspace_anchor(session, nil))
      run_turn(harness, session)
      run_turn(harness, session)
      const calls = harness.llm.mock_calls()
      assert_eq(len(calls), 4)
      assert_eq(anchor_count(calls[1], "primary: /workspace/b"), 1)
      assert_eq(anchor_count(calls[1], "/workspace/lib (mount_mode: read_only, mounted_at: mount-time)"), 1)
      for call in calls.slice(2) {
        assert_eq(anchor_count(call, "Workspace anchor cleared."), 1)
      }
      const cleared = standing(harness, session)
      assert_eq(len(cleared), 1)
      assert(contains(cleared[0].reminder.body, "Workspace anchor cleared."))
      assert(!contains(cleared[0].reminder.body, "primary:"))
      assert_eq(harness.agent.workspace_anchor(session), nil)
    "#,
    );
}

#[test]
fn compaction_and_fork_preserve_standing_workspace_facts() {
    run_case(
        r#"
      run_turn(harness, session)
      compaction.policy({session_id: session, max_tokens: 1, keep_first: 0, keep_last: 0})
      const compacted = compaction.run(session, {
        strategy: "custom", summarize_fn: { _messages, _reminders -> {summary: "Task recap."} },
      })
      assert(compacted.compacted)
      assert(compacted.archived_messages > 0, "the old committed anchor really was archived")
      assert_eq(len(standing(harness, session)), 1)
      run_turn(harness, session)
      const forked = harness.agent.fork(session)
      run_turn(harness, forked)
      assert(harness.agent.set_workspace_anchor(forked, {primary: "/workspace/child", anchored_at: "child-time"}))
      run_turn(harness, forked)
      const calls = harness.llm.mock_calls()
      assert_eq(len(calls), 4)
      assert_eq(anchor_count(calls[1], "primary: /workspace/a"), 1)
      assert_eq(anchor_count(calls[2], "primary: /workspace/a"), 1)
      assert_eq(anchor_count(calls[3], "primary: /workspace/child"), 1)
      assert(contains(standing(harness, session)[0].reminder.body, "primary: /workspace/a"))
      assert(contains(standing(harness, forked)[0].reminder.body, "primary: /workspace/child"))
    "#,
    );
}

#[test]
fn anchorless_restart_does_not_restore_workspace_authority() {
    run_case(
        r#"
      run_turn(harness, session)
      harness.agent.set_workspace_anchor(session, nil)
      run_turn(harness, session)
      harness.agent.close(session)
      const reopened = harness.agent.open(session)
      assert_eq(harness.agent.workspace_anchor(reopened), nil)
      run_turn(harness, reopened)
      const fresh = harness.agent.open("__SESSION__-untouched")
      run_turn(harness, fresh)
      const calls = harness.llm.mock_calls()
      assert_eq(len(calls), 4)
      assert_eq(anchor_count(calls[1], "Workspace anchor cleared."), 1)
      assert_eq(len(standing(harness, reopened)), 0, "closed transcripts do not restore active authority")
      assert_eq(anchor_count(calls[3], "<workspace-anchor>"), 0,
        "an untouched anchorless session gets no synthetic withdrawal")
    "#,
    );
}
