//! Integration tests for the `compaction.{policy,check,run}` primitive
//! (#2505 / epic A.8). Walks the spec triad — `defer` under threshold,
//! `compact_now` once tokens cross, and `compaction.run` driving the
//! shared #2323 lifecycle.

use harn_vm::value::VmError;

fn run(source: &str) -> Result<String, String> {
    harn_vm::reset_thread_local_state();
    let chunk = harn_vm::compile_source(source)?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut vm = harn_vm::Vm::new();
                harn_vm::register_vm_stdlib(&mut vm);
                vm.execute(&chunk)
                    .await
                    .map_err(|e: VmError| format!("{e:?}"))?;
                Ok(vm.output().to_string())
            })
            .await
    })
}

fn out(source: &str) -> Vec<String> {
    let raw = run(source).unwrap_or_else(|e| panic!("script failed: {e}"));
    raw.lines()
        .filter_map(|l| l.strip_prefix("[harn] "))
        .map(|s| s.to_string())
        .collect()
}

#[test]
fn check_defers_when_session_under_threshold() {
    let lines = out(r#"
pipeline main(harness: Harness, task) {
  const s = harness.agent.open()
  compaction.policy({session_id: s, max_tokens: 10000, max_turns: 50})
  const decision = compaction.check(s)
  harness.stdio.log(decision["action"])
  harness.stdio.log(decision["message_count"])
  harness.stdio.log(decision["trigger"])
  harness.stdio.log(decision["policy_inherited"])
}
"#);
    assert_eq!(lines, vec!["defer", "0", "manual", "false"], "{lines:?}");
}

#[test]
fn check_marks_compact_now_when_tokens_cross() {
    let lines = out(r#"
pipeline main(harness: Harness, task) {
  const s = harness.agent.open()
  compaction.policy({session_id: s, max_tokens: 1, max_turns: 100})
  harness.agent.inject(s, {role: "user", content: "Investigate the parser regression in stdlib once more."})
  harness.agent.inject(s, {role: "assistant", content: "The root cause is in diagnostic recovery; we missed an EOF guard."})
  const decision = compaction.check(s)
  harness.stdio.log(decision["action"])
  harness.stdio.log(decision["trigger"])
  harness.stdio.log(decision["strategy"])
}
"#);
    assert_eq!(
        lines,
        vec!["compact_now", "tokens", "summarize-then-prune"],
        "{lines:?}"
    );
}

#[test]
fn check_uses_default_policy_when_session_unscoped() {
    let lines = out(r#"
pipeline main(harness: Harness, task) {
  // No session_id → registers the default policy.
  compaction.policy({max_turns: 2})
  const s = harness.agent.open()
  harness.agent.inject(s, {role: "user", content: "one"})
  harness.agent.inject(s, {role: "assistant", content: "two"})
  harness.agent.inject(s, {role: "user", content: "three"})
  const decision = compaction.check(s)
  harness.stdio.log(decision["action"])
  harness.stdio.log(decision["trigger"])
  harness.stdio.log(decision["policy_inherited"])
}
"#);
    assert_eq!(lines, vec!["compact_now", "turns", "true"], "{lines:?}");
}

#[test]
fn run_compacts_session_via_lifecycle_with_custom_strategy() {
    let lines = out(r#"
pipeline main(harness: Harness, task) {
  const s = harness.agent.open()
  compaction.policy({session_id: s, max_tokens: 1, keep_last: 1})
  harness.agent.inject(s, {role: "user", content: "Investigate the parser regression."})
  harness.agent.inject(s, {role: "assistant", content: "The root cause is in diagnostic recovery."})
  harness.agent.inject(s, {role: "user", content: "Capture the next command."})

  const outcome = compaction.run(s, {
    strategy: "custom",
    summarize_fn: { archived, _reminders -> {summary: "policy compacted " + to_string(len(archived))} },
  })
  harness.stdio.log(outcome["compacted"])
  harness.stdio.log(outcome["archived_messages"])
  harness.stdio.log(outcome["engine_strategy"])
  harness.stdio.log(outcome["strategy"])
  harness.stdio.log(transcript_summary(outcome["transcript"])?.contains("policy compacted"))
}
"#);
    assert_eq!(
        lines,
        vec!["true", "2", "custom", "custom", "true"],
        "{lines:?}"
    );
}

#[test]
fn policy_supports_safety_ratio_and_context_window() {
    let lines = out(r#"
pipeline main(harness: Harness, task) {
  const s = harness.agent.open()
  const snapshot = compaction.policy({
    session_id: s,
    context_window: 100000,
    safety_ratio: 0.5,
    strategy: "window",
  })
  harness.stdio.log(snapshot["token_threshold"])
  harness.stdio.log(snapshot["strategy"])
}
"#);
    assert_eq!(lines, vec!["50000", "window"], "{lines:?}");
}

#[test]
fn check_rejects_unknown_strategy_during_policy() {
    let err = run(r#"
pipeline main(harness: Harness, task) {
  compaction.policy({strategy: "not-a-strategy"})
}
"#)
    .expect_err("unknown strategy should error");
    assert!(
        err.contains("unknown compaction policy strategy"),
        "error mentions the unknown strategy: {err}"
    );
}

#[test]
fn run_without_session_id_uses_current_session() {
    // The agent loop pushes the current session id onto a thread-local
    // stack; `compaction.run()` (no session arg) resolves it. From a
    // free-standing pipeline there is no current session, so we expect
    // an error — verifying the contract.
    let err = run(r"
pipeline main(harness: Harness, task) {
  compaction.run()
}
")
    .expect_err("missing session id should error");
    assert!(
        err.contains("no `session_id` provided and no active agent session"),
        "error mentions missing session id: {err}"
    );
}
