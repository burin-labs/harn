mod comments;
mod roundtrip;
mod scoped_blocks;

use harn_lexer::Lexer;
use harn_parser::Parser;

use crate::helpers::format_duration;
use crate::{format_source, format_source_opts, FmtOptions, AUTO_SEPARATOR_WIDTH};

pub(super) fn assert_roundtrip(source: &str) {
    let formatted = format_source(source).unwrap();
    let mut lexer = Lexer::new(&formatted);
    let tokens = lexer
        .tokenize()
        .unwrap_or_else(|e| panic!("Formatted output failed to lex:\n{formatted}\nError: {e}"));
    let mut parser = Parser::new(tokens);
    parser
        .parse()
        .unwrap_or_else(|e| panic!("Formatted output failed to parse:\n{formatted}\nError: {e}"));
    let formatted2 = format_source(&formatted).unwrap();
    assert_eq!(formatted, formatted2, "Formatter is not idempotent");
}

#[test]
fn test_format_hello() {
    let source = r#"pipeline default(task) {
  log("Hello, Harn!")
}"#;
    let result = format_source(source).unwrap();
    assert_eq!(
        result,
        "pipeline default(task) {\n  log(\"Hello, Harn!\")\n}\n"
    );
}

#[test]
fn test_format_const_let() {
    let source = r#"pipeline default(task) {
  const x = 42
  let y = "hello"
}"#;
    let result = format_source(source).unwrap();
    assert!(result.contains("const x = 42"));
    assert!(result.contains("let y = \"hello\""));
}

#[test]
fn test_format_binary_ops() {
    let source = r"pipeline default(task) {
  let x = 1 + 2
  let y = a * b
}";
    let result = format_source(source).unwrap();
    assert!(result.contains("1 + 2"));
    assert!(result.contains("a * b"));
}

#[test]
fn test_format_ternary_list_branch() {
    let source = r#"pipeline default() {
  let args = repo ? ["--repo", repo] : []
  let first = xs?.[0]
  let legacy_first = xs?[0]
}"#;
    let result = format_source(source).unwrap();
    assert!(result.contains(r#"repo ? ["--repo", repo] : []"#));
    assert!(result.contains("xs?.[0]"));
    assert!(result.contains("legacy_first = xs?.[0]"));
    assert_roundtrip(source);
}

#[test]
fn test_format_pipe_placeholder_patterns() {
    let source = r#"pipeline default(task) {
  let words = "hello world" |> split(_, " ")
  let result = [3, 1, 2]
    |> _.sort()
    |> len(_)
}"#;
    let result = format_source(source).unwrap();
    assert!(result.contains(r#""hello world" |> split(_, " ")"#));
    assert!(result.contains("let result = [3, 1, 2]\n    |> _.sort()\n    |> len(_)"));
    assert_roundtrip(source);
}

#[test]
fn test_format_duration() {
    assert_eq!(format_duration(5000), "5s");
    assert_eq!(format_duration(60000), "1m");
    assert_eq!(format_duration(3600000), "1h");
    assert_eq!(format_duration(86_400_000), "1d");
    assert_eq!(format_duration(604_800_000), "1w");
    assert_eq!(format_duration(500), "500ms");
}

#[test]
fn test_format_if_else() {
    let source = r#"pipeline default(task) {
  if x > 0 {
    log("positive")
  } else {
    log("non-positive")
  }
}"#;
    let result = format_source(source).unwrap();
    assert!(result.contains("if x > 0 {"));
    assert!(result.contains("} else {"));
}

#[test]
fn test_format_for_in() {
    let source = r"pipeline default(task) {
  for i in [1, 2, 3] {
    log(i)
  }
}";
    let result = format_source(source).unwrap();
    assert!(result.contains("for i in [1, 2, 3] {"));
}

#[test]
fn test_format_fn() {
    let source = r"pipeline default(task) {
  fn add(a, b) {
    return a + b
  }
}";
    let result = format_source(source).unwrap();
    assert!(result.contains("fn add(a, b) {"));
    assert!(result.contains("return a + b"));
}

#[test]
fn test_format_semicolon_separated_statements_to_newlines() {
    let source = r"pipeline default(task) { let x = 1; let y = 2; return; }";
    let result = format_source(source).unwrap();
    assert_eq!(
        result,
        "pipeline default(task) {\n  let x = 1\n  let y = 2\n  return\n}\n"
    );
}

#[test]
fn test_format_semicolon_separated_skill_fields_to_newlines() {
    let source = r#"skill deploy { description "Ship it"; prompt "Do the thing"; model "x" }"#;
    let result = format_source(source).unwrap();
    assert_eq!(
        result,
        "skill deploy {\n  description \"Ship it\"\n  prompt \"Do the thing\"\n  model \"x\"\n}\n"
    );
}

#[test]
fn test_format_eval_pack_fields_and_summary_to_newlines() {
    let source = r#"eval_pack pack "regression-pack" { cases: [{id: "one"}]; for case in cases { __io_println(case.id) }; summarize { __io_println(pack.id) } }"#;
    let result = format_source(source).unwrap();
    assert_eq!(
        result,
        "eval_pack pack \"regression-pack\" {\n  cases: [{id: \"one\"}]\n  for case in cases {\n    __io_println(case.id)\n  }\n  summarize {\n    __io_println(pack.id)\n  }\n}\n"
    );
}

#[test]
fn test_format_tool_description_semicolon_body_to_newlines() {
    let source = r#"tool read(path: string) { description "Read a file"; log(path) }"#;
    let result = format_source(source).unwrap();
    assert_eq!(
        result,
        "tool read(path: string) {\n  description \"Read a file\"\n  log(path)\n}\n"
    );
}

#[test]
fn test_single_newline_at_end() {
    let source = r#"pipeline default(task) {
  log("hello")
}"#;
    let result = format_source(source).unwrap();
    assert!(result.ends_with("}\n"));
    assert!(!result.ends_with("}\n\n"));
}

#[test]
fn test_no_trailing_whitespace() {
    let source = r#"pipeline default(task) {
  log("hello")
}"#;
    let result = format_source(source).unwrap();
    for line in result.lines() {
        assert_eq!(
            line,
            line.trim_end(),
            "Line has trailing whitespace: {line:?}"
        );
    }
}

#[test]
fn test_wraps_long_function_call_arguments() {
    let source = r"pipeline default(task) {
  let x = some_call(with_a_really_long_argument_name_one, with_a_really_long_argument_name_two, with_a_really_long_argument_name_three, with_a_really_long_argument_name_four, with_a_really_long_argument_name_five)
}";
    let result = format_source(source).unwrap();
    assert!(result.contains("some_call(\n"));
    assert!(result.contains("with_a_really_long_argument_name_five,\n"));
}

#[test]
fn test_removes_single_line_trailing_commas() {
    let source = r#"pipeline default(task) {
  let xs = [1, 2, 3,]
  let d = {name: "ship", enabled: true,}
  log("ready",)
}"#;
    let result = format_source(source).unwrap();
    assert_eq!(
        result,
        "pipeline default(task) {\n  let xs = [1, 2, 3]\n  let d = {name: \"ship\", enabled: true}\n  log(\"ready\")\n}\n"
    );
    assert_roundtrip(source);
}

#[test]
fn test_wraps_long_method_call_arguments() {
    let source = r"pipeline default(task) {
  let x = some_really_long_receiver_name.with_a_very_long_prefix().and_another_segment().call_some_extremely_long_method_name(with_a_really_long_argument_name_one, with_a_really_long_argument_name_two, with_a_really_long_argument_name_three, with_a_really_long_argument_name_four, with_a_really_long_argument_name_five)
}";
    let result = format_source(source).unwrap();
    assert!(result.contains(".call_some_extremely_long_method_name(\n"));
    assert!(result.contains("with_a_really_long_argument_name_five,\n"));
}

#[test]
fn test_wraps_long_list_literals() {
    let source = r"pipeline default(task) {
  let x = [with_a_really_long_item_name_one, with_a_really_long_item_name_two, with_a_really_long_item_name_three, with_a_really_long_item_name_four, with_a_really_long_item_name_five]
}";
    let result = format_source(source).unwrap();
    assert!(result.contains("[\n"));
    assert!(result.contains("with_a_really_long_item_name_five,\n"));
}

#[test]
fn test_adds_trailing_commas_when_wrapping_existing_multiline_sequences() {
    let source = r"pipeline default(task) {
  let xs = [
    with_a_really_long_item_name_one,
    with_a_really_long_item_name_two
  ]
  send(
    with_a_really_long_argument_name_one,
    with_a_really_long_argument_name_two
  )
}";
    let result = fmt_opts(source, 60);
    assert!(
        result.contains("with_a_really_long_item_name_two,\n"),
        "Expected wrapped list to end the last item with a comma, got:\n{result}"
    );
    assert!(
        result.contains("with_a_really_long_argument_name_two,\n"),
        "Expected wrapped call to end the last arg with a comma, got:\n{result}"
    );
    assert_roundtrip(source);
}

#[test]
fn test_wraps_long_dict_literals() {
    let source = r"pipeline default(task) {
  let x = {first_really_long_key_name: with_a_really_long_value_name_one, second_really_long_key_name: with_a_really_long_value_name_two, third_really_long_key_name: with_a_really_long_value_name_three}
}";
    let result = format_source(source).unwrap();
    assert!(result.contains("{\n"));
    assert!(result.contains("third_really_long_key_name: with_a_really_long_value_name_three,\n"));
}

#[test]
fn test_indents_nested_list_in_call_args() {
    // Regression for #741: nested list/dict literals inside multiline call
    // arguments must indent relative to their own opener.
    let source = r#"host_mock("project", "skills", [{name: "ship", description: "Ship a production release", when_to_use: "User says ship/release/deploy", body: "Follow the deploy runbook. One command at a time.", allowed_tools: ["deploy_service"], user_invocable: true}, {name: "inspect", description: "Inspect observability signals", body: "Query metrics, summarize anomalies.", allowed_tools: ["query_metrics", "look"]}], {})"#;
    let expected = r#"host_mock(
  "project",
  "skills",
  [
    {
      name: "ship",
      description: "Ship a production release",
      when_to_use: "User says ship/release/deploy",
      body: "Follow the deploy runbook. One command at a time.",
      allowed_tools: ["deploy_service"],
      user_invocable: true,
    },
    {
      name: "inspect",
      description: "Inspect observability signals",
      body: "Query metrics, summarize anomalies.",
      allowed_tools: ["query_metrics", "look"],
    },
  ],
  {},
)
"#;
    let result = format_source(source).unwrap();
    assert_eq!(result, expected);
    assert_roundtrip(source);
}

#[test]
fn test_indents_nested_dict_in_call_args() {
    // Same bug as above but with the outer arg being a dict literal whose
    // values are themselves wrapped collections.
    let source = r#"build_workflow("worker-flow", "act", {act: {kind: "stage", mode: "llm", model_policy: {provider: "mock"}, output_contract: {output_kinds: ["summary", "details"]}}})"#;
    let result = format_source(source).unwrap();
    // Outer call args sit at indent 2; inner dict body at indent 4; inner-inner
    // dict body at indent 6.
    assert!(
        result.contains("\n  {\n    act: {\n"),
        "Expected nested dict-in-call to indent relative to opener, got:\n{result}"
    );
    assert_roundtrip(source);
}

#[test]
fn test_indents_nested_collection_inside_pipeline_body() {
    // Same shape as #741 but one level deeper, inside a pipeline body.
    let source = r#"pipeline default(task) {
  host_mock("project", "skills", [{name: "ship", description: "Ship a production release", when_to_use: "User says ship/release/deploy", body: "Follow the deploy runbook. One command at a time.", allowed_tools: ["deploy_service"], user_invocable: true}], {})
}"#;
    let result = format_source(source).unwrap();
    // The list opener sits at indent 4 (depth 2). Items must land at indent 6.
    assert!(
        result.contains("    [\n      {\n        name: \"ship\",\n"),
        "Expected nested dict in pipeline-body call to indent relative to opener, got:\n{result}"
    );
    assert_roundtrip(source);
}

#[test]
fn test_wraps_long_struct_construction() {
    let source = r"struct BuildPlan {
  first_really_long_key_name: string
  second_really_long_key_name: string
  third_really_long_key_name: string
}

pipeline default(task) {
  let x = BuildPlan {first_really_long_key_name: with_a_really_long_value_name_one, second_really_long_key_name: with_a_really_long_value_name_two, third_really_long_key_name: with_a_really_long_value_name_three}
}";
    let result = format_source(source).unwrap();
    assert!(result.contains("BuildPlan {\n"));
    assert!(result.contains("third_really_long_key_name: with_a_really_long_value_name_three,\n"));
}

#[test]
fn test_wraps_long_enum_constructor_arguments() {
    let source = r"pipeline default(task) {
  let x = BuildPlan.Step(with_a_really_long_argument_name_one, with_a_really_long_argument_name_two, with_a_really_long_argument_name_three, with_a_really_long_argument_name_four)
}";
    let result = format_source(source).unwrap();
    assert!(result.contains("BuildPlan.Step(\n"));
    assert!(result.contains("with_a_really_long_argument_name_four,\n"));
}

#[test]
fn test_wraps_long_fn_decl_params() {
    let source = r"pipeline default(task) {
  fn process(first_really_long_param_name: string, second_really_long_param_name: int, third_really_long_param_name: bool) {
    log(1)
  }
}";
    let result = format_source(source).unwrap();
    assert!(
        result.contains("fn process(\n"),
        "Expected fn decl params to wrap, got:\n{result}"
    );
    assert_roundtrip(source);
}

#[test]
fn test_wraps_long_selective_import() {
    let source = r#"import { first_really_long_name, second_really_long_name, third_really_long_name, fourth_really_long_name } from "some/module"
pipeline default(task) { log(1) }"#;
    let result = format_source(source).unwrap();
    assert!(
        result.contains("  first_really_long_name,\n"),
        "Expected selective import names to wrap, got:\n{result}"
    );
    assert!(
        result.contains("  fourth_really_long_name,\n"),
        "Expected trailing comma on last import name, got:\n{result}"
    );
    assert_roundtrip(source);
}

#[test]
fn test_adds_parens_for_mixed_and_or() {
    // a && b || c — the AST is BinaryOp("||", BinaryOp("&&", a, b), c)
    // Formatter should add parens for clarity: (a && b) || c
    let source = r"pipeline default(task) {
  let x = a && b || c
}";
    let result = format_source(source).unwrap();
    assert!(
        result.contains("(a && b) || c"),
        "Expected clarifying parens for &&/|| mix, got:\n{result}"
    );
    assert_roundtrip(source);
}

#[test]
fn test_preserves_parens_or_inside_and() {
    // (a || b) && c — without parens this would change semantics
    let source = r"pipeline default(task) {
  let x = (a || b) && c
}";
    let result = format_source(source).unwrap();
    assert!(
        result.contains("(a || b) && c"),
        "Expected parens preserved for || inside &&, got:\n{result}"
    );
    assert_roundtrip(source);
}

#[test]
fn test_preserves_parens_lower_precedence_right() {
    // a * (b + c) — without parens this becomes a * b + c
    let source = r"pipeline default(task) {
  let x = a * (b + c)
}";
    let result = format_source(source).unwrap();
    assert!(
        result.contains("a * (b + c)"),
        "Expected parens preserved for + inside *, got:\n{result}"
    );
    assert_roundtrip(source);
}

#[test]
fn test_preserves_parens_right_subtraction() {
    // a - (b - c) — without parens this becomes a - b - c which differs
    let source = r"pipeline default(task) {
  let x = a - (b - c)
}";
    let result = format_source(source).unwrap();
    assert!(
        result.contains("a - (b - c)"),
        "Expected parens for right-child subtraction, got:\n{result}"
    );
    assert_roundtrip(source);
}

#[test]
fn test_long_binary_chain_wraps() {
    let source = r"pipeline default(task) {
  let x = first_really_long_variable_name + second_really_long_variable_name + third_really_long_variable_name + fourth_really_long_variable_name
}";
    let result = format_source(source).unwrap();
    // Should break and be idempotent
    assert_roundtrip(source);
    // Should contain a line-continuation break
    assert!(
        result.contains("\n    +") || result.contains("\n      +"),
        "Expected long binary chain to wrap, got:\n{result}"
    );
}

#[test]
fn test_subtraction_uses_backslash_continuation() {
    let source = r"pipeline default(task) {
  let x = first_really_long_variable_name + second_really_long_variable_name + third_really_long_variable_name - fourth_really_long_variable_name
}";
    let result = format_source(source).unwrap();
    assert_roundtrip(source);
    // The `-` operator needs `\` continuation
    if result.contains("\n    - ") {
        // If the break happened before `-`, there should be a backslash
        assert!(
            result.contains("\\\n"),
            "Expected backslash continuation before `-`, got:\n{result}"
        );
    }
}

#[test]
fn test_line_leading_safe_operators_do_not_use_backslash_continuation() {
    let source = r"pipeline default(task) {
  let fallback = first_really_long_variable_name ?? second_really_long_variable_name
  let same = first_really_long_variable_name == second_really_long_variable_name
}";
    let result = fmt_opts(source, 40);
    assert!(
        result.contains("\n    ?? ") && result.contains("\n    == "),
        "Expected line-leading ?? and == operators, got:\n{result}"
    );
    assert!(
        !result.contains("\\\n"),
        "Expected no backslash continuation for newline-safe operators, got:\n{result}"
    );

    let mut lexer = Lexer::new(&result);
    let tokens = lexer
        .tokenize()
        .unwrap_or_else(|e| panic!("Formatted output failed to lex:\n{result}\nError: {e}"));
    let mut parser = Parser::new(tokens);
    parser
        .parse()
        .unwrap_or_else(|e| panic!("Formatted output failed to parse:\n{result}\nError: {e}"));
    assert_eq!(result, fmt_opts(&result, 40));
}

#[test]
fn test_nested_function_call_wrapping() {
    let source = r"pipeline default(task) {
  let x = outer_function(inner_function(very_long_argument_name_one, very_long_argument_name_two, very_long_argument_name_three), another_really_long_argument_name)
}";
    assert_roundtrip(source);
}

#[test]
fn test_nested_list_in_dict_wrapping() {
    let source = r"pipeline default(task) {
  let x = {key_one: [really_long_element_one, really_long_element_two, really_long_element_three, really_long_element_four], key_two: value}
}";
    assert_roundtrip(source);
}

#[test]
fn test_nil_coalescing_with_logical_ops() {
    // `??` binds tighter than `||`/`&&`/comparisons/additive. The formatter
    // prints that grouping explicitly because people regularly read it the
    // other way around.
    let source = r"pipeline default(task) {
  let x = a ?? b || c
}";
    let result = format_source(source).unwrap();
    assert!(
        result.contains("(a ?? b) || c"),
        "Expected clarifying parens for ?? mixed with ||, got:\n{result}"
    );
    assert_roundtrip(source);
    // The opposite shape `a ?? (b || c)` must keep its parens — stripping them
    // would regroup to `(a ?? b) || c` and lose the `b || c` sub-expression.
    let source2 = r"pipeline default(task) {
  let x = a ?? (b || c)
}";
    let result2 = format_source(source2).unwrap();
    assert!(
        result2.contains("a ?? (b || c)"),
        "Expected parens preserved for (?? over || rhs), got:\n{result2}"
    );
    assert_roundtrip(source2);
}

#[test]
fn test_nil_coalescing_with_comparison_gets_clarifying_parens() {
    let source = r"pipeline default(task) {
  let x = classified == fixture?.expect_missing_dep ?? false
  let y = value ?? 0 > 0
}";
    let result = format_source(source).unwrap();
    assert!(
        result.contains("classified == (fixture?.expect_missing_dep ?? false)"),
        "Expected comparison rhs nil-coalescing to be parenthesized, got:\n{result}"
    );
    assert!(
        result.contains("(value ?? 0) > 0"),
        "Expected comparison lhs nil-coalescing to be parenthesized, got:\n{result}"
    );
    assert_roundtrip(source);
}

#[test]
fn test_division_right_associativity_preserved() {
    // a / (b / c) — must keep parens, otherwise becomes (a / b) / c
    let source = r"pipeline default(task) {
  let x = a / (b / c)
}";
    let result = format_source(source).unwrap();
    assert!(
        result.contains("a / (b / c)"),
        "Expected parens for right-child division, got:\n{result}"
    );
    assert_roundtrip(source);
}

#[test]
fn test_exponentiation_formats_with_natural_right_associativity() {
    let source = r"pipeline default(task) {
  let x = a ** b ** c
}";
    let result = format_source(source).unwrap();
    assert!(
        result.contains("a ** b ** c"),
        "Expected natural right-associative exponentiation, got:\n{result}"
    );
    assert_roundtrip(source);
}

#[test]
fn test_exponentiation_preserves_left_grouping_when_forced() {
    let source = r"pipeline default(task) {
  let x = (a ** b) ** c
}";
    let result = format_source(source).unwrap();
    assert!(
        result.contains("(a ** b) ** c"),
        "Expected parens preserved for left-grouped exponentiation, got:\n{result}"
    );
    assert_roundtrip(source);
}

#[test]
fn test_exponentiation_binds_tighter_than_multiplication() {
    let source = r"pipeline default(task) {
  let x = a * b ** c
}";
    let result = format_source(source).unwrap();
    assert!(
        result.contains("a * b ** c"),
        "Expected exponentiation to bind tighter than multiplication, got:\n{result}"
    );
    assert_roundtrip(source);
}

#[test]
fn test_multiplication_of_addition() {
    // a * (b + c) must not lose parens
    let source = r"pipeline default(task) {
  let x = a * (b + c)
}";
    let result = format_source(source).unwrap();
    assert!(result.contains("a * (b + c)"), "got:\n{result}");
    assert_roundtrip(source);
}

#[test]
fn test_no_unnecessary_parens_same_op() {
    // a + b + c — all same associative op, no parens needed
    let source = r"pipeline default(task) {
  let x = a + b + c
}";
    let result = format_source(source).unwrap();
    assert!(
        result.contains("a + b + c"),
        "Expected no unnecessary parens, got:\n{result}"
    );
    assert_roundtrip(source);
}

#[test]
fn test_preserves_parens_right_grouped_addition() {
    // a + (b + c) must keep its explicit rhs grouping.
    let source = r"pipeline default(task) {
  let x = a + (b + c)
}";
    let result = format_source(source).unwrap();
    assert!(
        result.contains("a + (b + c)"),
        "Expected parens preserved for right-grouped addition, got:\n{result}"
    );
    assert_roundtrip(source);
}

#[test]
fn test_preserves_parens_right_grouped_multiplication() {
    // a * (b * c) must keep its explicit rhs grouping.
    let source = r"pipeline default(task) {
  let x = a * (b * c)
}";
    let result = format_source(source).unwrap();
    assert!(
        result.contains("a * (b * c)"),
        "Expected parens preserved for right-grouped multiplication, got:\n{result}"
    );
    assert_roundtrip(source);
}

#[test]
fn test_preserves_parens_right_grouped_nil_coalescing() {
    // a ?? (b ?? c) must keep its explicit rhs grouping.
    let source = r"pipeline default(task) {
  let x = a ?? (b ?? c)
}";
    let result = format_source(source).unwrap();
    assert!(
        result.contains("a ?? (b ?? c)"),
        "Expected parens preserved for right-grouped nil coalescing, got:\n{result}"
    );
    assert_roundtrip(source);
}

#[test]
fn test_no_parens_for_natural_precedence() {
    // a + b * c — * binds tighter, no parens needed
    let source = r"pipeline default(task) {
  let x = a + b * c
}";
    let result = format_source(source).unwrap();
    assert!(
        result.contains("a + b * c"),
        "Expected no parens for natural precedence, got:\n{result}"
    );
    assert_roundtrip(source);
}

// --- Idempotence of wrapped output ---

#[test]
fn test_already_wrapped_fn_params_stable() {
    // Input that's already wrapped should not change
    let source = r"pipeline default(task) {
  fn process(
    first_really_long_param_name: string,
    second_really_long_param_name: int,
    third_really_long_param_name: bool,
  ) {
    log(1)
  }
}";
    assert_roundtrip(source);
}

#[test]
fn test_already_wrapped_import_stable() {
    let source = r#"import {
  first_really_long_name,
  second_really_long_name,
  third_really_long_name,
  fourth_really_long_name,
} from "some/module"
pipeline default(task) { log(1) }"#;
    assert_roundtrip(source);
}

#[test]
fn test_backslash_continuation_roundtrip() {
    // Source with backslash continuation should format and re-format stably
    let source = r"pipeline default(task) {
  let x = first_really_long_variable_name + second_really_long_variable_name \
    == third_really_long_variable_name
}";
    assert_roundtrip(source);
}

// --- Custom line width ---

fn fmt_opts(source: &str, line_width: usize) -> String {
    let opts = FmtOptions {
        line_width,
        separator_width: AUTO_SEPARATOR_WIDTH,
    };
    format_source_opts(source, &opts).unwrap()
}

#[test]
fn test_custom_line_width_wraps_earlier() {
    // "really_long_function_name(" = 26 chars; "alpha, beta, gamma" = 18; 26+18+1 = 45 > 40
    let source = r"pipeline default() {
  let x = really_long_function_name(alpha, beta, gamma)
}";
    let result = fmt_opts(source, 40);
    assert!(
        result.contains("really_long_function_name(\n"),
        "Expected wrapped call at width 40, got:\n{result}"
    );
    assert!(
        result.contains("    alpha,"),
        "Expected indented first arg, got:\n{result}"
    );
}

#[test]
fn test_custom_line_width_stays_inline_at_default() {
    // Same call should NOT wrap at default width 100.
    let source = r"pipeline default() {
  let x = really_long_function_name(alpha, beta, gamma)
}";
    let result = format_source(source).unwrap();
    assert!(
        result.contains("really_long_function_name(alpha, beta, gamma)"),
        "Should stay inline at width 100, got:\n{result}"
    );
}

#[test]
fn test_custom_line_width_wraps_list() {
    // "item_one, item_two, item_three, item_four" = 41 chars; with "[" prefix=1 → 42+1 > 40
    let source = r"pipeline default() {
  let x = [item_one, item_two, item_three, item_four]
}";
    let result = fmt_opts(source, 40);
    assert!(
        result.contains("[\n"),
        "Expected wrapped list at width 40, got:\n{result}"
    );
}

#[test]
fn test_custom_line_width_stays_inline_when_fits() {
    let source = r"pipeline default() {
  let x = foo(a, b)
}";
    let result = fmt_opts(source, 40);
    assert!(
        result.contains("foo(a, b)"),
        "Short call should stay inline at width 40, got:\n{result}"
    );
}

#[test]
fn test_default_opts_matches_format_source() {
    let source = r"pipeline default() {
  let x = compute(some_arg, another_arg)
}";
    let default_result = format_source(source).unwrap();
    let opts_result = format_source_opts(source, &FmtOptions::default()).unwrap();
    assert_eq!(default_result, opts_result);
}

#[test]
fn test_custom_line_width_wraps_fn_params() {
    // "  fn process(" = 14 chars; "input: string, options: dict" = 28; 14+28+1 = 43 > 40
    let source = r"pipeline default() {
  fn process(input: string, options: dict) -> string {
    return input
  }
}";
    let result = fmt_opts(source, 40);
    assert!(
        result.contains("fn process(\n"),
        "Expected wrapped fn params at width 40, got:\n{result}"
    );
}

#[test]
fn test_custom_line_width_idempotent() {
    let source = r"pipeline default() {
  let x = really_long_function_name(alpha, beta, gamma)
}";
    let opts = FmtOptions {
        line_width: 40,
        separator_width: AUTO_SEPARATOR_WIDTH,
    };
    let first = format_source_opts(source, &opts).unwrap();
    let second = format_source_opts(&first, &opts).unwrap();
    assert_eq!(first, second, "Custom-width formatter is not idempotent");
}

#[test]
fn test_custom_line_width_wraps_selective_import_with_trailing_comma() {
    let source = r#"import { first_really_long_name, second_really_long_name, third_really_long_name } from "module"
pipeline default() {
  log(1)
}"#;
    let result = fmt_opts(source, 50);
    assert!(
        result.contains("  third_really_long_name,\n"),
        "Expected wrapped import with trailing comma at width 50, got:\n{result}"
    );
}

// --- Postfix and unary precedence parens ---

#[test]
fn test_parens_binary_op_method_call() {
    let source = r#"pipeline default(task) {
  let x = (a ?? b).split("\n")
}"#;
    let result = format_source(source).unwrap();
    assert!(
        result.contains(r#"(a ?? b).split("\n")"#),
        "Expected parens preserved for ?? as method call object, got:\n{result}"
    );
    assert_roundtrip(source);
}

#[test]
fn test_parens_binary_op_property_access() {
    let source = r"pipeline default(task) {
  let x = (a + b).length
}";
    let result = format_source(source).unwrap();
    assert!(
        result.contains("(a + b).length"),
        "Expected parens preserved for + as property access object, got:\n{result}"
    );
    assert_roundtrip(source);
}

#[test]
fn test_parens_binary_op_subscript() {
    let source = r"pipeline default(task) {
  let x = (a ?? b)[0]
}";
    let result = format_source(source).unwrap();
    assert!(
        result.contains("(a ?? b)[0]"),
        "Expected parens preserved for ?? as subscript object, got:\n{result}"
    );
    assert_roundtrip(source);
}

#[test]
fn test_parens_unary_op_method_call() {
    let source = r"pipeline default(task) {
  let x = (-a).abs()
}";
    let result = format_source(source).unwrap();
    assert!(
        result.contains("(-a).abs()"),
        "Expected parens for unary-op as method call object, got:\n{result}"
    );
    assert_roundtrip(source);
}

#[test]
fn test_parens_unary_op_property_access() {
    let source = r"pipeline default(task) {
  let x = (!flag).description
}";
    let result = format_source(source).unwrap();
    assert!(
        result.contains("(!flag).description"),
        "Expected parens for unary-op as property access object, got:\n{result}"
    );
    assert_roundtrip(source);
}

#[test]
fn test_parens_ternary_method_call() {
    let source = r"pipeline default(task) {
  let x = (a ? b : c).method()
}";
    let result = format_source(source).unwrap();
    assert!(
        result.contains("(a ? b : c).method()"),
        "Expected parens for ternary as method call object, got:\n{result}"
    );
    assert_roundtrip(source);
}

#[test]
fn test_parens_unary_binary_operand() {
    let source = r"pipeline default(task) {
  let x = !(a + b)
}";
    let result = format_source(source).unwrap();
    assert!(
        result.contains("!(a + b)"),
        "Expected parens for binary op as unary operand, got:\n{result}"
    );
    assert_roundtrip(source);
}

#[test]
fn test_parens_chained_unary_method_on_binary() {
    // !(a ?? b).trim() — the unary wraps a method call whose object is a binary op
    let source = r"pipeline default(task) {
  let x = !(a ?? b).trim()
}";
    let result = format_source(source).unwrap();
    assert!(
        result.contains("!(a ?? b).trim()"),
        "Expected parens preserved in chained unary+method+binary, got:\n{result}"
    );
    assert_roundtrip(source);
}

#[test]
fn test_parens_chained_postfix_on_binary() {
    let source = r"pipeline default(task) {
  let x = (a + b)[0].method()
}";
    let result = format_source(source).unwrap();
    assert!(
        result.contains("(a + b)[0].method()"),
        "Expected parens for chained postfix on binary op, got:\n{result}"
    );
    assert_roundtrip(source);
}

#[test]
fn test_parens_binary_op_optional_method() {
    let source = r"pipeline default(task) {
  let x = (a ?? b)?.method()
}";
    let result = format_source(source).unwrap();
    assert!(
        result.contains("(a ?? b)?.method()"),
        "Expected parens for ?? as optional method call object, got:\n{result}"
    );
    assert_roundtrip(source);
}

#[test]
fn test_parens_binary_op_optional_property() {
    let source = r"pipeline default(task) {
  let x = (a ?? b)?.length
}";
    let result = format_source(source).unwrap();
    assert!(
        result.contains("(a ?? b)?.length"),
        "Expected parens for ?? as optional property access object, got:\n{result}"
    );
    assert_roundtrip(source);
}

#[test]
fn test_parens_long_binary_op_method_call_roundtrip() {
    let source = r#"pipeline default(task) {
  let x = (first_really_long_name ?? second_really_long_name).split("x")
}"#;
    assert_roundtrip(source);
}

#[test]
fn test_no_unnecessary_parens_on_simple_method_call() {
    // Normal method calls on identifiers, literals, etc. should NOT get parens
    let source = r#"pipeline default(task) {
  let x = text.split("\n")
  let y = items[0].name
  let z = obj?.method()
}"#;
    let result = format_source(source).unwrap();
    assert!(
        !result.contains("(text)"),
        "Should not add parens to simple identifier, got:\n{result}"
    );
    assert_roundtrip(source);
}

// --- Short lines stay inline ---

#[test]
fn test_short_fn_params_stay_inline() {
    let source = r"pipeline default(task) {
  fn add(a: int, b: int) -> int {
    return a + b
  }
}";
    let result = format_source(source).unwrap();
    assert!(
        result.contains("fn add(a: int, b: int) -> int {"),
        "Short params should stay inline, got:\n{result}"
    );
    assert_roundtrip(source);
}

#[test]
fn test_short_import_stays_inline() {
    let source = r#"import { foo, bar, baz } from "module"
pipeline default(task) { log(1) }"#;
    let result = format_source(source).unwrap();
    assert!(
        result.contains("import { bar, baz, foo } from \"module\""),
        "Short import should stay inline, got:\n{result}"
    );
    assert_roundtrip(source);
}

#[test]
fn test_import_block_sorts_std_first_then_alphabetically() {
    let source = r#"import "zeta"
import "std/http"
import "alpha"
pipeline default(task) { log(1) }"#;
    let result = format_source(source).unwrap();

    let std_index = result.find("import \"std/http\"").unwrap();
    let alpha_index = result.find("import \"alpha\"").unwrap();
    let zeta_index = result.find("import \"zeta\"").unwrap();

    assert!(
        std_index < alpha_index && alpha_index < zeta_index,
        "imports should sort with std/ first, got:\n{result}"
    );
}

#[test]
fn test_selective_import_names_sort_alphabetically() {
    let source = r#"import { zebra, alpha, middle } from "module"
pipeline default(task) { log(1) }"#;
    let result = format_source(source).unwrap();
    assert!(
        result.contains("import { alpha, middle, zebra } from \"module\""),
        "selective import names should sort, got:\n{result}"
    );
}

#[test]
fn test_roundtrip_never_type_annotation() {
    assert_roundtrip(
        r#"pipeline default(task) {
  fn fail() -> never {
    throw "err"
  }
}"#,
    );
}

#[test]
fn test_type_decl_shape_wraps_when_too_wide() {
    // A shape with many fields exceeds the 100-column line width
    // when rendered inline, so the formatter must split it onto
    // one-field-per-line. This was the round-trip regression that
    // collapsed `stdlib_hitl.harn` to a single line on every fmt run.
    let source = "type Big = {alpha: int, beta: string, gamma: bool, delta: list<dict<string, int>>, epsilon: duration, zeta: string}\n";
    let formatted = format_source(source).unwrap();
    assert!(
        formatted.contains("\n  alpha: int,"),
        "expected wrapped one-field-per-line shape, got:\n{formatted}"
    );
    assert!(
        formatted.contains("\n  zeta: string,"),
        "expected last field on its own line with trailing comma, got:\n{formatted}"
    );
    // And the wrapped form must round-trip cleanly.
    assert_roundtrip(source);
}

#[test]
fn test_type_decl_shape_stays_inline_when_short() {
    // Short shapes that fit comfortably stay on one line — the
    // wrapping decision is line-width driven, not a blanket
    // "always wrap shapes" rule.
    let source = "type Small = {x: int, y: int}\n";
    let formatted = format_source(source).unwrap();
    assert!(
        formatted.contains("type Small = {x: int, y: int}"),
        "expected single-line form, got:\n{formatted}"
    );
    assert_roundtrip(source);
}

#[test]
fn test_blank_line_between_top_level_fns() {
    let source = "fn one() -> int {\n  return 1\n}\nfn two() -> int {\n  return 2\n}\n";
    let result = format_source(source).unwrap();
    assert!(
        result.contains("}\n\nfn two"),
        "expected a blank line between adjacent top-level fns, got:\n{result}"
    );
    // Idempotence: formatting the formatted output must yield the same string.
    let result2 = format_source(&result).unwrap();
    assert_eq!(result, result2, "formatter is not idempotent for two fns");
}

#[test]
fn test_blank_line_between_mixed_top_level_items_idempotent() {
    let source = "type A = int\ntype B = string\nstruct C {\n  a: int\n}\nenum E {\n  X\n}\nfn f() -> int {\n  return 1\n}\n";
    let result = format_source(source).unwrap();
    // Each adjacent pair should be separated by exactly one blank line.
    assert!(result.contains("type A = int\n\ntype B"));
    assert!(result.contains("type B = string\n\nstruct"));
    assert!(result.contains("}\n\nenum"));
    assert!(result.contains("}\n\nfn"));
    let result2 = format_source(&result).unwrap();
    assert_eq!(
        result, result2,
        "formatter is not idempotent for mixed top-level items"
    );
}

#[test]
fn test_multiline_string_preserves_verbatim_body() {
    let source = "pipeline test(task) {\n  let template = \"\"\"\n// Auto-generated code - do not edit.\npub fn {{ fn_name }}() -> dict {\n  return {}\n}\n\"\"\"\n  log(template)\n}\n";
    let out = format_source(source).unwrap();
    assert_eq!(
        out, source,
        "formatter should preserve triple-quoted strings verbatim"
    );
    let out2 = format_source(&out).unwrap();
    assert_eq!(
        out, out2,
        "formatter should be idempotent on multiline strings"
    );
}

#[test]
fn test_prompt_section_template_string_preserves_verbatim_body() {
    let source = "pipeline test(task) {\n  let template = \"\"\"\n{{ section \"task\" }}\nBuild {{ thing }}.\n{{ endsection }}\n{{ section \"tools\" tools=tool_list }}{{ endsection }}\n\"\"\"\n  log(render_string(template, {thing: \"it\", tool_list: []}))\n}\n";
    let out = format_source(source).unwrap();
    assert_eq!(
        out, source,
        "formatter should preserve prompt sections inside triple-quoted strings verbatim"
    );
    let out2 = format_source(&out).unwrap();
    assert_eq!(out, out2, "formatter should be idempotent");
}

#[test]
fn test_multiline_interpolated_string_preserves_verbatim_body() {
    let source = "pipeline test(task) {\n  let name = \"x\"\n  let g = \"\"\"\n  root:\n    name: ${name}  \n    nested:\n      keep: exact\n\"\"\"\n  log(g)\n}\n";
    let out = format_source(source).unwrap();
    assert_eq!(
        out, source,
        "formatter should preserve interpolated triple-quoted strings verbatim"
    );
    let out2 = format_source(&out).unwrap();
    assert_eq!(out, out2, "formatter should be idempotent");
}

#[test]
fn test_string_literal_escapes_interpolation_start() {
    let source = "pipeline test(task) {\n  let s = \"\\${VAR}\"\n  log(s)\n}\n";
    let out = format_source(source).unwrap();
    assert_eq!(
        out, source,
        "formatter should preserve escaped interpolation starts"
    );
    let out2 = format_source(&out).unwrap();
    assert_eq!(out, out2, "formatter should be idempotent");
}

#[test]
fn test_multi_nil_coalescing_chain_wraps_each_operand() {
    // A chain of ≥3 `??` operators must wrap with each operator at
    // line start and a +2-space continuation indent relative to the
    // owning statement (body indent 2 → continuation indent 4).
    let source = r"pipeline default(task) {
  let x = first_long_name ?? second_long_name ?? third_long_name ?? fourth_long_name
}";
    let result = fmt_opts(source, 30);
    for expected in [
        "\n    ?? second_long_name",
        "\n    ?? third_long_name",
        "\n    ?? fourth_long_name",
    ] {
        assert!(
            result.contains(expected),
            "Expected line-leading `??` operator `{expected}`, got:\n{result}"
        );
    }
    assert!(
        !result.contains("\\\n"),
        "`??` is newline-safe; no backslash continuation expected, got:\n{result}"
    );
    assert_eq!(
        result,
        fmt_opts(&result, 30),
        "formatter is not idempotent on multi-?? chain"
    );
}

#[test]
fn test_nil_coalescing_chained_with_method_call_wraps() {
    // A method chain + trailing `??` must place the `??` on its own
    // line (continuation-indented) while the method chain stays intact
    // when it fits.
    let source = r"pipeline default(task) {
  let x = source.filter(keep).map(transform).collect() ?? fallback_sentinel
}";
    let result = fmt_opts(source, 40);
    assert!(
        result.contains("\n    ?? fallback_sentinel"),
        "Expected line-leading `??` after method chain, got:\n{result}"
    );
    assert!(
        !result.contains("\\\n"),
        "no backslash continuation expected, got:\n{result}"
    );
    assert_eq!(
        result,
        fmt_opts(&result, 40),
        "formatter is not idempotent on method-chain + ??"
    );
}

#[test]
fn test_optional_shorthand_rewrites_t_or_nil() {
    let source = "pipeline default(task) { let x: int | nil = nil\nlog(x) }";
    let formatted = format_source(source).unwrap();
    assert!(
        formatted.contains("let x: int? = nil"),
        "expected `int?` sugar, got:\n{formatted}"
    );
    assert!(
        !formatted.contains("int | nil"),
        "expected no `int | nil`, got:\n{formatted}"
    );
}

#[test]
fn test_optional_shorthand_idempotent() {
    let source = "pipeline default(task) { let x: int? = nil\nlog(x) }";
    let formatted = format_source(source).unwrap();
    let formatted2 = format_source(&formatted).unwrap();
    assert_eq!(formatted, formatted2);
    assert!(formatted.contains("int?"));
}

#[test]
fn test_optional_shorthand_skips_when_arm_is_union_or_intersection() {
    // `(A & B) | nil` cannot round-trip via `T?` because postfix `?`
    // attaches to a primary; the explicit form must be preserved.
    let source = "pipeline default(task) { let x: {a: int} & {b: string} | nil = nil\nlog(x) }";
    let formatted = format_source(source).unwrap();
    assert!(
        formatted.contains(" | nil"),
        "intersection-union should keep the long form, got:\n{formatted}"
    );
}

#[test]
fn test_imports_stay_tight_then_blank_before_first_item() {
    let source = "import \"std/http\"\nimport \"alpha\"\nimport \"zeta\"\npipeline default(task) { log(1) }\n";
    let result = format_source(source).unwrap();
    assert!(
        result.contains("import \"std/http\"\nimport \"alpha\"\nimport \"zeta\"\n\npipeline"),
        "imports should be tight with a single blank line before the first non-import item, got:\n{result}"
    );
    let result2 = format_source(&result).unwrap();
    assert_eq!(
        result, result2,
        "formatter is not idempotent around imports"
    );
}

#[test]
fn test_method_call_args_dont_overcount_multiline_receiver() {
    // When the receiver of a method call wraps to multiple lines, the
    // method-call args should be laid out based on the new line's column,
    // not the receiver's total byte length. With short args, they should
    // stay on the same line as the `.method(`.
    let source = r"pipeline default(task) {
  let x = some_function_with_a_pretty_long_name_that_will_wrap_its_args(arg_one, arg_two, arg_three)
    .map(item)
}";
    let result = format_source(source).unwrap();
    // Short args list (just `item`) must NOT wrap onto its own line just
    // because the receiver wrapped onto multiple lines above.
    assert!(
        result.contains(".map(item)"),
        "trailing method args wrapped unnecessarily after multi-line receiver:\n{result}"
    );
    assert_roundtrip(source);
}

#[test]
fn test_optional_method_call_args_dont_overcount_multiline_receiver() {
    let source = r"pipeline default(task) {
  let x = some_function_with_a_pretty_long_name_that_will_wrap_its_args(arg_one, arg_two, arg_three)
    ?.map(item)
}";
    let result = format_source(source).unwrap();
    assert!(
        result.contains("?.map(item)"),
        "trailing optional method args wrapped unnecessarily after multi-line receiver:\n{result}"
    );
    assert_roundtrip(source);
}

#[test]
fn test_roundtrip_plain_raw_string_keeps_no_hashes() {
    // A raw string without `"` must not gain hashes.
    let source = "pipeline default(task) { let p = r\"\\d+\" }";
    let result = format_source(source).unwrap();
    assert!(
        result.contains("r\"\\d+\""),
        "plain raw string should stay r\"...\":\n{result}"
    );
    assert_roundtrip(source);
}

#[test]
fn test_roundtrip_hashed_raw_string_with_quote() {
    // A raw string containing `"` must format with the hashed delimiter
    // (otherwise the output would re-lex incorrectly).
    let source = "pipeline default(task) { let p = r#\"\"([^\"]*)\"\"# }";
    let result = format_source(source).unwrap();
    assert!(
        result.contains("r#\"\"([^\"]*)\"\"#"),
        "raw string with quotes must keep the hashed delimiter:\n{result}"
    );
    assert_roundtrip(source);
}

#[test]
fn test_raw_string_with_quote_never_emits_bare_delimiter() {
    // Regression guard: a quote-bearing raw string must never be emitted as
    // r"...", which would terminate at the inner quote and corrupt the source.
    let source = "pipeline default(task) { let p = r#\"a\"b\"# }";
    let result = format_source(source).unwrap();
    assert!(
        !result.contains("r\"a\"b\""),
        "must not emit a bare r\"...\" around an embedded quote:\n{result}"
    );
    assert_roundtrip(source);
}
