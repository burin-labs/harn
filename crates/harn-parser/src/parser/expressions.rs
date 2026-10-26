use crate::ast::*;
use harn_lexer::{Span, TokenKind};

use super::error::ParserError;
use super::state::Parser;

impl Parser {
    pub(super) fn parse_primary(&mut self) -> Result<SNode, ParserError> {
        #[cfg(test)]
        self.record_stack_position();
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
                if name == "cost_route" && self.check_contextual_construct("cost_route") =>
            {
                self.parse_cost_route()
            }
            TokenKind::Identifier(name)
                if name == "deadline" && self.check_contextual_construct("deadline") =>
            {
                self.parse_deadline()
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
                let mut expr = self.with_nesting("parenthesized expression", |parser| {
                    let expr = parser.parse_expression()?;
                    parser.consume(&TokenKind::RParen, ")")?;
                    Ok(expr)
                })?;
                // The span covers the parentheses so a parent that starts at
                // this operand, and any fix that splices its source, stays
                // balanced: `(a + b) * c` must not read as `a + b) * c`.
                expr.span = Span::merge(start, self.prev_span());
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
        // Mirror the return type's nested-parse so a `throws E { body }` clause
        // stops cleanly at the closure body's `{`.
        let throws = if self.check(&TokenKind::Throws) {
            self.advance();
            Some(self.parse_nested_type_expr("closure throws type")?)
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
                throws,
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
                // The bare `x -> expr` arrow form has no signature slot for a
                // `throws` clause; only `fn(params) -> R throws E` can carry one.
                throws: None,
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
            let param_start = self.current_span();
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
                span: Span::merge(param_start, self.prev_span()),
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
