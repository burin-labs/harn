use super::*;

#[test]
fn background_flag_without_cleanup_warns() {
    let diags = lint_source(
        r#"
pipeline main() {
  const handle = walk_dir(".", {background: true})
  __io_println(handle.handle_id)
}
"#,
    );

    assert!(
        has_rule(&diags, "long-running-without-cleanup"),
        "expected long-running cleanup warning, got: {diags:?}"
    );
}

#[test]
fn background_flag_with_defer_cleanup_is_ok() {
    let diags = lint_source(
        r#"
pipeline main() {
  const handle = walk_dir(".", {background: true})
  defer {
    host_tool_call("cancel_handle", {handle_id: handle.handle_id})
  }
}
"#,
    );

    assert!(
        !has_rule(&diags, "long-running-without-cleanup"),
        "did not expect long-running cleanup warning, got: {diags:?}"
    );
}

#[test]
fn host_tool_background_flag_without_cleanup_warns() {
    let diags = lint_source(
        r#"
pipeline main() {
  host_tool_call("run_command", {argv: ["sleep", "10"], background: true})
}
"#,
    );

    assert!(
        has_rule(&diags, "long-running-without-cleanup"),
        "expected host tool cleanup warning, got: {diags:?}"
    );
}

#[test]
fn harness_background_flag_without_cleanup_warns() {
    let diags = lint_source(
        r#"
fn main(harness: Harness) {
  harness.tools.run_command({argv: ["sleep", "10"], background: true})
}
"#,
    );

    assert!(
        has_rule(&diags, "long-running-without-cleanup"),
        "expected canonical harness call to require cleanup, got: {diags:?}"
    );
}

#[test]
fn colliding_non_harness_background_method_is_not_flagged() {
    let diags = lint_source(
        r"
fn main(harness: Harness) {
  const client = {}
  client.run_command({background: true})
}
",
    );

    assert!(
        !has_rule(&diags, "long-running-without-cleanup"),
        "unrelated method must not be treated as a harness capability: {diags:?}"
    );
}

#[test]
fn migrated_cleanup_covers_legacy_background_call() {
    let diags = lint_source(
        r#"
fn main(harness: Harness) {
  const handle = run_command("sleep 10", {background: true})
  defer {
    harness.tools.cancel_handle(handle)
  }
}
"#,
    );

    assert!(
        !has_rule(&diags, "long-running-without-cleanup"),
        "canonical cleanup must cover a legacy trigger, got: {diags:?}"
    );
}

#[test]
fn legacy_cleanup_covers_migrated_background_call() {
    let diags = lint_source(
        r#"
fn main(harness: Harness) {
  const handle = harness.tools.run_command({argv: ["sleep", "10"], background: true})
  defer {
    cancel_handle(handle)
  }
}
"#,
    );

    assert!(
        !has_rule(&diags, "long-running-without-cleanup"),
        "legacy cleanup must cover a canonical trigger, got: {diags:?}"
    );
}

#[test]
fn migrated_cleanup_covers_migrated_background_call() {
    let diags = lint_source(
        r#"
fn main(harness: Harness) {
  const handle = harness.tools.run_command({argv: ["sleep", "10"], background: true})
  defer {
    harness.tools.cancel_handle(handle)
  }
}
"#,
    );

    assert!(
        !has_rule(&diags, "long-running-without-cleanup"),
        "fully canonical trigger and cleanup must pair, got: {diags:?}"
    );
}

#[test]
fn colliding_non_harness_cancel_method_does_not_count_as_cleanup() {
    let diags = lint_source(
        r#"
fn main(harness: Harness) {
  const client = {}
  const handle = run_command("sleep 10", {background: true})
  defer {
    client.cancel_handle(handle)
  }
}
"#,
    );

    assert!(
        has_rule(&diags, "long-running-without-cleanup"),
        "unrelated cancel method must not satisfy cleanup, got: {diags:?}"
    );
}
