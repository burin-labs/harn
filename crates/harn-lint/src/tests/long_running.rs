use super::*;

#[test]
fn long_running_flag_without_cleanup_warns() {
    let diags = lint_source(
        r#"
pipeline main() {
  let handle = walk_dir(".", {long_running: true})
  println(handle.handle_id)
}
"#,
    );

    assert!(
        has_rule(&diags, "long-running-without-cleanup"),
        "expected long-running cleanup warning, got: {diags:?}"
    );
}

#[test]
fn long_running_flag_with_defer_cleanup_is_ok() {
    let diags = lint_source(
        r#"
pipeline main() {
  let handle = walk_dir(".", {long_running: true})
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
fn host_tool_long_running_flag_without_cleanup_warns() {
    let diags = lint_source(
        r#"
pipeline main() {
  host_tool_call("run_command", {argv: ["sleep", "10"], long_running: true})
}
"#,
    );

    assert!(
        has_rule(&diags, "long-running-without-cleanup"),
        "expected host tool cleanup warning, got: {diags:?}"
    );
}
