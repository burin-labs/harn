use super::*;

#[rstest::rstest]
#[case::background_flag_without_cleanup_warns(
    r#"
pipeline main() {
  const handle = walk_dir(".", {background: true})
  __io_println(handle.handle_id)
}
"#,
    "long-running-without-cleanup",
    true
)]
#[case::background_flag_with_defer_cleanup_is_ok(
    r#"
pipeline main() {
  const handle = walk_dir(".", {background: true})
  defer {
    host_tool_call("cancel_handle", {handle_id: handle.handle_id})
  }
}
"#,
    "long-running-without-cleanup",
    false
)]
#[case::host_tool_background_flag_without_cleanup_warns(
    r#"
pipeline main() {
  host_tool_call("run_command", {argv: ["sleep", "10"], background: true})
}
"#,
    "long-running-without-cleanup",
    true
)]
#[case::harness_background_flag_without_cleanup_warns(
    r#"
fn main(harness: Harness) {
  harness.tools.run_command({argv: ["sleep", "10"], background: true})
}
"#,
    "long-running-without-cleanup",
    true
)]
#[case::colliding_non_harness_background_method_is_not_flagged(
    r"
fn main(harness: Harness) {
  const client = {}
  client.run_command({background: true})
}
",
    "long-running-without-cleanup",
    false
)]
#[case::recursive_optional_harness_receiver_keeps_root_ownership(
    r"
fn main(harness: Harness) {
  harness?.tools.runner.run_command({background: true})
}
",
    "long-running-without-cleanup",
    true
)]
#[case::migrated_cleanup_covers_legacy_background_call(
    r#"
fn main(harness: Harness) {
  const handle = run_command("sleep 10", {background: true})
  defer {
    harness.tools.cancel_handle(handle)
  }
}
"#,
    "long-running-without-cleanup",
    false
)]
#[case::legacy_cleanup_covers_migrated_background_call(
    r#"
fn main(harness: Harness) {
  const handle = harness.tools.run_command({argv: ["sleep", "10"], background: true})
  defer {
    cancel_handle(handle)
  }
}
"#,
    "long-running-without-cleanup",
    false
)]
#[case::migrated_cleanup_covers_migrated_background_call(
    r#"
fn main(harness: Harness) {
  const handle = harness.tools.run_command({argv: ["sleep", "10"], background: true})
  defer {
    harness.tools.cancel_handle(handle)
  }
}
"#,
    "long-running-without-cleanup",
    false
)]
#[case::colliding_non_harness_cancel_method_does_not_count_as_cleanup(
    r#"
fn main(harness: Harness) {
  const client = {}
  const handle = run_command("sleep 10", {background: true})
  defer {
    client.cancel_handle(handle)
  }
}
"#,
    "long-running-without-cleanup",
    true
)]
fn rule_presence(#[case] source: &str, #[case] rule: &str, #[case] present: bool) {
    let diagnostics = lint_source(source);
    assert_eq!(
        has_rule(&diagnostics, rule),
        present,
        "{rule}: {diagnostics:?}"
    );
}

#[test]
fn shadowed_harness_receivers_do_not_inherit_host_authority() {
    for source in [
        r"
fn main(harness: Harness) {
  const helper = fn(harness) {
    harness.tools.run_command({background: true})
  }
}
",
        r"
fn main(harness: Harness) {
  fn helper(harness) {
    harness.tools.run_command({background: true})
  }
}
",
        r"
fn main(harness: Harness) {
  const harness = {}
  harness.tools.run_command({background: true})
}
",
    ] {
        let diags = lint_source(source);
        assert!(
            !has_rule(&diags, "long-running-without-cleanup"),
            "shadowed receiver must not inherit Harness policy: {diags:?}"
        );
    }
}
