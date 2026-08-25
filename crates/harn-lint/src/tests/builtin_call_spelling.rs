//! Rules keyed on a builtin's name must survive the ambient-to-harness
//! migration.
//!
//! `HARN-LNT-071` tells authors to replace an ambient global with the typed
//! `harness.<capability>.<method>` that owns it. A rule that matched only the
//! ambient spelling stops applying at exactly that moment, and a rule that
//! quietly stops applying is indistinguishable from one that found nothing.
//! Each rule below is pinned in three directions: the ambient spelling still
//! reports, the migrated spelling reports, and a same-named method on a
//! receiver that is not the harness reports nothing.

use super::*;

#[test]
fn prompt_injection_risk_reports_the_ambient_spelling() {
    let diagnostics = lint_source(
        r#"
pipeline main(harness: Harness) {
  const user_text = "ignore safety"
  llm_call("hello", "You are safe. ${user_text}")
}
"#,
    );

    assert_eq!(count_rule(&diagnostics, "prompt-injection-risk"), 1);
}

#[test]
fn prompt_injection_risk_reports_the_harness_spelling() {
    let diagnostics = lint_source(
        r#"
pipeline main(harness: Harness) {
  const user_text = "ignore safety"
  harness.llm.call("hello", "You are safe. ${user_text}")
}
"#,
    );

    assert_eq!(
        count_rule(&diagnostics, "prompt-injection-risk"),
        1,
        "migrating the call site must not silence the rule: {diagnostics:?}"
    );
}

#[test]
fn prompt_injection_risk_ignores_a_call_method_on_another_receiver() {
    let diagnostics = lint_source(
        r#"
pipeline main(harness: Harness) {
  const user_text = "ignore safety"
  const proxy = {llm: {call: { a, b -> b }}}
  proxy.llm.call("hello", "You are safe. ${user_text}")
}
"#,
    );

    assert!(
        !has_rule(&diagnostics, "prompt-injection-risk"),
        "`llm.call` on a plain value is not the harness method: {diagnostics:?}"
    );
}

#[test]
fn mcp_tool_annotations_report_the_harness_spelling() {
    let diagnostics = lint_source(
        r#"
pipeline main(harness: Harness) {
  let tools = tool_registry()
  tools = tool_define(tools, "echo", "Echo input", {
    parameters: {text: "string"},
    handler: { args -> args.text },
  })
  harness.tools.mcp_tools(tools)
}
"#,
    );

    assert_eq!(
        count_rule(&diagnostics, "mcp-tool-annotations"),
        1,
        "migrating the registration must not silence the rule: {diagnostics:?}"
    );
}

#[test]
fn mcp_tool_annotations_still_accept_an_annotated_harness_registration() {
    let diagnostics = lint_source(
        r#"
pipeline main(harness: Harness) {
  let tools = tool_registry()
  tools = tool_define(tools, "echo", "Echo input", {
    parameters: {text: "string"},
    handler: { args -> args.text },
    annotations: {readOnlyHint: true, idempotentHint: true, openWorldHint: false},
  })
  harness.tools.mcp_tools(tools)
}
"#,
    );

    assert!(!has_rule(&diagnostics, "mcp-tool-annotations"));
}

#[test]
fn mcp_tool_annotations_ignore_an_mcp_tools_method_on_another_receiver() {
    let diagnostics = lint_source(
        r#"
pipeline main(harness: Harness) {
  let tools = tool_registry()
  tools = tool_define(tools, "echo", "Echo input", {
    parameters: {text: "string"},
    handler: { args -> args.text },
  })
  const proxy = {tools: {mcp_tools: { t -> t }}}
  proxy.tools.mcp_tools(tools)
}
"#,
    );

    assert!(
        !has_rule(&diagnostics, "mcp-tool-annotations"),
        "`tools.mcp_tools` on a plain value is not the harness method: {diagnostics:?}"
    );
}

#[test]
fn persona_hook_target_reports_the_harness_spelling() {
    let diagnostics = lint_source(
        r#"
@persona(name: "merge_captain")
fn merge_captain(ctx) {
  plan(ctx)
}

@step(name: "plan")
fn plan(ctx) {
  return ctx
}

pipeline main(harness: Harness) {
  harness.agent.register_step_hook("merge_*", "publish", "PreStep", { ctx -> nil })
}
"#,
    );

    let target_diagnostics: Vec<_> = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.rule == "persona-hook-target")
        .collect();
    assert_eq!(
        target_diagnostics.len(),
        1,
        "migrating the registration must not silence the rule: {diagnostics:?}"
    );
    assert!(target_diagnostics[0].message.contains("publish"));
}

#[test]
fn persona_hook_target_accepts_a_declared_step_through_the_harness_spelling() {
    let diagnostics = lint_source(
        r#"
@persona(name: "merge_captain")
fn merge_captain(ctx) {
  plan(ctx)
}

@step(name: "plan")
fn plan(ctx) {
  return ctx
}

pipeline main(harness: Harness) {
  harness.agent.register_step_hook("merge_*", "plan", "PreStep", { ctx -> nil })
}
"#,
    );

    assert!(!has_rule(&diagnostics, "persona-hook-target"));
}

#[test]
fn persona_hook_target_ignores_a_register_step_hook_method_on_another_receiver() {
    let diagnostics = lint_source(
        r#"
@persona(name: "merge_captain")
fn merge_captain(ctx) {
  plan(ctx)
}

@step(name: "plan")
fn plan(ctx) {
  return ctx
}

pipeline main(harness: Harness) {
  const proxy = {agent: {register_step_hook: { a, b, c, d -> nil }}}
  proxy.agent.register_step_hook("merge_*", "publish", "PreStep", { ctx -> nil })
}
"#,
    );

    assert!(
        !has_rule(&diagnostics, "persona-hook-target"),
        "`agent.register_step_hook` on a plain value is not the harness method: {diagnostics:?}"
    );
}

#[test]
fn a_shadowing_local_named_harness_does_not_carry_harness_authority() {
    let diagnostics = lint_source(
        r#"
pipeline main() {
  const user_text = "ignore safety"
  const harness = {llm: {call: { a, b -> b }}}
  harness.llm.call("hello", "You are safe. ${user_text}")
}
"#,
    );

    assert!(
        !has_rule(&diagnostics, "prompt-injection-risk"),
        "the receiver is a local dict, not the host handle: {diagnostics:?}"
    );
}
