//! Denying individual builtins, and keeping that denial on child work.
//!
//! A denied builtin fails while its allowed neighbours still run, an empty deny
//! set is a no-op, and the deny set propagates into `spawn` and `parallel`.

use std::collections::HashSet;

use super::harness::*;
#[test]
fn test_sandbox_deny_builtin() {
    let denied: HashSet<String> = std::iter::once("push".to_string()).collect();
    let result = run_harn_with_denied(
        r"pipeline t(harness: Harness, task: unknown) {
const xs = [1, 2]
push(xs, 3)
}",
        denied,
    );
    let err = result.unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("not permitted"),
        "expected not permitted, got: {msg}"
    );
    assert!(
        msg.contains("push"),
        "expected builtin name in error, got: {msg}"
    );
}

#[test]
fn test_sandbox_allowed_builtin_works() {
    // Denying "push" should not block "log"
    let denied: HashSet<String> = std::iter::once("push".to_string()).collect();
    let result = run_harn_with_denied(
        r#"pipeline t(harness: Harness, task: unknown) { harness.stdio.log("hello") }"#,
        denied,
    );
    let (output, _) = result.unwrap();
    assert_eq!(output.trim(), "[harn] hello");
}

#[test]
fn test_sandbox_empty_denied_set() {
    // With an empty denied set, everything should work.
    let result = run_harn_with_denied(
        r#"pipeline t(harness: Harness, task: unknown) { harness.stdio.log("ok") }"#,
        HashSet::new(),
    );
    let (output, _) = result.unwrap();
    assert_eq!(output.trim(), "[harn] ok");
}

#[test]
fn test_sandbox_deny_alias_blocks_typed_harness_method() {
    let denied: HashSet<String> = std::iter::once("llm_call".to_string()).collect();
    let result = run_harn_with_denied(
        r#"pipeline main(harness: Harness) {
const response = harness.llm.call("policy probe", nil, {
  provider: "mock",
  model: "mock",
})
harness.stdio.println(response)
}"#,
        denied,
    );
    let error = result.expect_err("the public alias must deny its typed Harness method");
    let message = error.to_string();
    assert!(
        message.contains("llm_call") && message.contains("not permitted"),
        "expected an llm_call permission error, got: {message}"
    );
}

#[test]
fn test_sandbox_propagates_to_spawn() {
    // Denied builtins should propagate to spawned VMs.
    let denied: HashSet<String> = std::iter::once("push".to_string()).collect();
    let result = run_harn_with_denied(
        r"pipeline t(harness: Harness, task: unknown) {
const handle = spawn {
  const xs = [1, 2]
  push(xs, 3)
}
await(handle)
}",
        denied,
    );
    let err = result.unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("not permitted"),
        "expected not permitted in spawned VM, got: {msg}"
    );
}

#[test]
fn test_sandbox_propagates_to_parallel() {
    // Denied builtins should propagate to parallel VMs.
    let denied: HashSet<String> = std::iter::once("push".to_string()).collect();
    let result = run_harn_with_denied(
        r"pipeline t(harness: Harness, task: unknown) {
const results = parallel(2) { i ->
  const xs = [1, 2]
  push(xs, 3)
}
}",
        denied,
    );
    let err = result.unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("not permitted"),
        "expected not permitted in parallel VM, got: {msg}"
    );
}
