use harn_lexer::Lexer;
use harn_parser::Parser;

use crate::helpers::format_duration;
use crate::{format_source, format_source_opts, FmtOptions};

fn assert_roundtrip(source: &str) {
    let formatted = format_source(source).unwrap();
    let mut lexer = Lexer::new(&formatted);
    let tokens = lexer
        .tokenize()
        .unwrap_or_else(|e| panic!("Formatted output failed to lex:\n{formatted}\nError: {e}"));
    let mut parser = Parser::new(tokens);
    parser
        .parse()
        .unwrap_or_else(|e| panic!("Formatted output failed to parse:\n{formatted}\nError: {e}"));
    // Format again and verify idempotence
    let formatted2 = format_source(&formatted).unwrap();
    assert_eq!(formatted, formatted2, "Formatter is not idempotent");
}

#[test]
fn test_roundtrip_basic() {
    assert_roundtrip("pipeline default(task) { let x = 42\nlog(x) }");
}

#[test]
fn test_roundtrip_fn_decl() {
    assert_roundtrip("pipeline default(task) { fn add(a, b) { return a + b }\nlog(add(1, 2)) }");
}

#[test]
fn test_roundtrip_closure() {
    assert_roundtrip("pipeline default(task) { let f = { x -> x * 2 }\nlog(f(3)) }");
}

#[test]
fn test_roundtrip_if_else() {
    assert_roundtrip("pipeline default(task) { if true { log(1) } else { log(2) } }");
}

#[test]
fn test_roundtrip_try_catch() {
    assert_roundtrip(r#"pipeline default(task) { try { throw "e" } catch (e) { log(e) } }"#);
}

#[test]
fn test_roundtrip_for_in() {
    assert_roundtrip("pipeline default(task) { for i in [1, 2, 3] { log(i) } }");
}

#[test]
fn test_roundtrip_match() {
    assert_roundtrip(
        r#"pipeline default(task) { match x { "a" -> { log(1) } "b" -> { log(2) } } }"#,
    );
}

#[test]
fn test_roundtrip_computed_dict_key() {
    assert_roundtrip(
        r#"pipeline default(task) { let k = "x"
  let d = {[k]: 42, fixed: 1} }"#,
    );
}

#[test]
fn test_roundtrip_interface() {
    assert_roundtrip(
        "interface Printable {\n  fn to_display() -> string\n}\npipeline default(task) { log(1) }",
    );
}

#[test]
fn test_roundtrip_public_decls_and_generic_interface() {
    assert_roundtrip(
        "pub pipeline build(task) extends base {\n  return\n}\n\npub enum Result {\n  Ok(value: string)\n}\n\npub struct Config {\n  port?: int\n}\n\ninterface Repository<T> {\n  fn map<U>(value: T, f: fn(T) -> U) -> U\n}",
    );
}

#[test]
fn test_roundtrip_enum() {
    assert_roundtrip("enum Color {\n  Red\n  Green\n  Blue\n}\npipeline default(task) { log(1) }");
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
fn test_format_let_var() {
    let source = r#"pipeline default(task) {
  let x = 42
  var y = "hello"
}"#;
    let result = format_source(source).unwrap();
    assert!(result.contains("let x = 42"));
    assert!(result.contains("var y = \"hello\""));
}

#[test]
fn test_format_binary_ops() {
    let source = r#"pipeline default(task) {
  let x = 1 + 2
  let y = a * b
}"#;
    let result = format_source(source).unwrap();
    assert!(result.contains("1 + 2"));
    assert!(result.contains("a * b"));
}

#[test]
fn test_format_duration() {
    assert_eq!(format_duration(5000), "5s");
    assert_eq!(format_duration(60000), "1m");
    assert_eq!(format_duration(3600000), "1h");
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
    let source = r#"pipeline default(task) {
  for i in [1, 2, 3] {
    log(i)
  }
}"#;
    let result = format_source(source).unwrap();
    assert!(result.contains("for i in [1, 2, 3] {"));
}

#[test]
fn test_format_fn() {
    let source = r#"pipeline default(task) {
  fn add(a, b) {
    return a + b
  }
}"#;
    let result = format_source(source).unwrap();
    assert!(result.contains("fn add(a, b) {"));
    assert!(result.contains("return a + b"));
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
            "Line has trailing whitespace: {:?}",
            line
        );
    }
}

#[test]
fn test_wraps_long_function_call_arguments() {
    let source = r#"pipeline default(task) {
  let x = some_call(with_a_really_long_argument_name_one, with_a_really_long_argument_name_two, with_a_really_long_argument_name_three, with_a_really_long_argument_name_four, with_a_really_long_argument_name_five)
}"#;
    let result = format_source(source).unwrap();
    assert!(result.contains("some_call(\n"));
    assert!(result.contains("with_a_really_long_argument_name_five,\n"));
}

#[test]
fn test_wraps_long_method_call_arguments() {
    let source = r#"pipeline default(task) {
  let x = some_really_long_receiver_name.with_a_very_long_prefix().and_another_segment().call_some_extremely_long_method_name(with_a_really_long_argument_name_one, with_a_really_long_argument_name_two, with_a_really_long_argument_name_three, with_a_really_long_argument_name_four, with_a_really_long_argument_name_five)
}"#;
    let result = format_source(source).unwrap();
    assert!(result.contains(".call_some_extremely_long_method_name(\n"));
    assert!(result.contains("with_a_really_long_argument_name_five,\n"));
}

#[test]
fn test_wraps_long_list_literals() {
    let source = r#"pipeline default(task) {
  let x = [with_a_really_long_item_name_one, with_a_really_long_item_name_two, with_a_really_long_item_name_three, with_a_really_long_item_name_four, with_a_really_long_item_name_five]
}"#;
    let result = format_source(source).unwrap();
    assert!(result.contains("[\n"));
    assert!(result.contains("with_a_really_long_item_name_five,\n"));
}

#[test]
fn test_wraps_long_dict_literals() {
    let source = r#"pipeline default(task) {
  let x = {first_really_long_key_name: with_a_really_long_value_name_one, second_really_long_key_name: with_a_really_long_value_name_two, third_really_long_key_name: with_a_really_long_value_name_three}
}"#;
    let result = format_source(source).unwrap();
    assert!(result.contains("{\n"));
    assert!(result.contains("third_really_long_key_name: with_a_really_long_value_name_three,\n"));
}

#[test]
fn test_wraps_long_struct_construction() {
    let source = r#"pipeline default(task) {
  let x = BuildPlan {first_really_long_key_name: with_a_really_long_value_name_one, second_really_long_key_name: with_a_really_long_value_name_two, third_really_long_key_name: with_a_really_long_value_name_three}
}"#;
    let result = format_source(source).unwrap();
    assert!(result.contains("BuildPlan\n  {\n"));
    assert!(result.contains("third_really_long_key_name: with_a_really_long_value_name_three,\n"));
}

#[test]
fn test_wraps_long_enum_constructor_arguments() {
    let source = r#"pipeline default(task) {
  let x = BuildPlan.Step(with_a_really_long_argument_name_one, with_a_really_long_argument_name_two, with_a_really_long_argument_name_three, with_a_really_long_argument_name_four)
}"#;
    let result = format_source(source).unwrap();
    assert!(result.contains("BuildPlan.Step(\n"));
    assert!(result.contains("with_a_really_long_argument_name_four,\n"));
}

// --- New tests for line-splitting and operator fixes ---

#[test]
fn test_wraps_long_fn_decl_params() {
    let source = r#"pipeline default(task) {
  fn process(first_really_long_param_name: string, second_really_long_param_name: int, third_really_long_param_name: bool) {
    log(1)
  }
}"#;
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
    let source = r#"pipeline default(task) {
  let x = a && b || c
}"#;
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
    let source = r#"pipeline default(task) {
  let x = (a || b) && c
}"#;
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
    let source = r#"pipeline default(task) {
  let x = a * (b + c)
}"#;
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
    let source = r#"pipeline default(task) {
  let x = a - (b - c)
}"#;
    let result = format_source(source).unwrap();
    assert!(
        result.contains("a - (b - c)"),
        "Expected parens for right-child subtraction, got:\n{result}"
    );
    assert_roundtrip(source);
}

#[test]
fn test_long_binary_chain_wraps() {
    let source = r#"pipeline default(task) {
  let x = first_really_long_variable_name + second_really_long_variable_name + third_really_long_variable_name + fourth_really_long_variable_name
}"#;
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
    let source = r#"pipeline default(task) {
  let x = first_really_long_variable_name + second_really_long_variable_name + third_really_long_variable_name - fourth_really_long_variable_name
}"#;
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

// --- Nested wrapping ---

#[test]
fn test_nested_function_call_wrapping() {
    let source = r#"pipeline default(task) {
  let x = outer_function(inner_function(very_long_argument_name_one, very_long_argument_name_two, very_long_argument_name_three), another_really_long_argument_name)
}"#;
    assert_roundtrip(source);
}

#[test]
fn test_nested_list_in_dict_wrapping() {
    let source = r#"pipeline default(task) {
  let x = {key_one: [really_long_element_one, really_long_element_two, really_long_element_three, really_long_element_four], key_two: value}
}"#;
    assert_roundtrip(source);
}

// --- Operator precedence edge cases ---

#[test]
fn test_nil_coalescing_with_logical_ops() {
    // `??` binds tighter than `||`, `&&`, comparisons, and additive ops
    // (placed between multiplicative and additive in the parser). So
    // `a ?? b || c` parses naturally as `(a ?? b) || c` and needs no parens.
    // This matches the intuition `xs?.count ?? 0 > 0` → `(xs?.count ?? 0) > 0`.
    let source = r#"pipeline default(task) {
  let x = a ?? b || c
}"#;
    let result = format_source(source).unwrap();
    assert!(
        result.contains("a ?? b || c"),
        "Expected no parens — natural precedence is (a ?? b) || c, got:\n{result}"
    );
    assert_roundtrip(source);
    // The opposite shape `a ?? (b || c)` MUST keep its parens, because the
    // default grouping is `(a ?? b) || c` — stripping the parens would lose
    // the `b || c` sub-expression.
    let source2 = r#"pipeline default(task) {
  let x = a ?? (b || c)
}"#;
    let result2 = format_source(source2).unwrap();
    assert!(
        result2.contains("a ?? (b || c)"),
        "Expected parens preserved for (?? over || rhs), got:\n{result2}"
    );
    assert_roundtrip(source2);
}

#[test]
fn test_division_right_associativity_preserved() {
    // a / (b / c) — must keep parens, otherwise becomes (a / b) / c
    let source = r#"pipeline default(task) {
  let x = a / (b / c)
}"#;
    let result = format_source(source).unwrap();
    assert!(
        result.contains("a / (b / c)"),
        "Expected parens for right-child division, got:\n{result}"
    );
    assert_roundtrip(source);
}

#[test]
fn test_exponentiation_formats_with_natural_right_associativity() {
    let source = r#"pipeline default(task) {
  let x = a ** b ** c
}"#;
    let result = format_source(source).unwrap();
    assert!(
        result.contains("a ** b ** c"),
        "Expected natural right-associative exponentiation, got:\n{result}"
    );
    assert_roundtrip(source);
}

#[test]
fn test_exponentiation_preserves_left_grouping_when_forced() {
    let source = r#"pipeline default(task) {
  let x = (a ** b) ** c
}"#;
    let result = format_source(source).unwrap();
    assert!(
        result.contains("(a ** b) ** c"),
        "Expected parens preserved for left-grouped exponentiation, got:\n{result}"
    );
    assert_roundtrip(source);
}

#[test]
fn test_exponentiation_binds_tighter_than_multiplication() {
    let source = r#"pipeline default(task) {
  let x = a * b ** c
}"#;
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
    let source = r#"pipeline default(task) {
  let x = a * (b + c)
}"#;
    let result = format_source(source).unwrap();
    assert!(result.contains("a * (b + c)"), "got:\n{result}");
    assert_roundtrip(source);
}

#[test]
fn test_no_unnecessary_parens_same_op() {
    // a + b + c — all same associative op, no parens needed
    let source = r#"pipeline default(task) {
  let x = a + b + c
}"#;
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
    let source = r#"pipeline default(task) {
  let x = a + (b + c)
}"#;
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
    let source = r#"pipeline default(task) {
  let x = a * (b * c)
}"#;
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
    let source = r#"pipeline default(task) {
  let x = a ?? (b ?? c)
}"#;
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
    let source = r#"pipeline default(task) {
  let x = a + b * c
}"#;
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
    let source = r#"pipeline default(task) {
  fn process(
    first_really_long_param_name: string,
    second_really_long_param_name: int,
    third_really_long_param_name: bool,
  ) {
    log(1)
  }
}"#;
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
    let source = r#"pipeline default(task) {
  let x = first_really_long_variable_name + second_really_long_variable_name \
    == third_really_long_variable_name
}"#;
    assert_roundtrip(source);
}

// --- Custom line width ---

fn fmt_opts(source: &str, line_width: usize) -> String {
    let opts = FmtOptions {
        line_width,
        separator_width: 80,
        auto_insert_separators: false,
    };
    format_source_opts(source, &opts).unwrap()
}

#[test]
fn test_custom_line_width_wraps_earlier() {
    // "really_long_function_name(" = 26 chars; "alpha, beta, gamma" = 18; 26+18+1 = 45 > 40
    let source = r#"pipeline default() {
  let x = really_long_function_name(alpha, beta, gamma)
}"#;
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
    let source = r#"pipeline default() {
  let x = really_long_function_name(alpha, beta, gamma)
}"#;
    let result = format_source(source).unwrap();
    assert!(
        result.contains("really_long_function_name(alpha, beta, gamma)"),
        "Should stay inline at width 100, got:\n{result}"
    );
}

#[test]
fn test_custom_line_width_wraps_list() {
    // "item_one, item_two, item_three, item_four" = 41 chars; with "[" prefix=1 → 42+1 > 40
    let source = r#"pipeline default() {
  let x = [item_one, item_two, item_three, item_four]
}"#;
    let result = fmt_opts(source, 40);
    assert!(
        result.contains("[\n"),
        "Expected wrapped list at width 40, got:\n{result}"
    );
}

#[test]
fn test_custom_line_width_stays_inline_when_fits() {
    let source = r#"pipeline default() {
  let x = foo(a, b)
}"#;
    let result = fmt_opts(source, 40);
    assert!(
        result.contains("foo(a, b)"),
        "Short call should stay inline at width 40, got:\n{result}"
    );
}

#[test]
fn test_default_opts_matches_format_source() {
    let source = r#"pipeline default() {
  let x = compute(some_arg, another_arg)
}"#;
    let default_result = format_source(source).unwrap();
    let opts_result = format_source_opts(source, &FmtOptions::default()).unwrap();
    assert_eq!(default_result, opts_result);
}

#[test]
fn test_custom_line_width_wraps_fn_params() {
    // "  fn process(" = 14 chars; "input: string, options: dict" = 28; 14+28+1 = 43 > 40
    let source = r#"pipeline default() {
  fn process(input: string, options: dict) -> string {
    return input
  }
}"#;
    let result = fmt_opts(source, 40);
    assert!(
        result.contains("fn process(\n"),
        "Expected wrapped fn params at width 40, got:\n{result}"
    );
}

#[test]
fn test_custom_line_width_idempotent() {
    let source = r#"pipeline default() {
  let x = really_long_function_name(alpha, beta, gamma)
}"#;
    let opts = FmtOptions {
        line_width: 40,
        separator_width: 80,
        auto_insert_separators: false,
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
    let source = r#"pipeline default(task) {
  let x = (a + b).length
}"#;
    let result = format_source(source).unwrap();
    assert!(
        result.contains("(a + b).length"),
        "Expected parens preserved for + as property access object, got:\n{result}"
    );
    assert_roundtrip(source);
}

#[test]
fn test_parens_binary_op_subscript() {
    let source = r#"pipeline default(task) {
  let x = (a ?? b)[0]
}"#;
    let result = format_source(source).unwrap();
    assert!(
        result.contains("(a ?? b)[0]"),
        "Expected parens preserved for ?? as subscript object, got:\n{result}"
    );
    assert_roundtrip(source);
}

#[test]
fn test_parens_unary_op_method_call() {
    let source = r#"pipeline default(task) {
  let x = (-a).abs()
}"#;
    let result = format_source(source).unwrap();
    assert!(
        result.contains("(-a).abs()"),
        "Expected parens for unary-op as method call object, got:\n{result}"
    );
    assert_roundtrip(source);
}

#[test]
fn test_parens_unary_op_property_access() {
    let source = r#"pipeline default(task) {
  let x = (!flag).description
}"#;
    let result = format_source(source).unwrap();
    assert!(
        result.contains("(!flag).description"),
        "Expected parens for unary-op as property access object, got:\n{result}"
    );
    assert_roundtrip(source);
}

#[test]
fn test_parens_ternary_method_call() {
    let source = r#"pipeline default(task) {
  let x = (a ? b : c).method()
}"#;
    let result = format_source(source).unwrap();
    assert!(
        result.contains("(a ? b : c).method()"),
        "Expected parens for ternary as method call object, got:\n{result}"
    );
    assert_roundtrip(source);
}

#[test]
fn test_parens_unary_binary_operand() {
    let source = r#"pipeline default(task) {
  let x = !(a + b)
}"#;
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
    let source = r#"pipeline default(task) {
  let x = !(a ?? b).trim()
}"#;
    let result = format_source(source).unwrap();
    assert!(
        result.contains("!(a ?? b).trim()"),
        "Expected parens preserved in chained unary+method+binary, got:\n{result}"
    );
    assert_roundtrip(source);
}

#[test]
fn test_parens_chained_postfix_on_binary() {
    let source = r#"pipeline default(task) {
  let x = (a + b)[0].method()
}"#;
    let result = format_source(source).unwrap();
    assert!(
        result.contains("(a + b)[0].method()"),
        "Expected parens for chained postfix on binary op, got:\n{result}"
    );
    assert_roundtrip(source);
}

#[test]
fn test_parens_binary_op_optional_method() {
    let source = r#"pipeline default(task) {
  let x = (a ?? b)?.method()
}"#;
    let result = format_source(source).unwrap();
    assert!(
        result.contains("(a ?? b)?.method()"),
        "Expected parens for ?? as optional method call object, got:\n{result}"
    );
    assert_roundtrip(source);
}

#[test]
fn test_parens_binary_op_optional_property() {
    let source = r#"pipeline default(task) {
  let x = (a ?? b)?.length
}"#;
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
    let source = r#"pipeline default(task) {
  fn add(a: int, b: int) -> int {
    return a + b
  }
}"#;
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
fn test_doc_comment_triple_slash_multiline() {
    let source =
        "/// First line.\n/// Second line.\npub fn exposed() -> string {\n  return \"x\"\n}\n";
    let result = format_source(source).unwrap();
    assert!(
        result.contains("/**\n * First line.\n * Second line.\n */"),
        "expected canonical multi-line /** */ block, got:\n{result}"
    );
    assert!(
        !result.contains("///"),
        "formatter should not emit `///` after normalization, got:\n{result}"
    );
}

#[test]
fn test_doc_comment_triple_slash_compact_one_liner() {
    let source = "/// Short.\npub fn exposed() -> string {\n  return \"x\"\n}\n";
    let result = format_source(source).unwrap();
    assert!(
        result.contains("/** Short. */"),
        "expected compact one-liner doc comment, got:\n{result}"
    );
}

#[test]
fn test_doc_comment_existing_block_is_canonicalized() {
    let source = "/** messy\n   alignment */\npub fn exposed() -> string {\n  return \"x\"\n}\n";
    let result = format_source(source).unwrap();
    assert!(
        result.contains("/**\n * messy\n * alignment\n */"),
        "expected canonical multi-line shape, got:\n{result}"
    );
}

#[test]
fn test_plain_double_slash_comment_preserved_verbatim() {
    let source = "// plain comment\npub fn exposed() -> string {\n  return \"x\"\n}\n";
    let result = format_source(source).unwrap();
    assert!(
        result.contains("// plain comment"),
        "plain // comment should be preserved verbatim, got:\n{result}"
    );
    assert!(
        !result.contains("/**"),
        "formatter should not convert // to /** */ (that's the linter's job), got:\n{result}"
    );
}

#[test]
fn test_doc_comment_inside_impl_block() {
    let source =
        "impl Foo {\n  /// Inner method.\n  pub fn bar() -> string {\n    return \"x\"\n  }\n}\n";
    let result = format_source(source).unwrap();
    assert!(
        result.contains("  /** Inner method. */"),
        "doc comment inside impl body should be normalized, got:\n{result}"
    );
    assert!(
        !result.contains("///"),
        "no `///` should remain after formatting, got:\n{result}"
    );
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
fn test_doc_comment_glued_to_item_blank_line_above() {
    let source =
        "fn first() -> int {\n  return 1\n}\n/// Second docs.\n/// More.\nfn second() -> int {\n  return 2\n}\n";
    let result = format_source(source).unwrap();
    // Blank line above the doc block; doc block glued to fn second.
    assert!(
        result.contains("}\n\n/**\n * Second docs.\n * More.\n */\nfn second"),
        "doc block should have blank line above and be glued to item, got:\n{result}"
    );
    let result2 = format_source(&result).unwrap();
    assert_eq!(
        result, result2,
        "formatter is not idempotent with doc comments between items"
    );
}

// ---------------------------------------------------------------------------
// Section-header canonicalization
// ---------------------------------------------------------------------------

fn canonical_bar() -> String {
    // Default separator_width is 80 → 77 dashes after `// `.
    let dashes: String = "-".repeat(77);
    format!("// {dashes}")
}

#[test]
fn test_section_header_three_line_canonical_passthrough() {
    let bar = canonical_bar();
    let source = format!(
        "fn a() -> int {{\n  return 1\n}}\n{bar}\n// Helpers\n{bar}\nfn b() -> int {{\n  return 2\n}}\n"
    );
    let result = format_source(&source).unwrap();
    let expected = format!(
        "fn a() -> int {{\n  return 1\n}}\n\n{bar}\n// Helpers\n{bar}\n\nfn b() -> int {{\n  return 2\n}}\n"
    );
    assert_eq!(result, expected, "canonical 3-line header not passthrough");
    let result2 = format_source(&result).unwrap();
    assert_eq!(result, result2, "3-line header not idempotent");
}

#[test]
fn test_section_header_three_line_short_bars_normalized() {
    let source =
        "fn a() -> int { return 1 }\n// ----\n// Helpers\n// ----\nfn b() -> int { return 2 }\n";
    let result = format_source(source).unwrap();
    let bar = canonical_bar();
    assert!(
        result.contains(&format!("{bar}\n// Helpers\n{bar}")),
        "short bars should normalize to separator_width, got:\n{result}"
    );
}

#[test]
fn test_section_header_one_line_bar_normalized() {
    let source = "fn a() -> int { return 1 }\n// ----\nfn b() -> int { return 2 }\n";
    let result = format_source(source).unwrap();
    let bar = canonical_bar();
    assert!(
        result.contains(&format!("\n{bar}\n")),
        "one-line bar should normalize, got:\n{result}"
    );
    // Pure bars stay one-liner (no title promotion).
    assert!(
        !result.contains("// Helpers"),
        "pure bar must not gain a title"
    );
}

#[test]
fn test_section_header_one_line_bar_with_title_promoted() {
    let source = "fn a() -> int { return 1 }\n// ---- Helpers ----\nfn b() -> int { return 2 }\n";
    let result = format_source(source).unwrap();
    let bar = canonical_bar();
    assert!(
        result.contains(&format!("{bar}\n// Helpers\n{bar}")),
        "one-liner with title should promote to 3-line form, got:\n{result}"
    );
}

#[test]
fn test_section_header_blank_lines_above_and_below() {
    let source = "fn a() -> int {\n  return 1\n}\n// ----\n// Helpers\n// ----\nfn b() -> int {\n  return 2\n}\n";
    let result = format_source(source).unwrap();
    let bar = canonical_bar();
    // Expect: prev fn close, blank, header, blank, next fn.
    let header = format!("{bar}\n// Helpers\n{bar}");
    let expected_window = format!("}}\n\n{header}\n\nfn b");
    assert!(
        result.contains(&expected_window),
        "expected blank lines above and below section header, got:\n{result}"
    );
}

#[test]
fn test_section_header_respects_custom_separator_width() {
    let opts = FmtOptions {
        line_width: 100,
        separator_width: 40,
        auto_insert_separators: false,
    };
    let source = "fn a() -> int { return 1 }\n// ----\nfn b() -> int { return 2 }\n";
    let result = format_source_opts(source, &opts).unwrap();
    let dashes: String = "-".repeat(37);
    let bar = format!("// {dashes}");
    assert!(
        result.contains(&bar),
        "separator should match separator_width=40, got:\n{result}"
    );
}

#[test]
fn test_multiline_string_preserves_indent() {
    let source = "pipeline test(task) {\n  let g = \"\"\"\n    hello world\n    second line\n    \"\"\"\n  log(g)\n}\n";
    let out = format_source(source).unwrap();
    assert!(
        out.contains("    hello world"),
        "multiline string body should keep indent, got:\n{out}"
    );
    assert!(
        out.contains("    \"\"\""),
        "closing \"\"\" should be indented one level deeper than the let, got:\n{out}"
    );
    let out2 = format_source(&out).unwrap();
    assert_eq!(
        out, out2,
        "formatter should be idempotent on multiline strings"
    );
}

#[test]
fn test_multiline_interpolated_string_preserves_indent() {
    let source = "pipeline test(task) {\n  let name = \"x\"\n  let g = \"\"\"\n    hi ${name}\n    \"\"\"\n  log(g)\n}\n";
    let out = format_source(source).unwrap();
    assert!(
        out.contains("    hi ${name}"),
        "interpolated body should keep indent, got:\n{out}"
    );
    let out2 = format_source(&out).unwrap();
    assert_eq!(out, out2, "formatter should be idempotent");
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
