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
    fn parses_match_expression_with_let_in_arm_body() {
        let source = r#"
pipeline p() {
  let x = match 1 {
    1 -> {
      let a = 1
      a
    }
    _ -> { 0 }
  }
}
"#;

        assert!(parse_source(source).is_ok());
    }

    #[test]
    fn parses_public_declarations_and_generic_interfaces() {
        let source = r#"
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
"#;

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
    fn parses_generic_structs_and_enums() {
        let source = r#"
struct Pair<A, B> {
  first: A
  second: B
}

enum Option<T> {
  Some(value: T)
  None
}
"#;

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
    fn parses_struct_literal_syntax_for_known_structs() {
        let source = r#"
struct Point {
  x: int
  y: int
}

pipeline test(task) {
  let point = Point { x: 3, y: 4 }
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
        let source = r#"
pipeline p(task) {
  let x = 1; let y = 2
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
        assert_eq!(body.len(), 2, "semicolon should separate block statements");
    }

    #[test]
    fn parses_semicolon_separated_top_level_items() {
        let source = r#"fn first() {} ; fn second() {}"#;
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
        let block_source = r#"
pipeline p(task) {
  log(1);
}
"#;
        let eof_source = r#"fn only() {};"#;

        assert!(parse_source(block_source).is_ok());
        assert!(parse_source(eof_source).is_ok());
    }

    #[test]
    fn rejects_same_line_statements_without_separator() {
        let source = r#"pipeline p(task) { let x = 1 let y = 2 }"#;
        let err = parse_source(source).expect_err("missing separator should fail");
        assert!(
            err.to_string().contains("separator"),
            "expected separator error, got: {err}"
        );
    }

    #[test]
    fn rejects_semicolon_before_else_and_catch() {
        let if_err = parse_source(r#"pipeline p(task) { if true { log(1) }; else { log(2) } }"#)
            .expect_err("semicolon before else should fail");
        assert!(
            if_err.to_string().contains("separator") || if_err.to_string().contains("else"),
            "unexpected if error: {if_err}"
        );

        let try_err = parse_source(r#"pipeline p(task) { try { log(1) }; catch { log(2) } }"#)
            .expect_err("semicolon before catch should fail");
        assert!(
            try_err.to_string().contains("separator") || try_err.to_string().contains("catch"),
            "unexpected try error: {try_err}"
        );
    }

    #[test]
    fn rejects_empty_statement_from_double_semicolon() {
        let source = r#"pipeline p(task) { log(1);; log(2) }"#;
        assert!(
            parse_source(source).is_err(),
            "double semicolon should fail"
        );
    }
}
