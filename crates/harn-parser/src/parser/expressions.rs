use crate::ast::*;
use harn_lexer::{Span, TokenKind};

use super::error::ParserError;
use super::state::Parser;

impl Parser {
    /// Parse a single expression (for string interpolation).
    ///
    /// An interpolation hole (`${ ... }`) must contain exactly one expression.
    /// After parsing it, require that the token stream is exhausted so leftover
    /// tokens are reported as a parse error instead of being silently dropped —
    /// otherwise `${a b}` would render just `a` and `${1e20}` just `1` (`e20`
    /// is a separate identifier, since scientific notation is not a float
    /// literal), masking the typo.
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
        // `?` may appear on the next line as a wrap-to-new-line continuation.
        // Postfix `?` (try) is already consumed by `parse_postfix`, so by the
        // time we reach here a `?` (possibly across a newline) is unambiguously
        // a ternary operator.
        if !self.check_skip_newlines(&TokenKind::Question) {
            return Ok(condition);
        }
        let start = condition.span;
        self.advance(); // skip ?
        self.skip_newlines();
        let true_val = self.with_nesting("ternary expression", |parser| parser.parse_ternary())?;
        // `consume` already skips leading newlines for `:`.
        self.consume(&TokenKind::Colon, ":")?;
        self.skip_newlines();
        let false_val = self.with_nesting("ternary expression", |parser| parser.parse_ternary())?;
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
        let mut left = self.parse_unary()?;
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
            let right = self.parse_unary()?;
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

    // `**` binds more tightly than a unary prefix on its *left* operand, so
    // `-2 ** 2` parses as `-(2 ** 2)` (matching Python, Ruby, and ordinary math
    // notation rather than the spreadsheet `(-2) ** 2` reading). The base is
    // therefore a `postfix` expression, while the exponent recurses through
    // `parse_unary` so a unary prefix on the *right* still works (`2 ** -3` is
    // `2 ** (-3)`) and chained `**` stays right-associative.
    pub(super) fn parse_exponent(&mut self) -> Result<SNode, ParserError> {
        let left = self.parse_postfix()?;
        if !self.check_skip_newlines(&TokenKind::Pow) {
            return Ok(left);
        }

        let start = left.span;
        self.advance();
        self.skip_newlines();
        let right = self.with_nesting("exponent expression", |parser| parser.parse_unary())?;
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
            let operand = self.with_nesting("unary expression", |parser| parser.parse_unary())?;
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
            let operand = self.with_nesting("unary expression", |parser| parser.parse_unary())?;
            return Ok(spanned(
                Node::UnaryOp {
                    op: "-".into(),
                    operand: Box::new(operand),
                },
                Span::merge(start, self.prev_span()),
            ));
        }
        self.parse_exponent()
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
                if optional && self.check(&TokenKind::LBracket) {
                    self.advance();
                    let index = self.parse_nested_expression("optional subscript index")?;
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
                        Some(Box::new(self.parse_nested_expression("slice bound")?))
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
                    let index = self.parse_nested_expression("subscript index")?;
                    if self.check(&TokenKind::Colon) {
                        self.advance();
                        let end_expr = if self.check(&TokenKind::RBracket) {
                            None
                        } else {
                            Some(Box::new(self.parse_nested_expression("slice bound")?))
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
                    expr = spanned(
                        Node::OptionalSubscriptAccess {
                            object: Box::new(expr),
                            index: Box::new(index),
                        },
                        Span::merge(start, self.prev_span()),
                    );
                    continue;
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
                | TokenKind::Deadline
                | TokenKind::RequestApproval
                | TokenKind::DualControl
                | TokenKind::AskUser
                | TokenKind::EscalateTo
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
            TokenKind::Identifier(name)
                if name == "cost_route" && self.peek_kind() == Some(&TokenKind::LBrace) =>
            {
                self.parse_cost_route()
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
                let expr = self.with_nesting("parenthesized expression", |parser| {
                    let expr = parser.parse_expression()?;
                    parser.consume(&TokenKind::RParen, ")")?;
                    Ok(expr)
                })?;
                Ok(expr)
            }
            TokenKind::LBracket => self.parse_list_literal(),
            TokenKind::LBrace => self.parse_dict_or_closure(),
            TokenKind::Parallel => self.parse_parallel(),
            TokenKind::Retry => self.parse_retry(),
            TokenKind::If => self.parse_if_else(),
            TokenKind::Spawn => self.parse_spawn_expr(),
            TokenKind::RequestApproval => self.parse_hitl_expr(HitlKind::RequestApproval),
            TokenKind::DualControl => self.parse_hitl_expr(HitlKind::DualControl),
            TokenKind::AskUser => self.parse_hitl_expr(HitlKind::AskUser),
            TokenKind::EscalateTo => self.parse_hitl_expr(HitlKind::EscalateTo),
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
        let return_type = if self.check(&TokenKind::Arrow) {
            self.advance();
            Some(self.parse_nested_type_expr("closure return type")?)
        } else {
            None
        };
        self.consume(&TokenKind::LBrace, "{")?;
        let body = self.parse_block()?;
        self.consume(&TokenKind::RBrace, "}")?;
        Ok(spanned(
            Node::Closure {
                params,
                return_type,
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

    /// Parse a first-class HITL primitive: one of `request_approval`,
    /// `dual_control`, `ask_user`, `escalate_to`. The keyword has
    /// already been peeked at; this method consumes it plus the
    /// parenthesized argument list.
    ///
    /// Each argument is either positional (`expr`) or named
    /// (`name: expr`). The grammar accepts the existing positional
    /// invocation form so existing scripts and conformance tests
    /// (e.g. `request_approval("deploy", {quorum: 2, ...})`) keep
    /// working unchanged. Argument validation (required names,
    /// duplicates, ordering) is performed by the typechecker.
    pub(super) fn parse_hitl_expr(&mut self, kind: HitlKind) -> Result<SNode, ParserError> {
        let start = self.current_span();
        let kw_token = match kind {
            HitlKind::RequestApproval => TokenKind::RequestApproval,
            HitlKind::DualControl => TokenKind::DualControl,
            HitlKind::AskUser => TokenKind::AskUser,
            HitlKind::EscalateTo => TokenKind::EscalateTo,
        };
        self.consume(&kw_token, kind.as_keyword())?;
        self.consume(&TokenKind::LParen, "(")?;
        self.skip_newlines();

        let mut args: Vec<HitlArg> = Vec::new();
        while !self.is_at_end() && !self.check(&TokenKind::RParen) {
            let arg_start = self.current_span();
            // Look ahead two tokens to detect `identifier ":"`. The
            // identifier itself is parsed as part of the expression so
            // we keep the dispatch simple: peek for `Identifier` then
            // a `Colon` to identify a named argument.
            // `peek_kind_at(0)` is the current token; `peek_kind_at(1)`
            // is one ahead. A named-arg slot starts with `ident :`.
            let is_named = matches!(
                (self.peek_kind_at(0), self.peek_kind_at(1)),
                (Some(TokenKind::Identifier(_)), Some(TokenKind::Colon))
            );
            let (name, value) = if is_named {
                let Some(TokenKind::Identifier(raw)) = self.peek_kind_at(0).cloned() else {
                    unreachable!("named arg dispatch already matched Identifier token")
                };
                self.advance();
                self.consume(&TokenKind::Colon, ":")?;
                self.skip_newlines();
                let value = self.parse_nested_expression("HITL argument")?;
                (Some(raw), value)
            } else {
                (None, self.parse_nested_expression("HITL argument")?)
            };
            let arg_span = Span::merge(arg_start, self.prev_span());
            args.push(HitlArg {
                name,
                value,
                span: arg_span,
            });
            self.skip_newlines();
            if self.check(&TokenKind::Comma) {
                self.advance();
                self.skip_newlines();
            } else {
                break;
            }
        }

        self.skip_newlines();
        self.consume(&TokenKind::RParen, ")")?;
        Ok(spanned(
            Node::HitlExpr { kind, args },
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
                    let expr = self.parse_nested_expression("list spread")?;
                    elements.push(spanned(
                        Node::Spread(Box::new(expr)),
                        Span::merge(spread_start, self.prev_span()),
                    ));
                } else {
                    self.pos = saved_pos;
                    elements.push(self.parse_nested_expression("list element")?);
                }
            } else {
                elements.push(self.parse_nested_expression("list element")?);
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
                return_type: None,
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
                        let expr = self.parse_nested_expression("dict spread")?;
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
                let k = self.parse_nested_expression("computed dict key")?;
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
            let value = self.parse_nested_expression("dict value")?;
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
                Some(Box::new(self.parse_nested_expression("parameter default")?))
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
                    let expr = self.parse_nested_expression("spread argument")?;
                    args.push(spanned(
                        Node::Spread(Box::new(expr)),
                        Span::merge(spread_start, self.prev_span()),
                    ));
                } else {
                    self.pos = saved_pos;
                    args.push(self.parse_nested_expression("argument")?);
                }
            } else {
                args.push(self.parse_nested_expression("argument")?);
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
