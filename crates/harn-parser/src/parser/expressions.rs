use crate::ast::*;
use harn_lexer::{Span, TokenKind};

use super::error::ParserError;
use super::state::Parser;

impl Parser {
    /// Parse a single expression (for string interpolation).
    pub fn parse_single_expression(&mut self) -> Result<SNode, ParserError> {
        self.skip_newlines();
        self.parse_expression()
    }

    pub(super) fn parse_expression(&mut self) -> Result<SNode, ParserError> {
        self.skip_newlines();
        self.parse_pipe()
    }

    pub(super) fn parse_pipe(&mut self) -> Result<SNode, ParserError> {
        let mut left = self.parse_range()?;
        while self.check_skip_newlines(&TokenKind::Pipe) {
            let start = left.span;
            self.advance();
            self.skip_newlines();
            let right = self.parse_range()?;
            left = spanned(
                Node::BinaryOp {
                    op: "|>".into(),
                    left: Box::new(left),
                    right: Box::new(right),
                },
                Span::merge(start, self.prev_span()),
            );
        }
        Ok(left)
    }

    pub(super) fn parse_range(&mut self) -> Result<SNode, ParserError> {
        let left = self.parse_ternary()?;
        if self.check(&TokenKind::To) {
            let start = left.span;
            self.advance();
            let right = self.parse_ternary()?;
            let inclusive = if self.check(&TokenKind::Exclusive) {
                self.advance();
                false
            } else {
                true
            };
            return Ok(spanned(
                Node::RangeExpr {
                    start: Box::new(left),
                    end: Box::new(right),
                    inclusive,
                },
                Span::merge(start, self.prev_span()),
            ));
        }
        Ok(left)
    }

    pub(super) fn parse_ternary(&mut self) -> Result<SNode, ParserError> {
        let condition = self.parse_logical_or()?;
        if !self.check(&TokenKind::Question) {
            return Ok(condition);
        }
        let start = condition.span;
        self.advance(); // skip ?
        let true_val = self.parse_logical_or()?;
        self.consume(&TokenKind::Colon, ":")?;
        let false_val = self.parse_logical_or()?;
        Ok(spanned(
            Node::Ternary {
                condition: Box::new(condition),
                true_expr: Box::new(true_val),
                false_expr: Box::new(false_val),
            },
            Span::merge(start, self.prev_span()),
        ))
    }

    // `??` binds tighter than arithmetic/comparison but looser than `* / % **`,
    // so `xs?.count ?? 0 > 0` parses as `(xs?.count ?? 0) > 0`.
    pub(super) fn parse_nil_coalescing(&mut self) -> Result<SNode, ParserError> {
        let mut left = self.parse_multiplicative()?;
        while self.check_skip_newlines(&TokenKind::NilCoal) {
            let start = left.span;
            self.advance();
            self.skip_newlines();
            let right = self.parse_multiplicative()?;
            left = spanned(
                Node::BinaryOp {
                    op: "??".into(),
                    left: Box::new(left),
                    right: Box::new(right),
                },
                Span::merge(start, self.prev_span()),
            );
        }
        Ok(left)
    }

    pub(super) fn parse_logical_or(&mut self) -> Result<SNode, ParserError> {
        let mut left = self.parse_logical_and()?;
        while self.check_skip_newlines(&TokenKind::Or) {
            let start = left.span;
            self.advance();
            self.skip_newlines();
            let right = self.parse_logical_and()?;
            left = spanned(
                Node::BinaryOp {
                    op: "||".into(),
                    left: Box::new(left),
                    right: Box::new(right),
                },
                Span::merge(start, self.prev_span()),
            );
        }
        Ok(left)
    }

    pub(super) fn parse_logical_and(&mut self) -> Result<SNode, ParserError> {
        let mut left = self.parse_equality()?;
        while self.check_skip_newlines(&TokenKind::And) {
            let start = left.span;
            self.advance();
            self.skip_newlines();
            let right = self.parse_equality()?;
            left = spanned(
                Node::BinaryOp {
                    op: "&&".into(),
                    left: Box::new(left),
                    right: Box::new(right),
                },
                Span::merge(start, self.prev_span()),
            );
        }
        Ok(left)
    }

    pub(super) fn parse_equality(&mut self) -> Result<SNode, ParserError> {
        let mut left = self.parse_comparison()?;
        while self.check_skip_newlines(&TokenKind::Eq) || self.check_skip_newlines(&TokenKind::Neq)
        {
            let start = left.span;
            let op = if self.check(&TokenKind::Eq) {
                "=="
            } else {
                "!="
            };
            self.advance();
            self.skip_newlines();
            let right = self.parse_comparison()?;
            left = spanned(
                Node::BinaryOp {
                    op: op.into(),
                    left: Box::new(left),
                    right: Box::new(right),
                },
                Span::merge(start, self.prev_span()),
            );
        }
        Ok(left)
    }

    pub(super) fn parse_comparison(&mut self) -> Result<SNode, ParserError> {
        let mut left = self.parse_additive()?;
        loop {
            if self.check_skip_newlines(&TokenKind::Lt)
                || self.check_skip_newlines(&TokenKind::Gt)
                || self.check_skip_newlines(&TokenKind::Lte)
                || self.check_skip_newlines(&TokenKind::Gte)
            {
                let start = left.span;
                let op = match self.current().map(|t| &t.kind) {
                    Some(TokenKind::Lt) => "<",
                    Some(TokenKind::Gt) => ">",
                    Some(TokenKind::Lte) => "<=",
                    Some(TokenKind::Gte) => ">=",
                    _ => "<",
                };
                self.advance();
                self.skip_newlines();
                let right = self.parse_additive()?;
                left = spanned(
                    Node::BinaryOp {
                        op: op.into(),
                        left: Box::new(left),
                        right: Box::new(right),
                    },
                    Span::merge(start, self.prev_span()),
                );
            } else if self.check(&TokenKind::In) {
                let start = left.span;
                self.advance();
                self.skip_newlines();
                let right = self.parse_additive()?;
                left = spanned(
                    Node::BinaryOp {
                        op: "in".into(),
                        left: Box::new(left),
                        right: Box::new(right),
                    },
                    Span::merge(start, self.prev_span()),
                );
            } else if self.check_identifier("not") {
                let saved = self.pos;
                self.advance();
                if self.check(&TokenKind::In) {
                    let start = left.span;
                    self.advance();
                    self.skip_newlines();
                    let right = self.parse_additive()?;
                    left = spanned(
                        Node::BinaryOp {
                            op: "not_in".into(),
                            left: Box::new(left),
                            right: Box::new(right),
                        },
                        Span::merge(start, self.prev_span()),
                    );
                } else {
                    self.pos = saved;
                    break;
                }
            } else {
                break;
            }
        }
        Ok(left)
    }

    pub(super) fn parse_additive(&mut self) -> Result<SNode, ParserError> {
        let mut left = self.parse_nil_coalescing()?;
        while self.check_skip_newlines(&TokenKind::Plus) || self.check(&TokenKind::Minus) {
            let start = left.span;
            let op = if self.check(&TokenKind::Plus) {
                "+"
            } else {
                "-"
            };
            self.advance();
            self.skip_newlines();
            let right = self.parse_nil_coalescing()?;
            left = spanned(
                Node::BinaryOp {
                    op: op.into(),
                    left: Box::new(left),
                    right: Box::new(right),
                },
                Span::merge(start, self.prev_span()),
            );
        }
        Ok(left)
    }

    pub(super) fn parse_multiplicative(&mut self) -> Result<SNode, ParserError> {
        let mut left = self.parse_exponent()?;
        while self.check_skip_newlines(&TokenKind::Star)
            || self.check_skip_newlines(&TokenKind::Slash)
            || self.check_skip_newlines(&TokenKind::Percent)
        {
            let start = left.span;
            let op = if self.check(&TokenKind::Star) {
                "*"
            } else if self.check(&TokenKind::Slash) {
                "/"
            } else {
                "%"
            };
            self.advance();
            self.skip_newlines();
            let right = self.parse_exponent()?;
            left = spanned(
                Node::BinaryOp {
                    op: op.into(),
                    left: Box::new(left),
                    right: Box::new(right),
                },
                Span::merge(start, self.prev_span()),
            );
        }
        Ok(left)
    }

    pub(super) fn parse_exponent(&mut self) -> Result<SNode, ParserError> {
        let left = self.parse_unary()?;
        if !self.check_skip_newlines(&TokenKind::Pow) {
            return Ok(left);
        }

        let start = left.span;
        self.advance();
        self.skip_newlines();
        let right = self.parse_exponent()?;
        Ok(spanned(
            Node::BinaryOp {
                op: "**".into(),
                left: Box::new(left),
                right: Box::new(right),
            },
            Span::merge(start, self.prev_span()),
        ))
    }

    pub(super) fn parse_unary(&mut self) -> Result<SNode, ParserError> {
        if self.check(&TokenKind::Not) {
            let start = self.current_span();
            self.advance();
            let operand = self.parse_unary()?;
            return Ok(spanned(
                Node::UnaryOp {
                    op: "!".into(),
                    operand: Box::new(operand),
                },
                Span::merge(start, self.prev_span()),
            ));
        }
        if self.check(&TokenKind::Minus) {
            let start = self.current_span();
            self.advance();
            let operand = self.parse_unary()?;
            return Ok(spanned(
                Node::UnaryOp {
                    op: "-".into(),
                    operand: Box::new(operand),
                },
                Span::merge(start, self.prev_span()),
            ));
        }
        self.parse_postfix()
    }

    pub(super) fn parse_postfix(&mut self) -> Result<SNode, ParserError> {
        let mut expr = self.parse_primary()?;

        loop {
            if self.check_skip_newlines(&TokenKind::Dot)
                || self.check_skip_newlines(&TokenKind::QuestionDot)
            {
                let optional = self.check(&TokenKind::QuestionDot);
                let start = expr.span;
                self.advance();
                let member = self.consume_identifier_or_keyword("member name")?;
                if self.check(&TokenKind::LParen) {
                    self.advance();
                    let args = self.parse_arg_list()?;
                    self.consume(&TokenKind::RParen, ")")?;
                    if optional {
                        expr = spanned(
                            Node::OptionalMethodCall {
                                object: Box::new(expr),
                                method: member,
                                args,
                            },
                            Span::merge(start, self.prev_span()),
                        );
                    } else {
                        expr = spanned(
                            Node::MethodCall {
                                object: Box::new(expr),
                                method: member,
                                args,
                            },
                            Span::merge(start, self.prev_span()),
                        );
                    }
                } else if optional {
                    expr = spanned(
                        Node::OptionalPropertyAccess {
                            object: Box::new(expr),
                            property: member,
                        },
                        Span::merge(start, self.prev_span()),
                    );
                } else {
                    expr = spanned(
                        Node::PropertyAccess {
                            object: Box::new(expr),
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
                        Some(Box::new(self.parse_expression()?))
                    };
                    self.consume(&TokenKind::RBracket, "]")?;
                    expr = spanned(
                        Node::SliceAccess {
                            object: Box::new(expr),
                            start: None,
                            end: end_expr,
                        },
                        Span::merge(start, self.prev_span()),
                    );
                } else {
                    let index = self.parse_expression()?;
                    if self.check(&TokenKind::Colon) {
                        self.advance();
                        let end_expr = if self.check(&TokenKind::RBracket) {
                            None
                        } else {
                            Some(Box::new(self.parse_expression()?))
                        };
                        self.consume(&TokenKind::RBracket, "]")?;
                        expr = spanned(
                            Node::SliceAccess {
                                object: Box::new(expr),
                                start: Some(Box::new(index)),
                                end: end_expr,
                            },
                            Span::merge(start, self.prev_span()),
                        );
                    } else {
                        self.consume(&TokenKind::RBracket, "]")?;
                        expr = spanned(
                            Node::SubscriptAccess {
                                object: Box::new(expr),
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
                expr = spanned(
                    Node::StructConstruct {
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
                            expr = spanned(
                                Node::FunctionCall {
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
            } else if self.check(&TokenKind::LParen) && matches!(expr.node, Node::Identifier(_)) {
                let start = expr.span;
                self.advance();
                let args = self.parse_arg_list()?;
                self.consume(&TokenKind::RParen, ")")?;
                if let Node::Identifier(name) = expr.node {
                    expr = spanned(
                        Node::FunctionCall {
                            name,
                            type_args: Vec::new(),
                            args,
                        },
                        Span::merge(start, self.prev_span()),
                    );
                }
            } else if self.check(&TokenKind::Question) {
                // Disambiguate `?[index]` (optional subscript), `expr?`
                // (postfix try), and `expr ? a : b` (ternary).
                //
                // Optional subscript wins eagerly when the next token is `[`
                // because `cond ? [a, b, c] : ...` is rare and writing it as
                // `cond ? ([a, b, c]) : ...` is a fine workaround, while
                // `obj?[k]` is the natural way to chain into a list/dict.
                let next_pos = self.pos + 1;
                let next_kind = self.tokens.get(next_pos).map(|t| &t.kind);
                if matches!(next_kind, Some(TokenKind::LBracket)) {
                    let start = expr.span;
                    self.advance(); // consume ?
                    self.advance(); // consume [
                    let index = self.parse_expression()?;
                    self.consume(&TokenKind::RBracket, "]")?;
                    expr = spanned(
                        Node::OptionalSubscriptAccess {
                            object: Box::new(expr),
                            index: Box::new(index),
                        },
                        Span::merge(start, self.prev_span()),
                    );
                    continue;
                }
                // Postfix try `expr?` vs ternary `expr ? a : b`: if the next
                // token could start a ternary branch, let parse_ternary
                // handle the `?`.
                let is_ternary = next_kind.is_some_and(|kind| {
                    matches!(
                        kind,
                        TokenKind::Identifier(_)
                            | TokenKind::IntLiteral(_)
                            | TokenKind::FloatLiteral(_)
                            | TokenKind::StringLiteral(_)
                            | TokenKind::InterpolatedString(_)
                            | TokenKind::True
                            | TokenKind::False
                            | TokenKind::Nil
                            | TokenKind::LParen
                            | TokenKind::LBrace
                            | TokenKind::Not
                            | TokenKind::Minus
                            | TokenKind::Fn
                    )
                });
                if is_ternary {
                    break;
                }
                let start = expr.span;
                self.advance();
                expr = spanned(
                    Node::TryOperator {
                        operand: Box::new(expr),
                    },
                    Span::merge(start, self.prev_span()),
                );
            } else {
                break;
            }
        }

        Ok(expr)
    }

    pub(super) fn parse_primary(&mut self) -> Result<SNode, ParserError> {
        let tok = self.current().ok_or_else(|| ParserError::UnexpectedEof {
            expected: "expression".into(),
            span: self.prev_span(),
        })?;
        let start = self.current_span();

        match &tok.kind {
            TokenKind::StringLiteral(s) => {
                let s = s.clone();
                self.advance();
                Ok(spanned(
                    Node::StringLiteral(s),
                    Span::merge(start, self.prev_span()),
                ))
            }
            TokenKind::RawStringLiteral(s) => {
                let s = s.clone();
                self.advance();
                Ok(spanned(
                    Node::RawStringLiteral(s),
                    Span::merge(start, self.prev_span()),
                ))
            }
            TokenKind::InterpolatedString(segments) => {
                let segments = segments.clone();
                self.advance();
                Ok(spanned(
                    Node::InterpolatedString(segments),
                    Span::merge(start, self.prev_span()),
                ))
            }
            TokenKind::IntLiteral(n) => {
                let n = *n;
                self.advance();
                Ok(spanned(
                    Node::IntLiteral(n),
                    Span::merge(start, self.prev_span()),
                ))
            }
            TokenKind::FloatLiteral(n) => {
                let n = *n;
                self.advance();
                Ok(spanned(
                    Node::FloatLiteral(n),
                    Span::merge(start, self.prev_span()),
                ))
            }
            TokenKind::True => {
                self.advance();
                Ok(spanned(
                    Node::BoolLiteral(true),
                    Span::merge(start, self.prev_span()),
                ))
            }
            TokenKind::False => {
                self.advance();
                Ok(spanned(
                    Node::BoolLiteral(false),
                    Span::merge(start, self.prev_span()),
                ))
            }
            TokenKind::Nil => {
                self.advance();
                Ok(spanned(
                    Node::NilLiteral,
                    Span::merge(start, self.prev_span()),
                ))
            }
            TokenKind::Identifier(name) => {
                let name = name.clone();
                self.advance();
                Ok(spanned(
                    Node::Identifier(name),
                    Span::merge(start, self.prev_span()),
                ))
            }
            TokenKind::LParen => {
                self.advance();
                let expr = self.parse_expression()?;
                self.consume(&TokenKind::RParen, ")")?;
                Ok(expr)
            }
            TokenKind::LBracket => self.parse_list_literal(),
            TokenKind::LBrace => self.parse_dict_or_closure(),
            TokenKind::Parallel => self.parse_parallel(),
            TokenKind::Retry => self.parse_retry(),
            TokenKind::If => self.parse_if_else(),
            TokenKind::Spawn => self.parse_spawn_expr(),
            TokenKind::DurationLiteral(ms) => {
                let ms = *ms;
                self.advance();
                Ok(spanned(
                    Node::DurationLiteral(ms),
                    Span::merge(start, self.prev_span()),
                ))
            }
            TokenKind::Deadline => self.parse_deadline(),
            TokenKind::Try => self.parse_try_catch(),
            TokenKind::Match => self.parse_match(),
            TokenKind::Fn => self.parse_fn_expr(),
            // Heredoc `<<TAG ... TAG` is only valid inside LLM tool-call JSON;
            // in source-position expressions, redirect authors to triple-quoted strings.
            TokenKind::Lt
                if matches!(self.peek_kind(), Some(&TokenKind::Lt))
                    && matches!(self.peek_kind_at(2), Some(TokenKind::Identifier(_))) =>
            {
                Err(ParserError::Unexpected {
                    got: "`<<` heredoc-like syntax".to_string(),
                    expected: "an expression — heredocs are only valid \
                               inside LLM tool-call argument JSON; \
                               for multiline strings in source code use \
                               triple-quoted `\"\"\"...\"\"\"`"
                        .to_string(),
                    span: start,
                })
            }
            _ => Err(self.error("expression")),
        }
    }

    /// Anonymous function `fn(params) { body }`. Sets `fn_syntax: true` on the
    /// Closure so the formatter can round-trip the original syntax.
    pub(super) fn parse_fn_expr(&mut self) -> Result<SNode, ParserError> {
        let start = self.current_span();
        self.consume(&TokenKind::Fn, "fn")?;
        self.consume(&TokenKind::LParen, "(")?;
        let params = self.parse_typed_param_list()?;
        self.consume(&TokenKind::RParen, ")")?;
        self.consume(&TokenKind::LBrace, "{")?;
        let body = self.parse_block()?;
        self.consume(&TokenKind::RBrace, "}")?;
        Ok(spanned(
            Node::Closure {
                params,
                body,
                fn_syntax: true,
            },
            Span::merge(start, self.prev_span()),
        ))
    }

    pub(super) fn parse_spawn_expr(&mut self) -> Result<SNode, ParserError> {
        let start = self.current_span();
        self.consume(&TokenKind::Spawn, "spawn")?;
        self.consume(&TokenKind::LBrace, "{")?;
        let body = self.parse_block()?;
        self.consume(&TokenKind::RBrace, "}")?;
        Ok(spanned(
            Node::SpawnExpr { body },
            Span::merge(start, self.prev_span()),
        ))
    }

    pub(super) fn parse_list_literal(&mut self) -> Result<SNode, ParserError> {
        let start = self.current_span();
        self.consume(&TokenKind::LBracket, "[")?;
        let mut elements = Vec::new();
        self.skip_newlines();

        while !self.is_at_end() && !self.check(&TokenKind::RBracket) {
            if self.check(&TokenKind::Dot) {
                let saved_pos = self.pos;
                self.advance();
                if self.check(&TokenKind::Dot) {
                    self.advance();
                    self.consume(&TokenKind::Dot, ".")?;
                    let spread_start = self.tokens[saved_pos].span;
                    let expr = self.parse_expression()?;
                    elements.push(spanned(
                        Node::Spread(Box::new(expr)),
                        Span::merge(spread_start, self.prev_span()),
                    ));
                } else {
                    self.pos = saved_pos;
                    elements.push(self.parse_expression()?);
                }
            } else {
                elements.push(self.parse_expression()?);
            }
            self.skip_newlines();
            if self.check(&TokenKind::Comma) {
                self.advance();
                self.skip_newlines();
            }
        }

        self.consume(&TokenKind::RBracket, "]")?;
        Ok(spanned(
            Node::ListLiteral(elements),
            Span::merge(start, self.prev_span()),
        ))
    }

    pub(super) fn parse_dict_or_closure(&mut self) -> Result<SNode, ParserError> {
        let start = self.current_span();
        self.consume(&TokenKind::LBrace, "{")?;
        self.skip_newlines();

        if self.check(&TokenKind::RBrace) {
            self.advance();
            return Ok(spanned(
                Node::DictLiteral(Vec::new()),
                Span::merge(start, self.prev_span()),
            ));
        }

        // Scan for `->` before the closing `}` to distinguish closure from dict.
        let saved = self.pos;
        if self.is_closure_lookahead() {
            self.pos = saved;
            return self.parse_closure_body(start);
        }
        self.pos = saved;
        self.parse_dict_literal(start)
    }

    /// After seeing `Identifier {`, decide whether the brace block is a
    /// struct-construction field list rather than a control-flow block.
    /// Struct fields always start with `name:` / `"name":` or `}`.
    pub(super) fn is_struct_construct_lookahead(&self, struct_name: &str) -> bool {
        if !struct_name
            .chars()
            .next()
            .is_some_and(|ch| ch.is_uppercase())
        {
            return false;
        }

        let mut offset = 1;
        while matches!(self.peek_kind_at(offset), Some(TokenKind::Newline)) {
            offset += 1;
        }

        match self.peek_kind_at(offset) {
            Some(TokenKind::RBrace) => true,
            Some(TokenKind::Identifier(_)) | Some(TokenKind::StringLiteral(_)) => {
                offset += 1;
                while matches!(self.peek_kind_at(offset), Some(TokenKind::Newline)) {
                    offset += 1;
                }
                matches!(self.peek_kind_at(offset), Some(TokenKind::Colon))
            }
            _ => false,
        }
    }

    /// Caller must save/restore `pos`; this advances while scanning.
    pub(super) fn is_closure_lookahead(&mut self) -> bool {
        let mut depth = 0;
        while !self.is_at_end() {
            if let Some(tok) = self.current() {
                match &tok.kind {
                    TokenKind::Arrow if depth == 0 => return true,
                    TokenKind::LBrace | TokenKind::LParen | TokenKind::LBracket => depth += 1,
                    TokenKind::RBrace if depth == 0 => return false,
                    TokenKind::RBrace => depth -= 1,
                    TokenKind::RParen | TokenKind::RBracket if depth > 0 => depth -= 1,
                    _ => {}
                }
                self.advance();
            } else {
                return false;
            }
        }
        false
    }

    /// Parse closure params and body (after opening { has been consumed).
    pub(super) fn parse_closure_body(&mut self, start: Span) -> Result<SNode, ParserError> {
        let params = self.parse_typed_param_list_until_arrow()?;
        self.consume(&TokenKind::Arrow, "->")?;
        let body = self.parse_block()?;
        self.consume(&TokenKind::RBrace, "}")?;
        Ok(spanned(
            Node::Closure {
                params,
                body,
                fn_syntax: false,
            },
            Span::merge(start, self.prev_span()),
        ))
    }

    /// Parse typed params until we see ->. Handles: `x`, `x: int`, `x, y`, `x: int, y: string`.
    pub(super) fn parse_typed_param_list_until_arrow(
        &mut self,
    ) -> Result<Vec<TypedParam>, ParserError> {
        self.parse_typed_params_until(|tok| tok == &TokenKind::Arrow)
    }

    pub(super) fn parse_dict_literal(&mut self, start: Span) -> Result<SNode, ParserError> {
        let entries = self.parse_dict_entries()?;
        Ok(spanned(
            Node::DictLiteral(entries),
            Span::merge(start, self.prev_span()),
        ))
    }

    pub(super) fn parse_dict_entries(&mut self) -> Result<Vec<DictEntry>, ParserError> {
        let mut entries = Vec::new();
        self.skip_newlines();

        while !self.is_at_end() && !self.check(&TokenKind::RBrace) {
            if self.check(&TokenKind::Dot) {
                let saved_pos = self.pos;
                self.advance();
                if self.check(&TokenKind::Dot) {
                    self.advance();
                    if self.check(&TokenKind::Dot) {
                        self.advance();
                        let spread_start = self.tokens[saved_pos].span;
                        let expr = self.parse_expression()?;
                        entries.push(DictEntry {
                            key: spanned(Node::NilLiteral, spread_start),
                            value: spanned(
                                Node::Spread(Box::new(expr)),
                                Span::merge(spread_start, self.prev_span()),
                            ),
                        });
                        self.skip_newlines();
                        if self.check(&TokenKind::Comma) {
                            self.advance();
                            self.skip_newlines();
                        }
                        continue;
                    }
                    self.pos = saved_pos;
                } else {
                    self.pos = saved_pos;
                }
            }
            let key = if self.check(&TokenKind::LBracket) {
                self.advance();
                let k = self.parse_expression()?;
                self.consume(&TokenKind::RBracket, "]")?;
                k
            } else if matches!(
                self.current().map(|t| &t.kind),
                Some(TokenKind::StringLiteral(_))
            ) {
                let key_span = self.current_span();
                let name =
                    if let Some(TokenKind::StringLiteral(s)) = self.current().map(|t| &t.kind) {
                        s.clone()
                    } else {
                        unreachable!()
                    };
                self.advance();
                spanned(Node::StringLiteral(name), key_span)
            } else {
                let key_span = self.current_span();
                let name = self.consume_identifier_or_keyword("dict key")?;
                spanned(Node::StringLiteral(name), key_span)
            };
            self.consume(&TokenKind::Colon, ":")?;
            let value = self.parse_expression()?;
            entries.push(DictEntry { key, value });
            self.skip_newlines();
            if self.check(&TokenKind::Comma) {
                self.advance();
                self.skip_newlines();
            }
        }

        self.consume(&TokenKind::RBrace, "}")?;
        Ok(entries)
    }

    /// Parse untyped parameter list (for pipelines, overrides).
    pub(super) fn parse_param_list(&mut self) -> Result<Vec<String>, ParserError> {
        let mut params = Vec::new();
        self.skip_newlines();

        while !self.is_at_end() && !self.check(&TokenKind::RParen) {
            params.push(self.consume_identifier("parameter name")?);
            if self.check(&TokenKind::Comma) {
                self.advance();
                self.skip_newlines();
            }
        }
        Ok(params)
    }

    /// Parse typed parameter list (for fn declarations).
    pub(super) fn parse_typed_param_list(&mut self) -> Result<Vec<TypedParam>, ParserError> {
        self.parse_typed_params_until(|tok| tok == &TokenKind::RParen)
    }

    /// Shared implementation: parse typed params with optional defaults until
    /// a terminator token is reached.
    pub(super) fn parse_typed_params_until(
        &mut self,
        is_terminator: impl Fn(&TokenKind) -> bool,
    ) -> Result<Vec<TypedParam>, ParserError> {
        let mut params = Vec::new();
        let mut seen_default = false;
        self.skip_newlines();

        while !self.is_at_end() {
            if let Some(tok) = self.current() {
                if is_terminator(&tok.kind) {
                    break;
                }
            } else {
                break;
            }
            let is_rest = if self.check(&TokenKind::Dot) {
                let p1 = self.pos + 1;
                let p2 = self.pos + 2;
                let is_ellipsis = p1 < self.tokens.len()
                    && p2 < self.tokens.len()
                    && self.tokens[p1].kind == TokenKind::Dot
                    && self.tokens[p2].kind == TokenKind::Dot;
                if is_ellipsis {
                    self.advance();
                    self.advance();
                    self.advance();
                    true
                } else {
                    false
                }
            } else {
                false
            };
            let name = self.consume_identifier("parameter name")?;
            let type_expr = self.try_parse_type_annotation()?;
            let default_value = if self.check(&TokenKind::Assign) {
                self.advance();
                seen_default = true;
                Some(Box::new(self.parse_expression()?))
            } else {
                if seen_default && !is_rest {
                    return Err(self.error(
                        "Required parameter cannot follow a parameter with a default value",
                    ));
                }
                None
            };
            if is_rest
                && !is_terminator(
                    &self
                        .current()
                        .map(|t| t.kind.clone())
                        .unwrap_or(TokenKind::Eof),
                )
            {
                return Err(self.error("Rest parameter must be the last parameter"));
            }
            params.push(TypedParam {
                name,
                type_expr,
                default_value,
                rest: is_rest,
            });
            if self.check(&TokenKind::Comma) {
                self.advance();
                self.skip_newlines();
            }
        }
        Ok(params)
    }

    pub(super) fn parse_arg_list(&mut self) -> Result<Vec<SNode>, ParserError> {
        let mut args = Vec::new();
        self.skip_newlines();

        while !self.is_at_end() && !self.check(&TokenKind::RParen) {
            if self.check(&TokenKind::Dot) {
                let saved_pos = self.pos;
                self.advance();
                if self.check(&TokenKind::Dot) {
                    self.advance();
                    self.consume(&TokenKind::Dot, ".")?;
                    let spread_start = self.tokens[saved_pos].span;
                    let expr = self.parse_expression()?;
                    args.push(spanned(
                        Node::Spread(Box::new(expr)),
                        Span::merge(spread_start, self.prev_span()),
                    ));
                } else {
                    self.pos = saved_pos;
                    args.push(self.parse_expression()?);
                }
            } else {
                args.push(self.parse_expression()?);
            }
            self.skip_newlines();
            if self.check(&TokenKind::Comma) {
                self.advance();
                self.skip_newlines();
            }
        }
        Ok(args)
    }
}
