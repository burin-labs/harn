//! Typed tool-handler results (harn#7901).

use super::*;

const RULE: &str = "untyped-tool-handler-result";

/// The falsifier. A handler returning a freeform dict must be reported, or the
/// typed cases below would also pass against a rule that had simply stopped
/// firing.
#[test]
fn a_handler_returning_a_freeform_dict_is_reported() {
    let diagnostics = lint_source(
        "pub fn build(tools: any) -> any {\n\
         \x20 return tool_define(tools, \"apply\", \"applies\", {\n\
         \x20   handler: { args -> {ok: false, error: \"blocked\"} },\n\
         \x20   parameters: {},\n\
         \x20 })\n\
         }\n",
    );
    assert!(
        has_rule(&diagnostics, RULE),
        "expected an untyped handler-result diagnostic: {diagnostics:?}"
    );
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.rule == RULE && diagnostic.severity == LintSeverity::Error
    }));
}

/// The same shape returned by an explicit `return` inside a block body, which
/// is how most real handlers are written.
#[test]
fn an_explicit_return_of_a_dict_is_reported() {
    let diagnostics = lint_source(
        "pub fn build(tools: any) -> any {\n\
         \x20 return tool_define(tools, \"apply\", \"applies\", {\n\
         \x20   handler: fn(args: dict) {\n\
         \x20     if args.dry_run { return {ok: true} }\n\
         \x20     return {ok: false}\n\
         \x20   },\n\
         \x20   parameters: {},\n\
         \x20 })\n\
         }\n",
    );
    let hits = diagnostics.iter().filter(|d| d.rule == RULE).count();
    assert_eq!(hits, 2, "both returns should be reported: {diagnostics:?}");
}

#[test]
fn an_immutable_local_dict_return_is_reported() {
    let diagnostics = lint_source(
        "pub fn build(tools: any) -> any {\n\
         \x20 return tool_define(tools, \"apply\", \"applies\", {\n\
         \x20   handler: { args ->\n\
         \x20     const result = {failed: true, error_code: 7}\n\
         \x20     return result\n\
         \x20   },\n\
         \x20   parameters: {},\n\
         \x20 })\n\
         }\n",
    );
    assert!(
        has_rule(&diagnostics, RULE),
        "a named immutable dict is still an untyped outcome: {diagnostics:?}"
    );
}

#[test]
fn a_mutable_local_is_not_guessed_from_its_initializer() {
    let diagnostics = lint_source(
        "pub fn build(tools: any) -> any {\n\
         \x20 return tool_define(tools, \"apply\", \"applies\", {\n\
         \x20   handler: { args ->\n\
         \x20     let result = {ok: false}\n\
         \x20     result = ApplyOutcome{ok: true}\n\
         \x20     return result\n\
         \x20   },\n\
         \x20   parameters: {},\n\
         \x20 })\n\
         }\n",
    );
    assert!(
        !has_rule(&diagnostics, RULE),
        "a mutable binding needs type analysis before classification: {diagnostics:?}"
    );
}

/// A typed struct declares the outcome by its type, so there is nothing to
/// infer and nothing to report.
#[test]
fn a_handler_returning_a_typed_struct_is_not_reported() {
    let diagnostics = lint_source(
        "struct ApplyOutcome { ok: bool }\n\
         pub fn build(tools: any) -> any {\n\
         \x20 return tool_define(tools, \"apply\", \"applies\", {\n\
         \x20   handler: { args -> ApplyOutcome{ok: false} },\n\
         \x20   parameters: {},\n\
         \x20 })\n\
         }\n",
    );
    assert!(
        !has_rule(&diagnostics, RULE),
        "a typed struct return must not be reported: {diagnostics:?}"
    );
}

/// The rule is scoped to handlers. An ordinary function returning a dict is a
/// different question and is not this rule's business.
#[test]
fn an_ordinary_function_returning_a_dict_is_not_reported() {
    let diagnostics = lint_source(
        "pub fn summarize(args: dict) -> dict {\n\
         \x20 return {ok: true, count: 2}\n\
         }\n",
    );
    assert!(
        !has_rule(&diagnostics, RULE),
        "only tool handlers are in scope: {diagnostics:?}"
    );
}

#[test]
fn a_tool_search_strategy_handler_is_not_a_tool_result() {
    let diagnostics = lint_source(
        "pub fn search() -> any {\n\
         \x20 return with_llm_script(nil, [{\n\
         \x20   tool_search: {strategy: {handler: { query, _candidates, _state ->\n\
         \x20     {tool_names: [query], ranked: []}\n\
         \x20   }}}\n\
         \x20 }])\n\
         }\n",
    );
    assert!(
        !has_rule(&diagnostics, RULE),
        "a search strategy does not return a tool outcome: {diagnostics:?}"
    );
}

#[test]
fn a_tool_configuration_held_in_a_binding_is_still_checked() {
    let diagnostics = lint_source(
        "pub fn build(tools: any) -> any {\n\
         \x20 const config = {handler: { args -> {ok: false} }, parameters: {}}\n\
         \x20 return tool_define(tools, \"apply\", \"applies\", config)\n\
         }\n",
    );
    assert!(
        has_rule(&diagnostics, RULE),
        "indirect registration must not hide a freeform result: {diagnostics:?}"
    );
}

/// The envelope the rule's own suggestion recommends for a text result. It is
/// a dict literal, so a rule keyed only on "is a dict" would warn about the
/// shape it just asked for. Paired with the freeform-dict falsifier at the top
/// of this file, which must keep firing for this exemption to mean anything.
#[test]
fn a_handler_returning_the_typed_result_envelope_is_not_reported() {
    let diagnostics = lint_source(
        "pub fn build(tools: any) -> any {\n\
         \x20 return tool_define(tools, \"search\", \"searches\", {\n\
         \x20   handler: { args ->\n\
         \x20     return {\n\
         \x20       schema: \"harn.agent_tool_handler_result.v1\",\n\
         \x20       text: \"3 matches\",\n\
         \x20       data: {matches: 3},\n\
         \x20     }\n\
         \x20   },\n\
         \x20   parameters: {},\n\
         \x20 })\n\
         }\n",
    );
    assert_eq!(
        diagnostics.iter().filter(|d| d.rule == RULE).count(),
        0,
        "the typed result envelope must not be reported: {diagnostics:?}"
    );
}

/// The exemption is the exact schema string, not the presence of a `schema`
/// key. A dict that names some other schema declares no outcome, so the rule
/// still has something true to say about it.
#[test]
fn a_dict_naming_a_different_schema_is_still_reported() {
    let diagnostics = lint_source(
        "pub fn build(tools: any) -> any {\n\
         \x20 return tool_define(tools, \"search\", \"searches\", {\n\
         \x20   handler: { args -> {schema: \"something.else.v1\", ok: false} },\n\
         \x20   parameters: {},\n\
         \x20 })\n\
         }\n",
    );
    assert!(
        has_rule(&diagnostics, RULE),
        "only the handler-result envelope is exempt: {diagnostics:?}"
    );
}

/// The lint and the runtime must match on one string. If the runtime's owner
/// changes, this fails rather than letting the rule quietly warn about the
/// envelope again.
#[test]
fn the_exempt_schema_is_the_runtime_owner_string() {
    assert_eq!(
        harn_vm::llm::AGENT_TOOL_HANDLER_RESULT_SCHEMA,
        "harn.agent_tool_handler_result.v1"
    );
}
