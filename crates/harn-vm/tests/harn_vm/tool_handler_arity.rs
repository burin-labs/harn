//! A registered tool handler is checked against the registry's dispatch arity.
//!
//! The tool runtime invokes a registered handler with exactly one argument
//! (`vm.call_closure_pub(&handler, &[args_vm])` in `llm::agent_tools`). A
//! handler that declares a leading capability parameter therefore fails at
//! dispatch with an arity error.
//!
//! Nothing used to observe that mismatch. A *direct* call to such a function
//! is caught by HARN-TYP-006, but a handler is stored as a **value** in the
//! registry dict, so its arity was never compared against anything. Typing the
//! `handler` slot on `tool_define`'s config (`TOOL_DEFINE_CONFIG` in
//! `harn_builtin_meta::shapes`) closes the gap: the checker already infers a
//! `fn(...)` type for a bare function reference, so declaring the slot's
//! function type makes the comparison happen at the registration site.
//!
//! This is not hypothetical. Harn's own `std/agent/workers` registered three
//! `HarnessAgent`-prefixed handlers from the typed-capability cutover (#5814)
//! until this slot was typed.

use harn_lexer::Lexer;
use harn_parser::Parser;

/// Type-check `source` against the real builtin manifest and return the error
/// texts. The manifest install is what makes `tool_define`'s signature — and
/// therefore its typed `handler` slot — visible to the checker.
fn errors(source: &str) -> Vec<String> {
    harn_parser::install_builtin_manifest(harn_vm::stdlib::all_builtin_manifest());
    let tokens = Lexer::new(source).tokenize().expect("source should lex");
    let program = Parser::new(tokens).parse().expect("source should parse");
    harn_parser::TypeChecker::new()
        .check(&program)
        .into_iter()
        .filter(|d| d.severity == harn_parser::DiagnosticSeverity::Error)
        .map(|d| d.message)
        .collect()
}

const GOOD_HANDLER: &str = r#"fn good_tool(args: dict) -> string {
  return "ok"
}
"#;

const CAPABILITY_HANDLER: &str = r#"fn migrated_tool(harness: Harness, args: dict) -> string {
  return "bad"
}
"#;

fn register(handler_expr: &str) -> String {
    format!(
        r#"{GOOD_HANDLER}{CAPABILITY_HANDLER}fn main(harness: Harness) {{
  let tools = tool_registry()
  tools = tool_define(tools, "t", "A tool.", {{
    handler: {handler_expr},
    parameters: {{v: {{type: "string", description: "v"}}}},
    annotations: {{kind: "read"}},
  }})
  harness.stdio.println(to_string(tools))
}}
"#
    )
}

/// Guards the rest of this file against vacuity: every other test here asserts
/// the *absence* of a handler error, which would pass trivially if the builtin
/// manifest were not installed and `tool_define` were unknown.
#[test]
fn a_capability_prefixed_handler_is_rejected_at_the_registration_site() {
    let errs = errors(&register("migrated_tool"));
    assert!(
        errs.iter()
            .any(|e| e.contains("fn(Harness, dict) -> string")),
        "the registration should report the handler's own function type; got: {errs:?}"
    );
}

#[test]
fn a_single_argument_handler_is_accepted() {
    let errs = errors(&register("good_tool"));
    assert!(
        !errs.iter().any(|e| e.contains("handler")),
        "a correctly-shaped handler must not be flagged; got: {errs:?}"
    );
}

#[test]
fn a_closure_that_supplies_the_capability_is_accepted() {
    // The repair for a handler that genuinely needs a capability: close over
    // it at the registration site and keep the registered arity at one. This
    // is what `std/agent/workers` now does for the subagent lifecycle tools.
    let errs = errors(&register("{ args -> migrated_tool(harness, args) }"));
    assert!(
        !errs.iter().any(|e| e.contains("handler")),
        "a capability-closing wrapper must be accepted; got: {errs:?}"
    );
}

#[test]
fn unrelated_config_keys_stay_open() {
    // `TOOL_DEFINE_CONFIG` carries a row tail. A closed shape would reject
    // every key the signature does not name, which is why the slot could not
    // simply be enumerated.
    let source = format!(
        r#"{GOOD_HANDLER}fn main(harness: Harness) {{
  let tools = tool_registry()
  tools = tool_define(tools, "t", "A tool.", {{
    handler: good_tool,
    parameters: {{}},
    returns: {{type: "object"}},
    executor: "harn",
    defer_loading: false,
    annotations: {{kind: "read"}},
    guidance: "Use sparingly.",
  }})
  harness.stdio.println(to_string(tools))
}}
"#
    );
    let errs = errors(&source);
    assert!(
        !errs.iter().any(|e| e.contains("config")),
        "documented config keys must remain accepted; got: {errs:?}"
    );
}
