use crate::ast::*;
use harn_lexer::{Span, TokenKind};

use super::error::ParserError;
use super::expression_nodes::{self as nodes, Expression};
use super::state::Parser;

impl Parser {
    pub(super) fn parse_postfix(&mut self) -> Result<Expression, ParserError> {
        let expr = Box::new(self.parse_primary()?);
        self.parse_postfix_suffixes(expr)
    }

    fn parse_postfix_suffixes(&mut self, mut expr: Expression) -> Result<Expression, ParserError> {
        loop {
            if self.check_skip_newlines(&TokenKind::Dot)
                || self.check_skip_newlines(&TokenKind::QuestionDot)
            {
                let optional = self.check(&TokenKind::QuestionDot);
                let start = expr.span;
                self.advance();
                if optional && self.check(&TokenKind::LBracket) {
                    self.advance();
                    let index = self.parse_nested_expression("optional subscript index")?;
                    self.consume(&TokenKind::RBracket, "]")?;
                    expr = nodes::expression(
                        || Node::OptionalSubscriptAccess {
                            object: expr,
                            index: Box::new(index),
                        },
                        Span::merge(start, self.prev_span()),
                    );
                    continue;
                }
                let member = self.consume_identifier_or_keyword("member name")?;
                if self.check(&TokenKind::LParen) {
                    self.advance();
                    let args = self.parse_arg_list()?;
                    self.consume(&TokenKind::RParen, ")")?;
                    if optional {
                        expr = nodes::expression(
                            || Node::OptionalMethodCall {
                                object: expr,
                                method: member,
                                args,
                            },
                            Span::merge(start, self.prev_span()),
                        );
                    } else {
                        expr = nodes::expression(
                            || Node::MethodCall {
                                object: expr,
                                method: member,
                                args,
                            },
                            Span::merge(start, self.prev_span()),
                        );
                    }
                } else if optional {
                    expr = nodes::expression(
                        || Node::OptionalPropertyAccess {
                            object: expr,
                            property: member,
                        },
                        Span::merge(start, self.prev_span()),
                    );
                } else {
                    expr = nodes::expression(
                        || Node::PropertyAccess {
                            object: expr,
                            property: member,
                        },
                        Span::merge(start, self.prev_span()),
                    );
                }
            } else if self.check(&TokenKind::LBracket) {
                let start = expr.span;
                self.advance();

                // Disambiguate `[:end]` / `[start:end]` / `[start:]` slices from
                // `[index]` subscript access.
                if self.check(&TokenKind::Colon) {
                    self.advance();
                    let end_expr = if self.check(&TokenKind::RBracket) {
                        None
                    } else {
                        Some(Box::new(self.parse_nested_expression("slice bound")?))
                    };
                    self.consume(&TokenKind::RBracket, "]")?;
                    expr = nodes::expression(
                        || Node::SliceAccess {
                            object: expr,
                            start: None,
                            end: end_expr,
                        },
                        Span::merge(start, self.prev_span()),
                    );
                } else {
                    let index = self.parse_nested_expression("subscript index")?;
                    if self.check(&TokenKind::Colon) {
                        self.advance();
                        let end_expr = if self.check(&TokenKind::RBracket) {
                            None
                        } else {
                            Some(Box::new(self.parse_nested_expression("slice bound")?))
                        };
                        self.consume(&TokenKind::RBracket, "]")?;
                        expr = nodes::expression(
                            || Node::SliceAccess {
                                object: expr,
                                start: Some(Box::new(index)),
                                end: end_expr,
                            },
                            Span::merge(start, self.prev_span()),
                        );
                    } else {
                        self.consume(&TokenKind::RBracket, "]")?;
                        expr = nodes::expression(
                            || Node::SubscriptAccess {
                                object: expr,
                                index: Box::new(index),
                            },
                            Span::merge(start, self.prev_span()),
                        );
                    }
                }
            } else if self.check(&TokenKind::LBrace) {
                let struct_name = match &expr.node {
                    Node::Identifier(name) if self.is_struct_construct_lookahead(name) => {
                        Some(name.clone())
                    }
                    _ => None,
                };
                let Some(struct_name) = struct_name else {
                    break;
                };
                let start = expr.span;
                self.advance();
                let dict = self.parse_dict_literal(start)?;
                let fields = match dict.node {
                    Node::DictLiteral(fields) => fields,
                    _ => unreachable!("dict parser must return a dict literal"),
                };
                expr = nodes::expression(
                    || Node::StructConstruct {
                        struct_name,
                        fields,
                    },
                    dict.span,
                );
            } else if self.check(&TokenKind::Lt) && matches!(expr.node, Node::Identifier(_)) {
                let saved_pos = self.pos;
                let start = expr.span;
                self.advance();
                let parsed_type_args = self.parse_type_arg_list();
                if let Ok(type_args) = parsed_type_args {
                    if self.check(&TokenKind::LParen) {
                        self.advance();
                        let args = self.parse_arg_list()?;
                        self.consume(&TokenKind::RParen, ")")?;
                        if let Node::Identifier(name) = expr.node {
                            expr = nodes::expression(
                                || Node::FunctionCall {
                                    name,
                                    type_args,
                                    args,
                                },
                                Span::merge(start, self.prev_span()),
                            );
                        }
                    } else {
                        self.pos = saved_pos;
                        break;
                    }
                } else {
                    self.pos = saved_pos;
                    break;
                }
            } else if self.check(&TokenKind::LParen) {
                let start = expr.span;
                self.advance();
                let args = self.parse_arg_list()?;
                self.consume(&TokenKind::RParen, ")")?;
                if let Node::Identifier(name) = &expr.node {
                    let name = name.clone();
                    expr = nodes::expression(
                        || Node::FunctionCall {
                            name,
                            type_args: Vec::new(),
                            args,
                        },
                        Span::merge(start, self.prev_span()),
                    );
                } else {
                    expr = nodes::expression(
                        || Node::ValueCall { callee: expr, args },
                        Span::merge(start, self.prev_span()),
                    );
                }
            } else if self.check(&TokenKind::Question) {
                // Disambiguate `?[index]` (legacy optional subscript), `expr?`
                // (postfix try), and `expr ? a : b` (ternary).
                if self.question_starts_ternary_branch() {
                    break;
                }
                if matches!(self.peek_kind_at(1), Some(TokenKind::LBracket)) {
                    let start = expr.span;
                    self.advance(); // consume ?
                    self.advance(); // consume [
                    let index = self.parse_nested_expression("optional subscript index")?;
                    self.consume(&TokenKind::RBracket, "]")?;
                    expr = nodes::expression(
                        || Node::OptionalSubscriptAccess {
                            object: expr,
                            index: Box::new(index),
                        },
                        Span::merge(start, self.prev_span()),
                    );
                    continue;
                }
                let start = expr.span;
                self.advance();
                expr = nodes::expression(
                    || Node::TryOperator { operand: expr },
                    Span::merge(start, self.prev_span()),
                );
            } else if self.check(&TokenKind::Not) {
                // Postfix `!` is the non-null assertion. A `!` following a
                // primary can only be this: `!=` is a single `Neq` token and
                // prefix `!` (logical not) is parsed at the start of a unary,
                // never reached here.
                let start = expr.span;
                self.advance();
                expr = nodes::expression(
                    || Node::NonNullAssert { operand: expr },
                    Span::merge(start, self.prev_span()),
                );
            } else {
                break;
            }
        }

        Ok(expr)
    }

    fn question_starts_ternary_branch(&self) -> bool {
        // Look at the first non-newline token after `?`. A ternary may wrap
        // its true-branch onto a new line (`cond ?\n value : other`), so a
        // newline immediately after `?` must not cause us to misclassify this
        // as a postfix-`?`.
        let next = self
            .tokens
            .iter()
            .skip(self.pos + 1)
            .find(|t| t.kind != TokenKind::Newline)
            .map(|t| &t.kind);
        next.is_some_and(Self::token_starts_ternary_branch)
            && self.question_has_top_level_ternary_colon()
    }

    fn token_starts_ternary_branch(kind: &TokenKind) -> bool {
        matches!(
            kind,
            TokenKind::Identifier(_)
                | TokenKind::IntLiteral(_)
                | TokenKind::FloatLiteral(_)
                | TokenKind::StringLiteral(_)
                | TokenKind::RawStringLiteral(_)
                | TokenKind::InterpolatedString(_)
                | TokenKind::True
                | TokenKind::False
                | TokenKind::Nil
                | TokenKind::LParen
                | TokenKind::LBracket
                | TokenKind::LBrace
                | TokenKind::Not
                | TokenKind::Minus
                | TokenKind::Fn
                | TokenKind::If
                | TokenKind::Match
                | TokenKind::Try
                | TokenKind::Spawn
                | TokenKind::Parallel
                | TokenKind::Retry
                | TokenKind::DurationLiteral(_)
        )
    }

    fn question_has_top_level_ternary_colon(&self) -> bool {
        let mut delimiter_depth = 0usize;
        // True when the most recent significant top-level token was `?` or
        // `:` — i.e. we're scanning for the start of a branch and a newline
        // here is just a wrap, not an end-of-ternary.
        let mut at_branch_start = true;
        for (pos, token) in self.tokens.iter().enumerate().skip(self.pos + 1) {
            if delimiter_depth == 0 {
                match token.kind {
                    TokenKind::Colon => return true,
                    TokenKind::Newline => {
                        if at_branch_start {
                            // `?` (or `:`) was the last significant token; this
                            // newline simply wraps the branch onto a new line.
                            continue;
                        }
                        if self.next_non_newline_continues_ternary_branch(pos + 1) {
                            continue;
                        }
                        return false;
                    }
                    TokenKind::RParen
                    | TokenKind::RBracket
                    | TokenKind::RBrace
                    | TokenKind::Eof => {
                        return false;
                    }
                    _ => {
                        at_branch_start = false;
                    }
                }
            }

            match token.kind {
                TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => {
                    delimiter_depth += 1;
                }
                TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                    delimiter_depth = delimiter_depth.saturating_sub(1);
                }
                TokenKind::Eof => return false,
                _ => {}
            }
        }
        false
    }

    fn next_non_newline_continues_ternary_branch(&self, start_pos: usize) -> bool {
        let Some(kind) = self
            .tokens
            .iter()
            .skip(start_pos)
            .find(|token| token.kind != TokenKind::Newline)
            .map(|token| &token.kind)
        else {
            return false;
        };
        matches!(
            kind,
            TokenKind::Colon
                | TokenKind::Plus
                | TokenKind::Star
                | TokenKind::Slash
                | TokenKind::Percent
                | TokenKind::Pow
                | TokenKind::And
                | TokenKind::Or
                | TokenKind::Eq
                | TokenKind::Neq
                | TokenKind::Lt
                | TokenKind::Gt
                | TokenKind::Lte
                | TokenKind::Gte
                | TokenKind::NilCoal
                | TokenKind::Pipe
                | TokenKind::Dot
                | TokenKind::QuestionDot
        )
    }
}
