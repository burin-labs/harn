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
