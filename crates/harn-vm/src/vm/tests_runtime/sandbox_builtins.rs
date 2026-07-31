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
        r"pipeline t(harness: Harness, task) {
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
        r#"pipeline t(harness: Harness, task) { harness.stdio.log("hello") }"#,
        denied,
    );
    let (output, _) = result.unwrap();
    assert_eq!(output.trim(), "[harn] hello");
}

#[test]
fn test_sandbox_empty_denied_set() {
    // With an empty denied set, everything should work.
    let result = run_harn_with_denied(
        r#"pipeline t(harness: Harness, task) { harness.stdio.log("ok") }"#,
        HashSet::new(),
    );
    let (output, _) = result.unwrap();
    assert_eq!(output.trim(), "[harn] ok");
}

#[test]
fn test_sandbox_propagates_to_spawn() {
    // Denied builtins should propagate to spawned VMs.
    let denied: HashSet<String> = std::iter::once("push".to_string()).collect();
    let result = run_harn_with_denied(
        r"pipeline t(harness: Harness, task) {
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
        r"pipeline t(harness: Harness, task) {
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
