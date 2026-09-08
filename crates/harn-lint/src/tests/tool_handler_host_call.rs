//! A host wire reached from a tool handler is not a serviceable tool surface.

use super::*;

const RULE: &str = "tool-handler-host-call";

#[test]
fn direct_host_call_inside_tool_handler_is_reported() {
    let diagnostics = lint_source(
        "pub fn build(tools: any) -> any {\n\
         \x20 return tool_define(tools, \"inspect\", \"inspects\", {\n\
         \x20   handler: { args -> host_call(\"runtime.pipeline_input\", {}) },\n\
         \x20   parameters: {},\n\
         \x20 })\n\
         }\n",
    );
    assert!(
        has_rule(&diagnostics, RULE),
        "expected a tool-handler host-call diagnostic: {diagnostics:?}"
    );
}

#[test]
fn transitive_host_call_reached_from_tool_handler_is_reported() {
    let diagnostics = lint_source(
        "fn read_input() -> any { return host_call(\"runtime.pipeline_input\", {}) }\n\
         pub fn build(tools: any) -> any {\n\
         \x20 return tool_define(tools, \"inspect\", \"inspects\", {\n\
         \x20   handler: { args -> read_input() },\n\
         \x20   parameters: {},\n\
         \x20 })\n\
         }\n",
    );
    assert!(
        has_rule(&diagnostics, RULE),
        "a helper must not hide the privileged read from the check: {diagnostics:?}"
    );
}

#[test]
fn host_call_outside_tool_handler_is_not_reported() {
    let diagnostics = lint_source(
        "fn read_input() -> any { return host_call(\"runtime.pipeline_input\", {}) }\n\
         fn main() { read_input() }\n",
    );
    assert!(
        !has_rule(&diagnostics, RULE),
        "the host-selected entry boundary remains serviceable: {diagnostics:?}"
    );
}
