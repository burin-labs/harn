use crate::ast::*;
use harn_lexer::{Span, Token, TokenKind};
use std::collections::HashSet;

use super::error::ParserError;

/// Recursive descent parser for Harn.
pub struct Parser {
    pub(super) tokens: Vec<Token>,
    pub(super) pos: usize,
    pub(super) errors: Vec<ParserError>,
    pub(super) struct_names: HashSet<String>,
}

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Self {
            tokens,
            pos: 0,
            errors: Vec::new(),
            struct_names: HashSet::new(),
        }
    }

    pub(super) fn current_span(&self) -> Span {
        self.tokens
            .get(self.pos)
            .map(|t| t.span)
            .unwrap_or(Span::dummy())
    }

    pub(super) fn current_kind(&self) -> Option<&TokenKind> {
        self.tokens.get(self.pos).map(|t| &t.kind)
    }

    pub(super) fn prev_span(&self) -> Span {
        if self.pos > 0 {
            self.tokens[self.pos - 1].span
        } else {
            Span::dummy()
        }
    }

    /// Parse a complete .harn file. Reports multiple errors via recovery.
    pub fn parse(&mut self) -> Result<Vec<SNode>, ParserError> {
        let mut nodes = Vec::new();
        self.skip_newlines();

        while !self.is_at_end() {
            // Recovery may leave us pointing at a stray `}` at top level; skip it.
            if self.check(&TokenKind::RBrace) {
                self.advance();
                self.skip_newlines();
                continue;
            }

            let result = if self.check(&TokenKind::Import) {
                self.parse_import()
            } else if self.check(&TokenKind::At) {
                self.parse_attributed_decl()
            } else if self.check(&TokenKind::Pipeline) {
                self.parse_pipeline()
            } else {
                self.parse_statement()
            };

            match result {
                Ok(node) => nodes.push(node),
                Err(err) => {
                    self.errors.push(err);
                    self.synchronize();
                }
            }
            self.skip_newlines();
        }

        if let Some(first) = self.errors.first() {
            return Err(first.clone());
        }
        Ok(nodes)
    }

    /// Return all accumulated parser errors (after `parse()` returns).
    pub fn all_errors(&self) -> &[ParserError] {
        &self.errors
    }

    /// Check if the current token is one that starts a statement.
    pub(super) fn is_statement_start(&self) -> bool {
        matches!(
            self.current_kind(),
            Some(
                TokenKind::Let
                    | TokenKind::Var
                    | TokenKind::If
                    | TokenKind::For
                    | TokenKind::While
                    | TokenKind::Match
                    | TokenKind::Retry
                    | TokenKind::Return
                    | TokenKind::Throw
                    | TokenKind::Fn
                    | TokenKind::Pub
                    | TokenKind::Try
                    | TokenKind::Select
                    | TokenKind::Pipeline
                    | TokenKind::Import
                    | TokenKind::Parallel
                    | TokenKind::Enum
                    | TokenKind::Struct
                    | TokenKind::Interface
                    | TokenKind::Guard
                    | TokenKind::Require
                    | TokenKind::Deadline
                    | TokenKind::Yield
                    | TokenKind::Mutex
                    | TokenKind::Tool
            )
        )
    }

    /// Advance past tokens until we reach a likely statement boundary.
    pub(super) fn synchronize(&mut self) {
        while !self.is_at_end() {
            if self.check(&TokenKind::Newline) {
                self.advance();
                if self.is_at_end() || self.is_statement_start() {
                    return;
                }
                continue;
            }
            if self.check(&TokenKind::RBrace) {
                return;
            }
            self.advance();
        }
    }

    pub(super) fn is_at_end(&self) -> bool {
        self.pos >= self.tokens.len()
            || matches!(self.tokens.get(self.pos), Some(t) if t.kind == TokenKind::Eof)
    }

    pub(super) fn current(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    pub(super) fn peek_kind(&self) -> Option<&TokenKind> {
        self.tokens.get(self.pos + 1).map(|t| &t.kind)
    }

    pub(super) fn peek_kind_at(&self, offset: usize) -> Option<&TokenKind> {
        self.tokens.get(self.pos + offset).map(|t| &t.kind)
    }

    pub(super) fn check(&self, kind: &TokenKind) -> bool {
        self.current()
            .map(|t| std::mem::discriminant(&t.kind) == std::mem::discriminant(kind))
            .unwrap_or(false)
    }

    /// Check for `kind`, skipping newlines first; used for binary operators
    /// like `||` and `&&` that can span lines.
    pub(super) fn check_skip_newlines(&mut self, kind: &TokenKind) -> bool {
        let saved = self.pos;
        self.skip_newlines();
        if self.check(kind) {
            true
        } else {
            self.pos = saved;
            false
        }
    }

    /// Check if current token is an identifier with the given name (without consuming it).
    pub(super) fn check_identifier(&self, name: &str) -> bool {
        matches!(self.current().map(|t| &t.kind), Some(TokenKind::Identifier(s)) if s == name)
    }

    pub(super) fn advance(&mut self) {
        if self.pos < self.tokens.len() {
            self.pos += 1;
        }
    }

    pub(super) fn consume(
        &mut self,
        kind: &TokenKind,
        expected: &str,
    ) -> Result<Token, ParserError> {
        self.skip_newlines();
        let tok = self.current().ok_or_else(|| self.make_error(expected))?;
        if std::mem::discriminant(&tok.kind) != std::mem::discriminant(kind) {
            return Err(self.make_error(expected));
        }
        let tok = tok.clone();
        self.advance();
        Ok(tok)
    }

    pub(super) fn consume_identifier(&mut self, expected: &str) -> Result<String, ParserError> {
        self.skip_newlines();
        let tok = self.current().ok_or_else(|| self.make_error(expected))?;
        if let TokenKind::Identifier(name) = &tok.kind {
            let name = name.clone();
            self.advance();
            Ok(name)
        } else {
            // Distinguish reserved-keyword misuse (e.g. `for tool in list`) from
            // a general unexpected token so the error is actionable.
            let kw_name = harn_lexer::KEYWORDS
                .iter()
                .find(|&&kw| kw == tok.kind.to_string());
            if let Some(kw) = kw_name {
                Err(ParserError::Unexpected {
                    got: format!("'{kw}' (reserved keyword)"),
                    expected: expected.into(),
                    span: tok.span,
                })
            } else {
                Err(self.make_error(expected))
            }
        }
    }

    /// Like `consume_identifier`, but also accepts keywords as identifiers.
    /// Used for property access (e.g., `obj.type`) and dict keys where
    /// keywords are valid member names.
    pub(super) fn consume_identifier_or_keyword(
        &mut self,
        expected: &str,
    ) -> Result<String, ParserError> {
        self.skip_newlines();
        let tok = self.current().ok_or_else(|| self.make_error(expected))?;
        if let TokenKind::Identifier(name) = &tok.kind {
            let name = name.clone();
            self.advance();
            return Ok(name);
        }
        let name = match &tok.kind {
            TokenKind::Pipeline => "pipeline",
            TokenKind::Extends => "extends",
            TokenKind::Override => "override",
            TokenKind::Let => "let",
            TokenKind::Var => "var",
            TokenKind::If => "if",
            TokenKind::Else => "else",
            TokenKind::For => "for",
            TokenKind::In => "in",
            TokenKind::Match => "match",
            TokenKind::Retry => "retry",
            TokenKind::Parallel => "parallel",
            TokenKind::Return => "return",
            TokenKind::Import => "import",
            TokenKind::True => "true",
            TokenKind::False => "false",
            TokenKind::Nil => "nil",
            TokenKind::Try => "try",
            TokenKind::Catch => "catch",
            TokenKind::Throw => "throw",
            TokenKind::Fn => "fn",
            TokenKind::Spawn => "spawn",
            TokenKind::While => "while",
            TokenKind::TypeKw => "type",
            TokenKind::Enum => "enum",
            TokenKind::Struct => "struct",
            TokenKind::Interface => "interface",
            TokenKind::Pub => "pub",
            TokenKind::From => "from",
            TokenKind::To => "to",
            TokenKind::Tool => "tool",
            TokenKind::Exclusive => "exclusive",
            TokenKind::Guard => "guard",
            TokenKind::Deadline => "deadline",
            TokenKind::Defer => "defer",
            TokenKind::Yield => "yield",
            TokenKind::Mutex => "mutex",
            TokenKind::Break => "break",
            TokenKind::Continue => "continue",
            TokenKind::Impl => "impl",
            _ => return Err(self.make_error(expected)),
        };
        let name = name.to_string();
        self.advance();
        Ok(name)
    }

    pub(super) fn skip_newlines(&mut self) {
        while self.pos < self.tokens.len() && self.tokens[self.pos].kind == TokenKind::Newline {
            self.pos += 1;
        }
    }

    pub(super) fn make_error(&self, expected: &str) -> ParserError {
        if let Some(tok) = self.tokens.get(self.pos) {
            if tok.kind == TokenKind::Eof {
                return ParserError::UnexpectedEof {
                    expected: expected.into(),
                    span: tok.span,
                };
            }
            ParserError::Unexpected {
                got: tok.kind.to_string(),
                expected: expected.into(),
                span: tok.span,
            }
        } else {
            ParserError::UnexpectedEof {
                expected: expected.into(),
                span: self.prev_span(),
            }
        }
    }

    pub(super) fn error(&self, expected: &str) -> ParserError {
        self.make_error(expected)
    }
}
