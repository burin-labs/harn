#![recursion_limit = "256"]
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
pipeline main(task) {
  const s = agent_session_open()
  compaction.policy({session_id: s, max_tokens: 10000, max_turns: 50})
  const decision = compaction.check(s)
  log(decision["action"])
  log(decision["message_count"])
  log(decision["trigger"])
  log(decision["policy_inherited"])
}
"#);
    assert_eq!(lines, vec!["defer", "0", "manual", "false"], "{lines:?}");
}

#[test]
fn check_marks_compact_now_when_tokens_cross() {
    let lines = out(r#"
pipeline main(task) {
  const s = agent_session_open()
  compaction.policy({session_id: s, max_tokens: 1, max_turns: 100})
  agent_session_inject(s, {role: "user", content: "Investigate the parser regression in stdlib once more."})
  agent_session_inject(s, {role: "assistant", content: "The root cause is in diagnostic recovery; we missed an EOF guard."})
  const decision = compaction.check(s)
  log(decision["action"])
  log(decision["trigger"])
  log(decision["strategy"])
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
pipeline main(task) {
  // No session_id → registers the default policy.
  compaction.policy({max_turns: 2})
  const s = agent_session_open()
  agent_session_inject(s, {role: "user", content: "one"})
  agent_session_inject(s, {role: "assistant", content: "two"})
  agent_session_inject(s, {role: "user", content: "three"})
  const decision = compaction.check(s)
  log(decision["action"])
  log(decision["trigger"])
  log(decision["policy_inherited"])
}
"#);
    assert_eq!(lines, vec!["compact_now", "turns", "true"], "{lines:?}");
}

#[test]
fn run_compacts_session_via_lifecycle_with_custom_strategy() {
    let lines = out(r#"
pipeline main(task) {
  const s = agent_session_open()
  compaction.policy({session_id: s, max_tokens: 1, keep_last: 1})
  agent_session_inject(s, {role: "user", content: "Investigate the parser regression."})
  agent_session_inject(s, {role: "assistant", content: "The root cause is in diagnostic recovery."})
  agent_session_inject(s, {role: "user", content: "Capture the next command."})

  const outcome = compaction.run(s, {
    strategy: "custom",
    summarize_fn: { archived, _reminders -> {summary: "policy compacted " + to_string(len(archived))} },
  })
  log(outcome["compacted"])
  log(outcome["archived_messages"])
  log(outcome["engine_strategy"])
  log(outcome["strategy"])
  log(transcript_summary(outcome["transcript"])?.contains("policy compacted"))
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
pipeline main(task) {
  const s = agent_session_open()
  const snapshot = compaction.policy({
    session_id: s,
    context_window: 100000,
    safety_ratio: 0.5,
    strategy: "window",
  })
  log(snapshot["token_threshold"])
  log(snapshot["strategy"])
}
"#);
    assert_eq!(lines, vec!["50000", "window"], "{lines:?}");
}

#[test]
fn check_rejects_unknown_strategy_during_policy() {
    let err = run(r#"
pipeline main(task) {
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
pipeline main(task) {
  compaction.run()
}
")
    .expect_err("missing session id should error");
    assert!(
        err.contains("no `session_id` provided and no active agent session"),
        "error mentions missing session id: {err}"
    );
}
