use crate::ast::SNode;
use crate::operators::{Associativity, BinaryOperator, Precedence, UnaryOperator};
use harn_lexer::{Span, TokenKind};

use super::error::ParserError;
use super::expression_nodes::{self as nodes, Expression};
use super::state::Parser;

enum RangeSpelling {
    /// `a..b`
    Exclusive,
    /// `a..=b`
    Inclusive,
    /// `a to b [exclusive]`, accepted only by [`crate::operators::OperatorGrammar::LEGACY`].
    LegacyTo,
}

/// The token that spells a single-token binary operator.
fn binary_operator_token(op: BinaryOperator) -> TokenKind {
    match op {
        BinaryOperator::Pipe => TokenKind::Pipe,
        BinaryOperator::Or => TokenKind::Or,
        BinaryOperator::And => TokenKind::And,
        BinaryOperator::Eq => TokenKind::Eq,
        BinaryOperator::Neq => TokenKind::Neq,
        BinaryOperator::Lt => TokenKind::Lt,
        BinaryOperator::Gt => TokenKind::Gt,
        BinaryOperator::Lte => TokenKind::Lte,
        BinaryOperator::Gte => TokenKind::Gte,
        BinaryOperator::In | BinaryOperator::NotIn => TokenKind::In,
        BinaryOperator::NilCoalesce => TokenKind::NilCoal,
        BinaryOperator::Add => TokenKind::Plus,
        BinaryOperator::Sub => TokenKind::Minus,
        BinaryOperator::Mul => TokenKind::Star,
        BinaryOperator::Div => TokenKind::Slash,
        BinaryOperator::Mod => TokenKind::Percent,
        BinaryOperator::Pow => TokenKind::Pow,
    }
}

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
        self.parse_precedence(self.grammar.loosest())
            .map(|expr| *expr)
    }

    /// Parse an expression whose loosest operator belongs to `group` or a
    /// tighter group on the active [`crate::operators::OperatorGrammar`] ladder.
    fn parse_precedence(&mut self, group: Precedence) -> Result<Expression, ParserError> {
        match group {
            Precedence::Ternary => self.parse_ternary(),
            Precedence::Range => self.parse_range(),
            Precedence::Prefix => self.parse_unary_expression(),
            Precedence::Exponent => self.parse_exponent(),
            Precedence::Postfix => self.parse_postfix(),
            binary => self.parse_binary_group(binary),
        }
    }

    /// A left-associative group of binary operators.
    fn parse_binary_group(&mut self, group: Precedence) -> Result<Expression, ParserError> {
        debug_assert_eq!(group.associativity(), Associativity::Left);
        let operand = self.grammar.tighter(group);
        let mut left = self.parse_precedence(operand)?;
        while let Some(op) = self.check_binary_operator(group) {
            let start = left.span;
            self.advance();
            if op == BinaryOperator::NotIn {
                self.advance();
            }
            self.skip_newlines();
            let right = self.parse_precedence(operand)?;
            left = nodes::binary(op, left, right, Span::merge(start, self.prev_span()));
        }
        Ok(left)
    }

    /// The operator of `group` at the current position, if any. Operators that
    /// continue across a line break consume the newlines before them.
    fn check_binary_operator(&mut self, group: Precedence) -> Option<BinaryOperator> {
        for op in BinaryOperator::ALL {
            if op.precedence() != group {
                continue;
            }
            let found = match op {
                BinaryOperator::In => self.check(&TokenKind::In),
                BinaryOperator::NotIn => {
                    self.check_identifier("not") && matches!(self.peek_kind(), Some(TokenKind::In))
                }
                _ => {
                    let kind = binary_operator_token(op);
                    if op.continues_across_newline() {
                        self.check_skip_newlines(&kind)
                    } else {
                        self.check(&kind)
                    }
                }
            };
            if found {
                return Some(op);
            }
        }
        None
    }

    /// Range formation does not associate: `a..b..c` needs parentheses.
    fn parse_range(&mut self) -> Result<Expression, ParserError> {
        let operand = self.grammar.tighter(Precedence::Range);
        let left = self.parse_precedence(operand)?;
        let Some(spelling) = self.check_range_operator()? else {
            return Ok(left);
        };
        let start = left.span;
        self.advance();
        self.skip_newlines();
        let right = self.parse_precedence(operand)?;
        let inclusive = match spelling {
            RangeSpelling::Exclusive => false,
            RangeSpelling::Inclusive => true,
            RangeSpelling::LegacyTo => {
                let exclusive = self.check_identifier("exclusive");
                if exclusive {
                    self.advance();
                }
                !exclusive
            }
        };
        if self.check(&TokenKind::DotDot) || self.check(&TokenKind::DotDotEq) {
            return Err(self.error("end of range; parenthesize a range used as a range bound"));
        }
        Ok(nodes::range(
            left,
            right,
            inclusive,
            Span::merge(start, self.prev_span()),
        ))
    }

    /// The range operator at the current position, if any. Under the
    /// current grammar the retired `a to b` spelling is a targeted error.
    fn check_range_operator(&mut self) -> Result<Option<RangeSpelling>, ParserError> {
        if self.check_skip_newlines(&TokenKind::DotDot) {
            return Ok(Some(RangeSpelling::Exclusive));
        }
        if self.check_skip_newlines(&TokenKind::DotDotEq) {
            return Ok(Some(RangeSpelling::Inclusive));
        }
        if self.check_identifier("to") {
            if self.grammar.legacy_ranges() {
                return Ok(Some(RangeSpelling::LegacyTo));
            }
            return Err(ParserError::RemovedRangeSyntax {
                span: self.current_span(),
            });
        }
        Ok(None)
    }

    fn parse_ternary(&mut self) -> Result<Expression, ParserError> {
        let condition = self.parse_precedence(self.grammar.tighter(Precedence::Ternary))?;
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
            BinaryOperator::Pow,
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
            UnaryOperator::Not
        } else if self.check(&TokenKind::Minus) {
            UnaryOperator::Neg
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
            if *op == UnaryOperator::Neg && matches!(operand.node, Node::IntLiteral(2))));

        for (source, expected_op) in [
            ("1\n+2", BinaryOperator::Add),
            ("1-\n2", BinaryOperator::Sub),
        ] {
            let tokens = Lexer::new(source).tokenize().unwrap();
            let expression = Parser::new(tokens).parse_single_expression().unwrap();
            assert!(
                matches!(&expression.node, Node::BinaryOp { op, left, right }
                if *op == expected_op
                    && matches!(left.node, Node::IntLiteral(1))
                    && matches!(right.node, Node::IntLiteral(2)))
            );
        }
    }
}
