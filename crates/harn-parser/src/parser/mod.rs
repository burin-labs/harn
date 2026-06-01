mod decls;
mod error;
mod expressions;
mod patterns;
mod state;
mod statements;
mod types;

pub use error::ParserError;
pub use state::Parser;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::*;
    use harn_lexer::Lexer;

    fn parse_source(source: &str) -> Result<Vec<SNode>, ParserError> {
        let mut lexer = Lexer::new(source);
        let tokens = lexer.tokenize().unwrap();
        let mut parser = Parser::new(tokens);
        parser.parse()
    }

    #[test]
    fn parses_bare_and_keyed_mutex() {
        fn first_mutex_has_key(source: &str) -> Option<bool> {
            let program = parse_source(source).unwrap();
            let mut has_key = None;
            crate::visit::walk_program(&program, &mut |node| {
                if has_key.is_none() {
                    if let Node::MutexBlock { key, .. } = &node.node {
                        has_key = Some(key.is_some());
                    }
                }
            });
            has_key
        }

        assert_eq!(
            first_mutex_has_key("pipeline default(task) { mutex { log(1) } }"),
            Some(false),
            "bare `mutex {{}}` should parse with no key"
        );
        assert_eq!(
            first_mutex_has_key("pipeline default(task) { mutex(\"acct\") { log(1) } }"),
            Some(true),
            "`mutex(resource) {{}}` should parse with a key"
        );
    }

    #[test]
    fn parser_reports_expression_nesting_depth_limit() {
        let depth = state::MAX_NESTING_DEPTH + 1;
        let source = format!("let x = {}0{}", "(".repeat(depth), ")".repeat(depth));

        let Err(err) = parse_source(&source) else {
            panic!("expected parser depth limit");
        };
        let message = err.to_string();

        assert!(message.contains("parser nesting depth within"));
        assert!(message.contains(&format!("{} levels", state::MAX_NESTING_DEPTH)));
    }

    #[test]
    fn parser_reports_unary_nesting_depth_limit() {
        let depth = state::MAX_NESTING_DEPTH + 1;
        let source = format!("let x = {}true", "!".repeat(depth));

        let Err(err) = parse_source(&source) else {
            panic!("expected parser depth limit");
        };
        let message = err.to_string();

        assert!(message.contains("parser nesting depth within"));
        assert!(message.contains(&format!("{} levels", state::MAX_NESTING_DEPTH)));
    }

    #[test]
    fn parser_reports_block_nesting_depth_limit() {
        let depth = state::MAX_NESTING_DEPTH + 1;
        let mut source = String::new();
        for _ in 0..depth {
            source.push_str("if true {\n");
        }
        source.push_str("let x = 1\n");
        for _ in 0..depth {
            source.push_str("}\n");
        }

        let Err(err) = parse_source(&source) else {
            panic!("expected parser depth limit");
        };
        let message = err.to_string();

        assert!(message.contains("parser nesting depth within"));
        assert!(message.contains(&format!("{} levels", state::MAX_NESTING_DEPTH)));
    }

    #[test]
    fn parser_reports_list_literal_nesting_depth_limit() {
        let depth = state::MAX_NESTING_DEPTH + 1;
        let source = format!("let x = {}0{}", "[".repeat(depth), "]".repeat(depth));

        let Err(err) = parse_source(&source) else {
            panic!("expected parser depth limit");
        };
        let message = err.to_string();

        assert!(message.contains("parser nesting depth within"));
        assert!(message.contains(&format!("{} levels", state::MAX_NESTING_DEPTH)));
    }

    #[test]
    fn parser_reports_type_nesting_depth_limit() {
        let depth = state::MAX_NESTING_DEPTH + 1;
        let source = format!("let x: {}int{} = []", "[".repeat(depth), "]".repeat(depth));

        let Err(err) = parse_source(&source) else {
            panic!("expected parser depth limit");
        };
        let message = err.to_string();

        assert!(message.contains("parser nesting depth within"));
        assert!(message.contains(&format!("{} levels", state::MAX_NESTING_DEPTH)));
    }

    #[test]
    fn parses_scoped_selective_import_as_slash_path() {
        let nodes =
            parse_source("pub import std::personas::prelude::{verify_then_act, bounded_loop}")
                .expect("scoped selective import parses");

        match &nodes[0].node {
            Node::SelectiveImport {
                path,
                names,
                is_pub,
            } => {
                assert_eq!(path, "std/personas/prelude");
                assert_eq!(names.len(), 2);
                assert_eq!(names[0], "verify_then_act");
                assert_eq!(names[1], "bounded_loop");
                assert!(*is_pub);
            }
            other => panic!("expected selective import, got {other:?}"),
        }
    }

    #[test]
    fn parses_match_expression_with_let_in_arm_body() {
        let source = r"
pipeline p() {
  let x = match 1 {
    1 -> {
      let a = 1
      a
    }
    _ -> { 0 }
  }
}
";

        assert!(parse_source(source).is_ok());
    }

    #[test]
    fn parses_line_leading_infix_continuation_operators() {
        let source = r#"
pipeline p() {
  let fallback = nil
    ?? "fallback"
  let same = 1
    == 1
  let smaller = 1
    < 2
}
"#;

        assert!(parse_source(source).is_ok());
    }

    #[test]
    fn parses_line_trailing_infix_continuation_operators() {
        // Sister case to the leading-operator test above. The trailing form
        // (operator at the end of the previous line, right operand on the
        // continuation line) used to error with "expected expression, found
        // \\n" because each binary-op parser advanced past the operator and
        // immediately tried to parse the right operand without skipping the
        // newline that followed.
        //
        // We assert against a raw source string here rather than a
        // conformance fixture because `harn fmt` canonicalizes the trailing
        // form back to the leading form on save, so a fixture round-trips
        // before the test ever validates the trailing form.
        let source = r#"
pipeline p() {
  let nc = nil ??
    "fallback"
  let conj = true &&
    true
  let disj = false ||
    true
  let same = 1 ==
    1
  let diff = 1 !=
    2
  let lt = 1 <
    2
  let gte = 2 >=
    2
  let sum = 1 +
    2
  let mul = 4 *
    2
  let div = 8 /
    2
  let pow = 2 **
    3
  let chain = nil ??
    nil ??
    "chain"
  let piped = 1 |>
    to_string
}
"#;

        assert!(parse_source(source).is_ok());
    }

    #[test]
    fn parses_list_literals_as_ternary_branches_without_breaking_optional_subscript() {
        let source = r#"
let repo_args = repo ? ["--repo", repo] : []
let selected = repo ? ["--repo", repo][0] : "none"
let nested = true ? [1, true ? [2] : []] : []
let first = xs?[0]
"#;

        let program = parse_source(source).expect("should parse");
        let Node::LetBinding { value, .. } = &program[0].node else {
            panic!("expected let binding");
        };
        let Node::Ternary {
            true_expr,
            false_expr,
            ..
        } = &value.node
        else {
            panic!("expected ternary");
        };
        assert!(matches!(&true_expr.node, Node::ListLiteral(items) if items.len() == 2));
        assert!(matches!(&false_expr.node, Node::ListLiteral(items) if items.is_empty()));

        let Node::LetBinding { value, .. } = &program[1].node else {
            panic!("expected let binding");
        };
        assert!(matches!(&value.node, Node::Ternary { true_expr, .. }
            if matches!(&true_expr.node, Node::SubscriptAccess { object, .. }
                if matches!(&object.node, Node::ListLiteral(_)))));

        let Node::LetBinding { value, .. } = &program[2].node else {
            panic!("expected let binding");
        };
        assert!(matches!(&value.node, Node::Ternary { true_expr, .. }
            if matches!(&true_expr.node, Node::ListLiteral(_))));

        let Node::LetBinding { value, .. } = &program[3].node else {
            panic!("expected let binding");
        };
        assert!(matches!(
            &value.node,
            Node::OptionalSubscriptAccess { object, index }
                if matches!(&object.node, Node::Identifier(name) if name == "xs")
                    && matches!(&index.node, Node::IntLiteral(0))
        ));
    }

    #[test]
    fn parses_multiline_ternary_with_leading_or_trailing_operators() {
        // Previously the parser rejected ternary wraps that put `?` or `:`
        // at the end of the previous line: a `?` at end-of-line was
        // misclassified as a postfix-try operator because
        // `question_starts_ternary_branch` only looked at the immediate
        // next token (a newline) instead of the next non-newline token,
        // and `parse_ternary` itself did not skip newlines around `?`
        // and `:`. We assert against raw source here rather than a
        // conformance fixture because `harn fmt` collapses any short
        // multi-line ternary back onto one line.
        let cases = [
            // operator-leading wrap
            "let a = true\n  ? 1\n  : 2\n",
            // operator-trailing wrap
            "let b = true ?\n  1 :\n  2\n",
            // wrap only on the false branch
            "let c = true ? 1\n  : 2\n",
            // nested ternary with operator-leading wrap
            "let d = true\n  ? false\n    ? 10\n    : 20\n  : 30\n",
        ];
        for source in &cases {
            let wrapped = format!("pipeline p() {{\n  {}\n}}\n", source.replace('\n', "\n  "));
            let program = parse_source(&wrapped)
                .unwrap_or_else(|e| panic!("failed to parse multiline ternary: {source:?}: {e}"));
            // Walk into the pipeline body and find a let binding whose value
            // is a Ternary.
            let Node::Pipeline { body, .. } = &program[0].node else {
                panic!("expected pipeline");
            };
            let Node::LetBinding { value, .. } = &body[0].node else {
                panic!("expected let binding for {source:?}");
            };
            assert!(
                matches!(&value.node, Node::Ternary { .. }),
                "expected Ternary node for source {source:?}, got {:?}",
                value.node
            );
        }
    }

    #[test]
    fn parses_public_declarations_and_generic_interfaces() {
        let source = r"
pub pipeline build(task) extends base {
  return
}

pub enum Result {
  Ok(value: string),
  Err(message: string, code: int),
}

pub struct Config {
  host: string
  port?: int
}

interface Repository<T> {
  type Item
  fn get(id: string) -> T
  fn map<U>(value: T, f: fn(T) -> U) -> U
}
";

        let program = parse_source(source).expect("should parse");
        assert!(matches!(
            &program[0].node,
            Node::Pipeline {
                is_pub: true,
                extends: Some(base),
                ..
            } if base == "base"
        ));
        assert!(matches!(
            &program[1].node,
            Node::EnumDecl {
                is_pub: true,
                type_params,
                ..
            } if type_params.is_empty()
        ));
        assert!(matches!(
            &program[2].node,
            Node::StructDecl {
                is_pub: true,
                type_params,
                ..
            } if type_params.is_empty()
        ));
        assert!(matches!(
            &program[3].node,
            Node::InterfaceDecl {
                type_params,
                associated_types,
                methods,
                ..
            }
                if type_params.len() == 1
                    && associated_types.len() == 1
                    && methods.len() == 2
                    && methods[1].type_params.len() == 1
        ));
    }

    #[test]
    fn parses_durable_persona_annotation_values() {
        let source = r#"
@persona(
  triggers: [github.pr_opened, schedule("*/30 * * * *")],
  tools: [github, ci],
  autonomy: act_with_approval,
  budget: {daily_usd: 20, frontier_escalations: 3},
  handoffs: [review_captain],
)
fn merge_captain(ctx) {
  return ctx
}
"#;

        let program = parse_source(source).expect("should parse persona annotations");
        let Node::AttributedDecl { attributes, inner } = &program[0].node else {
            panic!("expected attributed decl");
        };
        assert_eq!(attributes[0].name, "persona");
        assert!(matches!(inner.node, Node::FnDecl { .. }));
        let triggers = attributes[0].named_arg("triggers").expect("triggers arg");
        let Node::ListLiteral(items) = &triggers.node else {
            panic!("expected trigger list");
        };
        assert!(matches!(&items[0].node, Node::Identifier(name) if name == "github.pr_opened"));
        assert!(matches!(
            &items[1].node,
            Node::FunctionCall { name, args, .. } if name == "schedule" && args.len() == 1
        ));
    }

    #[test]
    fn parses_step_annotation_values() {
        let source = r#"
@step(name: "plan", model: "gpt-5.4-mini", approval: required, receipt: audit, error_boundary: escalate, retry: {max_attempts: 2})
fn plan_step(ctx) {
  return ctx
}
"#;

        let program = parse_source(source).expect("should parse step annotations");
        let Node::AttributedDecl { attributes, inner } = &program[0].node else {
            panic!("expected attributed decl");
        };
        assert_eq!(attributes[0].name, "step");
        assert!(matches!(inner.node, Node::FnDecl { .. }));
        let retry = attributes[0].named_arg("retry").expect("retry arg");
        let Node::DictLiteral(entries) = &retry.node else {
            panic!("expected retry dict");
        };
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn parses_generic_structs_and_enums() {
        let source = r"
struct Pair<A, B> {
  first: A
  second: B
}

enum Option<T> {
  Some(value: T)
  None
}
";

        let program = parse_source(source).expect("should parse");
        assert!(matches!(
            &program[0].node,
            Node::StructDecl { type_params, .. } if type_params.len() == 2
        ));
        assert!(matches!(
            &program[1].node,
            Node::EnumDecl { type_params, .. } if type_params.len() == 1
        ));
    }

    #[test]
    fn parses_explicit_generic_call_type_args() {
        let source = r#"
pipeline test(task) {
  let n = identity<int>(42)
  let words = identity<[string]>(["a"])
}
"#;

        let program = parse_source(source).expect("should parse");
        let pipeline = program
            .iter()
            .find(|node| matches!(node.node, Node::Pipeline { .. }))
            .expect("pipeline node");
        let body = match &pipeline.node {
            Node::Pipeline { body, .. } => body,
            _ => unreachable!(),
        };
        assert!(matches!(
            &body[0].node,
            Node::LetBinding { value, .. }
                if matches!(
                    &value.node,
                    Node::FunctionCall { name, type_args, .. }
                        if name == "identity" && type_args == &vec![TypeExpr::Named("int".into())]
                )
        ));
        assert!(matches!(
            &body[1].node,
            Node::LetBinding { value, .. }
                if matches!(
                    &value.node,
                    Node::FunctionCall { name, type_args, .. }
                        if name == "identity"
                            && type_args == &vec![TypeExpr::List(Box::new(TypeExpr::Named("string".into())))]
                )
        ));
    }

    #[test]
    fn rejects_empty_generic_call_type_args() {
        let source = r"
pipeline test(task) {
  let n = identity<>(42)
}
";

        assert!(parse_source(source).is_err());
    }

    #[test]
    fn parses_struct_literal_syntax_for_known_structs() {
        let source = r"
struct Point {
  x: int
  y: int
}

pipeline test(task) {
  let point = Point { x: 3, y: 4 }
}
";

        let program = parse_source(source).expect("should parse");
        let pipeline = program
            .iter()
            .find(|node| matches!(node.node, Node::Pipeline { .. }))
            .expect("pipeline node");
        let body = match &pipeline.node {
            Node::Pipeline { body, .. } => body,
            _ => unreachable!(),
        };
        assert!(matches!(
            &body[0].node,
            Node::LetBinding { value, .. }
                if matches!(
                    value.node,
                    Node::StructConstruct { ref struct_name, ref fields }
                        if struct_name == "Point" && fields.len() == 2
                )
        ));
    }

    #[test]
    fn parses_struct_literal_syntax_without_prior_struct_decl() {
        let source = r"
pipeline test(task) {
  let point = Point { x: 3, y: 4 }
}
";

        let program = parse_source(source).expect("should parse");
        let pipeline = program
            .iter()
            .find(|node| matches!(node.node, Node::Pipeline { .. }))
            .expect("pipeline node");
        let body = match &pipeline.node {
            Node::Pipeline { body, .. } => body,
            _ => unreachable!(),
        };
        assert!(matches!(
            &body[0].node,
            Node::LetBinding { value, .. }
                if matches!(
                    value.node,
                    Node::StructConstruct { ref struct_name, ref fields }
                        if struct_name == "Point" && fields.len() == 2
                )
        ));
    }

    #[test]
    fn parses_exponentiation_as_right_associative() {
        let mut lexer = Lexer::new("a ** b ** c");
        let tokens = lexer.tokenize().expect("tokens");
        let mut parser = Parser::new(tokens);
        let expr = parser.parse_single_expression().expect("expression");

        assert!(matches!(
            expr.node,
            Node::BinaryOp { ref op, ref left, ref right }
                if op == "**"
                    && matches!(left.node, Node::Identifier(ref name) if name == "a")
                    && matches!(
                        right.node,
                        Node::BinaryOp { ref op, ref left, ref right }
                            if op == "**"
                                && matches!(left.node, Node::Identifier(ref name) if name == "b")
                                && matches!(right.node, Node::Identifier(ref name) if name == "c")
                    )
        ));
    }

    #[test]
    fn parses_exponentiation_tighter_than_multiplication() {
        let mut lexer = Lexer::new("a * b ** c");
        let tokens = lexer.tokenize().expect("tokens");
        let mut parser = Parser::new(tokens);
        let expr = parser.parse_single_expression().expect("expression");

        assert!(matches!(
            expr.node,
            Node::BinaryOp { ref op, ref left, ref right }
                if op == "*"
                    && matches!(left.node, Node::Identifier(ref name) if name == "a")
                    && matches!(
                        right.node,
                        Node::BinaryOp { ref op, ref left, ref right }
                            if op == "**"
                                && matches!(left.node, Node::Identifier(ref name) if name == "b")
                                && matches!(right.node, Node::Identifier(ref name) if name == "c")
                    )
        ));
    }

    #[test]
    fn parses_semicolon_separated_statements_in_block() {
        let source = r"
pipeline p(task) {
  let x = 1; let y = 2
}
";

        let program = parse_source(source).expect("should parse");
        let pipeline = program
            .iter()
            .find(|node| matches!(node.node, Node::Pipeline { .. }))
            .expect("pipeline node");
        let body = match &pipeline.node {
            Node::Pipeline { body, .. } => body,
            _ => unreachable!(),
        };
        assert_eq!(body.len(), 2, "semicolon should separate block statements");
    }

    #[test]
    fn parses_semicolon_separated_top_level_items() {
        let source = r"fn first() {} ; fn second() {}";
        let program = parse_source(source).expect("should parse");
        assert_eq!(
            program.len(),
            2,
            "semicolon should separate top-level items"
        );
    }

    #[test]
    fn parses_return_and_yield_with_semicolon_terminators() {
        let source = r#"
fn generator() {
  yield; log("after yield")
}

pipeline p(task) {
  return; log("after return")
}
"#;

        let program = parse_source(source).expect("should parse");
        let generator = program
            .iter()
            .find(|node| matches!(&node.node, Node::FnDecl { name, .. } if name == "generator"))
            .expect("generator fn");
        let generator_body = match &generator.node {
            Node::FnDecl { body, .. } => body,
            _ => unreachable!(),
        };
        assert_eq!(generator_body.len(), 2);
        assert!(matches!(
            generator_body[0].node,
            Node::YieldExpr { value: None }
        ));

        let pipeline = program
            .iter()
            .find(|node| matches!(node.node, Node::Pipeline { .. }))
            .expect("pipeline node");
        let body = match &pipeline.node {
            Node::Pipeline { body, .. } => body,
            _ => unreachable!(),
        };
        assert_eq!(body.len(), 2);
        assert!(matches!(body[0].node, Node::ReturnStmt { value: None }));
    }

    #[test]
    fn parses_trailing_semicolons_before_brace_and_eof() {
        let block_source = r"
pipeline p(task) {
  log(1);
}
";
        let eof_source = r"fn only() {};";

        assert!(parse_source(block_source).is_ok());
        assert!(parse_source(eof_source).is_ok());
    }

    #[test]
    fn rejects_same_line_statements_without_separator() {
        let source = r"pipeline p(task) { let x = 1 let y = 2 }";
        let err = parse_source(source).expect_err("missing separator should fail");
        assert!(
            err.to_string().contains("separator"),
            "expected separator error, got: {err}"
        );
    }

    #[test]
    fn rejects_semicolon_before_else_and_catch() {
        let if_err = parse_source(r"pipeline p(task) { if true { log(1) }; else { log(2) } }")
            .expect_err("semicolon before else should fail");
        assert!(
            if_err.to_string().contains("separator") || if_err.to_string().contains("else"),
            "unexpected if error: {if_err}"
        );

        let try_err = parse_source(r"pipeline p(task) { try { log(1) }; catch { log(2) } }")
            .expect_err("semicolon before catch should fail");
        assert!(
            try_err.to_string().contains("separator") || try_err.to_string().contains("catch"),
            "unexpected try error: {try_err}"
        );
    }

    #[test]
    fn rejects_empty_statement_from_double_semicolon() {
        let source = r"pipeline p(task) { log(1);; log(2) }";
        assert!(
            parse_source(source).is_err(),
            "double semicolon should fail"
        );
    }

    fn parse_let_type(source: &str) -> TypeExpr {
        let nodes = parse_source(source).expect("source parses");
        let pipeline = nodes.first().expect("at least one decl");
        let body = match &pipeline.node {
            Node::Pipeline { body, .. } => body,
            other => panic!("expected pipeline, got {other:?}"),
        };
        match &body[0].node {
            Node::LetBinding { type_ann, .. } => type_ann.clone().expect("type annotation present"),
            other => panic!("expected let binding, got {other:?}"),
        }
    }

    #[test]
    fn optional_postfix_is_alias_for_union_with_nil() {
        let sugar = parse_let_type("pipeline p(task) { let x: int? = nil }");
        let unsugar = parse_let_type("pipeline p(task) { let x: int | nil = nil }");
        assert_eq!(sugar, unsugar);
        assert_eq!(
            sugar,
            TypeExpr::Union(vec![
                TypeExpr::Named("int".into()),
                TypeExpr::Named("nil".into()),
            ])
        );
    }

    #[test]
    fn optional_flattens_in_outer_union() {
        let ty = parse_let_type("pipeline p(task) { let x: int | string? = nil }");
        // `int | string?` flattens to `int | string | nil` rather than
        // a nested `int | (string | nil)`.
        assert_eq!(
            ty,
            TypeExpr::Union(vec![
                TypeExpr::Named("int".into()),
                TypeExpr::Named("string".into()),
                TypeExpr::Named("nil".into()),
            ])
        );
    }

    #[test]
    fn optional_dedupes_repeated_nil_arms() {
        let ty = parse_let_type("pipeline p(task) { let x: int? | nil = nil }");
        assert_eq!(
            ty,
            TypeExpr::Union(vec![
                TypeExpr::Named("int".into()),
                TypeExpr::Named("nil".into()),
            ])
        );
    }

    #[test]
    fn optional_binds_tighter_than_intersection() {
        // `A & B?` parses as `A & (B | nil)`, not `(A & B) | nil`.
        let ty = parse_let_type("pipeline p(task) { let x: {a: int} & {b: string}? = nil }");
        match ty {
            TypeExpr::Intersection(parts) => {
                assert_eq!(parts.len(), 2);
                assert!(matches!(parts[0], TypeExpr::Shape(_)));
                let TypeExpr::Union(members) = &parts[1] else {
                    panic!("expected second arm to be union, got {:?}", parts[1]);
                };
                assert_eq!(members.len(), 2);
                assert!(matches!(members[1], TypeExpr::Named(ref n) if n == "nil"));
            }
            other => panic!("expected intersection, got {other:?}"),
        }
    }
}
