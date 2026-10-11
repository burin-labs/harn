use super::*;

#[tokio::test]
async fn manifest_hooks_initialize_on_dispatch_and_keep_case_state_isolated() {
    let _guard = crate::tests::common::harn_state_lock::lock_harn_state_async().await;
    let temp = TempTestDir::new();
    temp.write(
        "harn.toml",
        r#"
[package]
name = "test-hooks"
[exports]
handlers = "hooks.harn"
[[hooks]]
event = "PostToolUse"
handler = "handlers::handle"
"#,
    );
    let dispatch = r#"
import { agent_dispatch_tool_call } from "std/agent/primitives"

@test(cases: [{name: "first", args: []}, {name: "second", args: []}])
pipeline test_dispatch(harness: Harness) {
  const tools = tool_define(tool_registry(), "read_note", "read a note", {
    parameters: {}, handler: { _args -> "original" },
  })
  for call in [1, 2] {
    const result = agent_dispatch_tool_call(
      harness.tools, {id: "read-" + to_string(call), name: "read_note", arguments: {}}, tools,
    )
    assert_eq(result?.rendered_result, "HOOKED" + to_string(call) + ":original")
  }
}
"#;
    for (name, hook, source, passes, error) in [
        (
            "unused",
            "let initialized = 1 / 0\npub fn handle(ctx) { return ctx }\n",
            "pipeline test_unused() { assert_eq(2 + 2, 4) }\n",
            1,
            None,
        ),
        (
            "dispatch",
            r#"
let calls = 0
pub fn handle(_harness: Harness, ctx: dict) {
  calls = calls + 1
  return "HOOKED" + to_string(calls) + ":" + ctx.result.text
}
"#,
            dispatch,
            2,
            None,
        ),
        (
            "broken",
            "pub fn other(ctx) { return ctx }\n",
            dispatch,
            0,
            Some("not exported"),
        ),
    ] {
        temp.write("hooks.harn", hook);
        temp.write("test_hooks.harn", source);
        let summary = run_tests_with_session(
            &temp.path().join("test_hooks.harn"),
            &RunOptions::new(30_000),
            &TestRunSession::default(),
        )
        .await;
        assert_eq!(
            summary.total,
            if name == "unused" { 1 } else { 2 },
            "{name}"
        );
        assert_eq!(summary.passed, passes, "{name}: {:?}", summary.results);
        assert_eq!(summary.skipped, 0, "{name}");
        if let Some(error) = error {
            assert_eq!(summary.failed, 2, "{name}");
            assert!(
                summary.results.iter().all(|result| result
                    .error
                    .as_deref()
                    .is_some_and(|message| message.contains(error))),
                "{:?}",
                summary.results
            );
        } else {
            assert_eq!(summary.failed, 0, "{name}");
        }
        if name == "unused" {
            assert_eq!(summary.aggregate.modules.modules_loaded, 0);
        }
    }
}
