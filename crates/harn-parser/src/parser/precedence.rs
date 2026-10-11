use crate::ast::SNode;
use harn_lexer::{Span, TokenKind};

use super::error::ParserError;
use super::expression_nodes::{self as nodes, Expression};
use super::state::Parser;

impl Parser {
    /// Parse a single expression for a string interpolation hole.
    pub fn parse_single_expression(&mut self) -> Result<SNode, ParserError> {
        self.check_token_nesting_limit()?;
        self.skip_newlines();
        let expr = self.parse_expression()?;
        self.skip_newlines();
        if !self.is_at_end() {
            return Err(self.error("end of interpolated expression"));
        }
        Ok(expr)
    }

    pub(super) fn parse_nested_expression(
        &mut self,
        context: &'static str,
    ) -> Result<SNode, ParserError> {
        self.with_nesting(context, |parser| parser.parse_expression())
    }

    pub(super) fn parse_expression(&mut self) -> Result<SNode, ParserError> {
        self.skip_newlines();
        self.parse_pipe().map(|expr| *expr)
    }

    fn parse_pipe(&mut self) -> Result<Expression, ParserError> {
        self.parse_binary_level(Self::parse_range, &[(TokenKind::Pipe, "|>")])
    }

    fn parse_range(&mut self) -> Result<Expression, ParserError> {
        let left = self.parse_ternary()?;
        if !self.check(&TokenKind::To) {
            return Ok(left);
        }
        let start = left.span;
        self.advance();
        let right = self.parse_ternary()?;
        let inclusive = if self.check(&TokenKind::Exclusive) {
            self.advance();
            false
        } else {
            true
        };
        Ok(nodes::range(
            left,
            right,
            inclusive,
            Span::merge(start, self.prev_span()),
        ))
    }

    fn parse_ternary(&mut self) -> Result<Expression, ParserError> {
        let condition = self.parse_logical_or()?;
        // Postfix try has already been consumed. A remaining `?`, including
        // one across a newline, starts a ternary.
        if !self.check_skip_newlines(&TokenKind::Question) {
            return Ok(condition);
        }
        let start = condition.span;
        self.advance();
        self.skip_newlines();
        let true_expr = self.with_nesting("ternary expression", |parser| parser.parse_ternary())?;
        self.consume(&TokenKind::Colon, ":")?;
        self.skip_newlines();
        let false_expr =
            self.with_nesting("ternary expression", |parser| parser.parse_ternary())?;
        Ok(nodes::ternary(
            condition,
            true_expr,
            false_expr,
            Span::merge(start, self.prev_span()),
        ))
    }

    fn parse_logical_or(&mut self) -> Result<Expression, ParserError> {
        self.parse_binary_level(Self::parse_logical_and, &[(TokenKind::Or, "||")])
    }

    fn parse_logical_and(&mut self) -> Result<Expression, ParserError> {
        self.parse_binary_level(Self::parse_equality, &[(TokenKind::And, "&&")])
    }

    fn parse_equality(&mut self) -> Result<Expression, ParserError> {
        self.parse_binary_level(
            Self::parse_comparison,
            &[(TokenKind::Eq, "=="), (TokenKind::Neq, "!=")],
        )
    }

    fn parse_comparison(&mut self) -> Result<Expression, ParserError> {
        let mut left = self.parse_additive()?;
        loop {
            let op = if self.check_skip_newlines(&TokenKind::Lt) {
                "<"
            } else if self.check_skip_newlines(&TokenKind::Gt) {
                ">"
            } else if self.check_skip_newlines(&TokenKind::Lte) {
                "<="
            } else if self.check_skip_newlines(&TokenKind::Gte) {
                ">="
            } else if self.check(&TokenKind::In) {
                "in"
            } else if self.check_identifier("not") {
                let saved = self.pos;
                self.advance();
                if !self.check(&TokenKind::In) {
                    self.pos = saved;
                    break;
                }
                "not_in"
            } else {
                break;
            };
            let start = left.span;
            self.advance();
            self.skip_newlines();
            let right = self.parse_additive()?;
            left = nodes::binary(op, left, right, Span::merge(start, self.prev_span()));
        }
        Ok(left)
    }

    fn parse_additive(&mut self) -> Result<Expression, ParserError> {
        let mut left = self.parse_nil_coalescing()?;
        // A leading minus on a new line starts a separate unary expression.
        while self.check_skip_newlines(&TokenKind::Plus) || self.check(&TokenKind::Minus) {
            let op = if self.check(&TokenKind::Plus) {
                "+"
            } else {
                "-"
            };
            let start = left.span;
            self.advance();
            self.skip_newlines();
            let right = self.parse_nil_coalescing()?;
            left = nodes::binary(op, left, right, Span::merge(start, self.prev_span()));
        }
        Ok(left)
    }

    // `??` binds tighter than addition/comparison and looser than multiplication.
    fn parse_nil_coalescing(&mut self) -> Result<Expression, ParserError> {
        self.parse_binary_level(Self::parse_multiplicative, &[(TokenKind::NilCoal, "??")])
    }

    fn parse_multiplicative(&mut self) -> Result<Expression, ParserError> {
        self.parse_binary_level(
            Self::parse_unary_expression,
            &[
                (TokenKind::Star, "*"),
                (TokenKind::Slash, "/"),
                (TokenKind::Percent, "%"),
            ],
        )
    }

    fn parse_binary_level(
        &mut self,
        operand: fn(&mut Self) -> Result<Expression, ParserError>,
        operators: &[(TokenKind, &str)],
    ) -> Result<Expression, ParserError> {
        let mut left = operand(self)?;
        while let Some((_, op)) = operators
            .iter()
            .find(|(kind, _)| self.check_skip_newlines(kind))
        {
            let start = left.span;
            self.advance();
            self.skip_newlines();
            let right = operand(self)?;
            left = nodes::binary(op, left, right, Span::merge(start, self.prev_span()));
        }
        Ok(left)
    }

    // The base binds tighter than a unary prefix (`-2 ** 2` is `-(2 ** 2)`).
    // The exponent accepts unary prefixes and remains right-associative.
    fn parse_exponent(&mut self) -> Result<Expression, ParserError> {
        let left = self.parse_postfix()?;
        if !self.check_skip_newlines(&TokenKind::Pow) {
            return Ok(left);
        }
        let start = left.span;
        self.advance();
        self.skip_newlines();
        let right = self.with_nesting("exponent expression", |parser| {
            parser.parse_unary_expression()
        })?;
        Ok(nodes::binary(
            "**",
            left,
            right,
            Span::merge(start, self.prev_span()),
        ))
    }

    pub(super) fn parse_unary(&mut self) -> Result<SNode, ParserError> {
        self.parse_unary_expression().map(|expr| *expr)
    }

    fn parse_unary_expression(&mut self) -> Result<Expression, ParserError> {
        let op = if self.check(&TokenKind::Not) {
            "!"
        } else if self.check(&TokenKind::Minus) {
            "-"
        } else {
            return self.parse_exponent();
        };
        let start = self.current_span();
        self.advance();
        let operand =
            self.with_nesting("unary expression", |parser| parser.parse_unary_expression())?;
        Ok(nodes::unary(
            op,
            operand,
            Span::merge(start, self.prev_span()),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Node;
    use harn_lexer::Lexer;

    #[test]
    fn additive_newlines_preserve_statement_boundaries() {
        let tokens = Lexer::new("1\n-2").tokenize().unwrap();
        let program = Parser::new(tokens).parse().unwrap();
        assert_eq!(program.len(), 2);
        assert!(matches!(program[0].node, Node::IntLiteral(1)));
        assert!(matches!(&program[1].node, Node::UnaryOp { op, operand }
            if op == "-" && matches!(operand.node, Node::IntLiteral(2))));

        for (source, expected_op) in [("1\n+2", "+"), ("1-\n2", "-")] {
            let tokens = Lexer::new(source).tokenize().unwrap();
            let expression = Parser::new(tokens).parse_single_expression().unwrap();
            assert!(
                matches!(&expression.node, Node::BinaryOp { op, left, right }
                if op == expected_op
                    && matches!(left.node, Node::IntLiteral(1))
                    && matches!(right.node, Node::IntLiteral(2)))
            );
        }
    }
}
