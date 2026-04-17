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
}
