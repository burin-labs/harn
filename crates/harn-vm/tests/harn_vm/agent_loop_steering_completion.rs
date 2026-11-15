#[test]
fn retarget_retires_prior_tool_failures_without_hiding_new_failures() {
    for fail_again in [false, true] {
        let session = super::fresh_session_id("retarget-tool-tally");
        let next_call = if fail_again {
            "llm_tool_call(\"blocked\", {}, \"after-retarget\"),"
        } else {
            ""
        };
        let source = format!(
            r#"
import {{ agent_session_push_user_message }} from "std/agent/state"
import {{ llm_text, llm_tool_call, with_llm_script }} from "std/testing"
pipeline default(harness: Harness, task: unknown) {{
  const session = "{session}"
  const tools = tool_define(tool_registry(), "blocked", "A refused operation.", {{
    handler: {{ _args -> throw "operation refused" }}, parameters: {{}}, returns: {{type: "string"}},
  }})
  with_llm_script(harness.llm, [
    llm_tool_call("blocked", {{}}, "before-retarget"),
    {next_call}
    llm_text("The proposed resource uses POST and GET. ##DONE##"),
  ], {{ ->
    const result = agent_loop(harness, "Create the resource.", nil, {{
      provider: "mock", session_id: session, root: "__HARN_TEST_SESSION_STORE_ROOT__",
      tools: tools, loop_until_done: true, max_iterations: 4,
      post_turn_callback: {{ info ->
        if info.iteration == 0 {{
          agent_session_push_user_message(harness.agent, session, {{
            content: "Explain the proposal without changing files.", mode: "steer",
            goal: {{objective: "Explain the proposal without changing files."}},
          }})
        }}
        return nil
      }},
    }})
    harness.stdio.log(result.terminal.kind)
    harness.stdio.log(result?.abandonment?.attempted_tool_calls ?? 0)
  }})
}}
"#
        );
        let output = super::run_with_bridge(&source).expect("steered agent run");
        let lines: Vec<_> = output.lines().collect();
        let expected = if fail_again {
            ["completion_unverified", "1"]
        } else {
            ["natural", "0"]
        };
        assert_eq!(lines, expected, "{output}");
    }
}
