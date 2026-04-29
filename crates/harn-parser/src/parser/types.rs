use crate::ast::*;
use harn_lexer::TokenKind;

use super::error::ParserError;
use super::state::Parser;

impl Parser {
    /// Parse a comma-separated list of type parameters until `>`.
    ///
    /// Each parameter may be prefixed with a variance marker:
    /// `in T` (contravariant) or `out T` (covariant). Unannotated
    /// parameters default to `Invariant`.
    pub(super) fn parse_type_param_list(&mut self) -> Result<Vec<TypeParam>, ParserError> {
        let mut params = Vec::new();
        self.skip_newlines();
        while !self.is_at_end() && !self.check(&TokenKind::Gt) {
            let variance = self.parse_optional_variance_marker();
            let name = self.consume_identifier("type parameter name")?;
            params.push(TypeParam { name, variance });
            if self.check(&TokenKind::Comma) {
                self.advance();
                self.skip_newlines();
            }
        }
        self.consume(&TokenKind::Gt, ">")?;
        Ok(params)
    }

    /// Consume an optional `in` / `out` variance marker at the start
    /// of a type parameter. `in` is a reserved keyword and so is
    /// always a marker when it appears here. `out` is a contextual
    /// keyword: it is a marker only when followed by another
    /// identifier (otherwise it is the parameter name itself).
    pub(super) fn parse_optional_variance_marker(&mut self) -> Variance {
        if self.check(&TokenKind::In) {
            self.advance();
            return Variance::Contravariant;
        }
        if self.check_identifier("out") {
            if let Some(kind) = self.peek_kind() {
                if matches!(kind, TokenKind::Identifier(_)) {
                    self.advance();
                    return Variance::Covariant;
                }
            }
        }
        Variance::Invariant
    }

    /// Parse an optional `where T: bound, U: bound` clause.
    pub(super) fn parse_where_clauses(&mut self) -> Result<Vec<WhereClause>, ParserError> {
        if let Some(tok) = self.current() {
            if let TokenKind::Identifier(ref id) = tok.kind {
                if id == "where" {
                    self.advance();
                    let mut clauses = Vec::new();
                    loop {
                        self.skip_newlines();
                        if self.check(&TokenKind::LBrace) || self.is_at_end() {
                            break;
                        }
                        let type_name = self.consume_identifier("type parameter name")?;
                        self.consume(&TokenKind::Colon, ":")?;
                        let bound = self.consume_identifier("type bound")?;
                        clauses.push(WhereClause { type_name, bound });
                        if self.check(&TokenKind::Comma) {
                            self.advance();
                        } else {
                            break;
                        }
                    }
                    return Ok(clauses);
                }
            }
        }
        Ok(Vec::new())
    }

    /// Parse an optional `: type` annotation. `None` when no colon follows.
    pub(super) fn try_parse_type_annotation(&mut self) -> Result<Option<TypeExpr>, ParserError> {
        if !self.check(&TokenKind::Colon) {
            return Ok(None);
        }
        self.advance();
        Ok(Some(self.parse_type_expr()?))
    }

    /// Parse a type expression: `int`, `string | nil`, `{name: string, age?: int}`.
    pub(super) fn parse_type_expr(&mut self) -> Result<TypeExpr, ParserError> {
        self.skip_newlines();
        let first = self.parse_type_primary()?;

        if self.check(&TokenKind::Bar) {
            let mut types = vec![first];
            while self.check(&TokenKind::Bar) {
                self.advance();
                types.push(self.parse_type_primary()?);
            }
            return Ok(TypeExpr::Union(types));
        }

        Ok(first)
    }

    /// Accepts identifiers and the `nil`/`true`/`false` keywords as type names.
    pub(super) fn parse_type_primary(&mut self) -> Result<TypeExpr, ParserError> {
        self.skip_newlines();
        if self.check(&TokenKind::LBrace) {
            return self.parse_shape_type();
        }
        if let Some(tok) = self.current() {
            match &tok.kind {
                TokenKind::Nil => {
                    self.advance();
                    return Ok(TypeExpr::Named("nil".to_string()));
                }
                TokenKind::True | TokenKind::False => {
                    self.advance();
                    return Ok(TypeExpr::Named("bool".to_string()));
                }
                TokenKind::StringLiteral(text) | TokenKind::RawStringLiteral(text) => {
                    let text = text.clone();
                    self.advance();
                    return Ok(TypeExpr::LitString(text));
                }
                TokenKind::IntLiteral(value) => {
                    let value = *value;
                    self.advance();
                    return Ok(TypeExpr::LitInt(value));
                }
                TokenKind::Minus => {
                    // Allow negative int literals: `-1 | 0 | 1`.
                    if let Some(TokenKind::IntLiteral(v)) = self.peek_kind_at(1) {
                        let v = *v;
                        self.advance();
                        self.advance();
                        return Ok(TypeExpr::LitInt(-v));
                    }
                }
                _ => {}
            }
        }
        if self.check(&TokenKind::Fn) {
            self.advance();
            self.consume(&TokenKind::LParen, "(")?;
            let mut params = Vec::new();
            self.skip_newlines();
            while !self.is_at_end() && !self.check(&TokenKind::RParen) {
                params.push(self.parse_type_expr()?);
                self.skip_newlines();
                if self.check(&TokenKind::Comma) {
                    self.advance();
                    self.skip_newlines();
                }
            }
            self.consume(&TokenKind::RParen, ")")?;
            self.consume(&TokenKind::Arrow, "->")?;
            let return_type = self.parse_type_expr()?;
            return Ok(TypeExpr::FnType {
                params,
                return_type: Box::new(return_type),
            });
        }
        let name = self.consume_identifier("type name")?;
        if name == "never" {
            return Ok(TypeExpr::Never);
        }
        if self.check(&TokenKind::Lt) {
            self.advance();
            let mut type_args = vec![self.parse_type_expr()?];
            while self.check(&TokenKind::Comma) {
                self.advance();
                type_args.push(self.parse_type_expr()?);
            }
            self.consume(&TokenKind::Gt, ">")?;
            if name == "list" && type_args.len() == 1 {
                return Ok(TypeExpr::List(Box::new(type_args.remove(0))));
            } else if name == "dict" && type_args.len() == 2 {
                return Ok(TypeExpr::DictType(
                    Box::new(type_args.remove(0)),
                    Box::new(type_args.remove(0)),
                ));
            } else if (name == "iter" || name == "Iter") && type_args.len() == 1 {
                return Ok(TypeExpr::Iter(Box::new(type_args.remove(0))));
            } else if (name == "Generator" || name == "generator") && type_args.len() == 1 {
                return Ok(TypeExpr::Generator(Box::new(type_args.remove(0))));
            } else if (name == "Stream" || name == "stream") && type_args.len() == 1 {
                return Ok(TypeExpr::Stream(Box::new(type_args.remove(0))));
            }
            return Ok(TypeExpr::Applied {
                name,
                args: type_args,
            });
        }
        Ok(TypeExpr::Named(name))
    }

    /// Parse a shape type: `{ name: string, age: int, active?: bool }`.
    pub(super) fn parse_shape_type(&mut self) -> Result<TypeExpr, ParserError> {
        self.consume(&TokenKind::LBrace, "{")?;
        let mut fields = Vec::new();
        self.skip_newlines();

        while !self.is_at_end() && !self.check(&TokenKind::RBrace) {
            // Shape field names parallel dict-literal keys: a few reserved
            // keywords (`type`, `match`, …) are common discriminant names
            // and must work in shape-type position too.
            let name = self.consume_identifier_or_keyword("field name")?;
            let optional = if self.check(&TokenKind::Question) {
                self.advance();
                true
            } else {
                false
            };
            self.consume(&TokenKind::Colon, ":")?;
            let type_expr = self.parse_type_expr()?;
            fields.push(ShapeField {
                name,
                type_expr,
                optional,
            });
            self.skip_newlines();
            if self.check(&TokenKind::Comma) {
                self.advance();
                self.skip_newlines();
            }
        }

        self.consume(&TokenKind::RBrace, "}")?;
        Ok(TypeExpr::Shape(fields))
    }
}
