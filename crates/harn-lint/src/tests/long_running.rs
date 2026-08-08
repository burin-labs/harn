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
