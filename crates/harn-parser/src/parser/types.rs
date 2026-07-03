use crate::ast::*;
use harn_lexer::TokenKind;

use super::error::ParserError;
use super::state::Parser;

impl Parser {
    pub(super) fn parse_nested_type_expr(
        &mut self,
        context: &'static str,
    ) -> Result<TypeExpr, ParserError> {
        self.with_nesting(context, |parser| parser.parse_type_expr())
    }

    /// Parse a comma-separated list of type parameters until `>`.
    ///
    /// Each parameter may be prefixed with a variance marker:
    /// `in T` (contravariant) or `out T` (covariant). Unannotated
    /// parameters default to `Invariant`.
    pub(super) fn parse_type_param_list(&mut self) -> Result<Vec<TypeParam>, ParserError> {
        let mut params = Vec::new();
        self.skip_newlines();
        if self.check(&TokenKind::Gt) {
            return Err(self.error("type parameter name"));
        }
        loop {
            let variance = self.parse_optional_variance_marker();
            let name = self.consume_identifier("type parameter name")?;
            params.push(TypeParam { name, variance });
            self.skip_newlines();
            if self.check(&TokenKind::Comma) {
                self.advance();
                self.skip_newlines();
                if self.check(&TokenKind::Gt) || self.is_at_end() {
                    return Err(self.error("type parameter name"));
                }
            } else {
                break;
            }
        }
        self.consume(&TokenKind::Gt, ">")?;
        Ok(params)
    }

    /// Parse a comma-separated type argument list after the opening `<`.
    pub(super) fn parse_type_arg_list(&mut self) -> Result<Vec<TypeExpr>, ParserError> {
        let mut args = Vec::new();
        self.skip_newlines();
        if self.check(&TokenKind::Gt) {
            return Err(self.error("type argument"));
        }
        while !self.is_at_end() && !self.check(&TokenKind::Gt) {
            args.push(self.parse_nested_type_expr("type argument")?);
            self.skip_newlines();
            if self.check(&TokenKind::Comma) {
                self.advance();
                self.skip_newlines();
                if self.check(&TokenKind::Gt) || self.is_at_end() {
                    return Err(self.error("type argument"));
                }
            } else {
                break;
            }
        }
        self.consume(&TokenKind::Gt, ">")?;
        Ok(args)
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
                    self.skip_newlines();
                    if self.check(&TokenKind::LBrace) || self.is_at_end() {
                        return Err(self.error("where clause"));
                    }
                    loop {
                        self.skip_newlines();
                        if self.check(&TokenKind::LBrace) || self.is_at_end() {
                            break;
                        }
                        let type_name = self.consume_identifier("type parameter name")?;
                        self.consume(&TokenKind::Colon, ":")?;
                        // A bound may be additive: `where T: A + B` constrains T
                        // by both A and B. Desugar each additive term into its
                        // own clause so downstream consumers see a flat list.
                        loop {
                            let bound = self.parse_nested_type_expr("type bound")?;
                            clauses.push(WhereClause {
                                type_name: type_name.clone(),
                                bound,
                            });
                            if self.check(&TokenKind::Plus) {
                                self.advance();
                                self.skip_newlines();
                            } else {
                                break;
                            }
                        }
                        if self.check(&TokenKind::Comma) {
                            self.advance();
                            self.skip_newlines();
                            if self.check(&TokenKind::LBrace) || self.is_at_end() {
                                return Err(self.error("where clause"));
                            }
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

    /// Parse a type expression: `int`, `string | nil`, `A & B`,
    /// `{name: string, age?: int}`, `int?`. `&` binds tighter than `|`,
    /// and postfix `?` (sugar for `T | nil`) binds tightest of all, so
    /// `A & B | C` parses as `(A & B) | C` and `A | B?` parses as
    /// `A | (B | nil)` — flattened to `A | B | nil`.
    pub(super) fn parse_type_expr(&mut self) -> Result<TypeExpr, ParserError> {
        self.skip_newlines();
        let first = self.parse_type_intersection()?;

        if self.check(&TokenKind::Bar) {
            let mut types: Vec<TypeExpr> = Vec::new();
            flatten_union_into(&mut types, first);
            while self.check(&TokenKind::Bar) {
                self.advance();
                let next = self.parse_type_intersection()?;
                flatten_union_into(&mut types, next);
            }
            dedupe_in_order(&mut types);
            if types.len() == 1 {
                return Ok(types.into_iter().next().unwrap());
            }
            return Ok(TypeExpr::Union(types));
        }

        Ok(first)
    }

    /// Parse `A & B & C`. Each component is a `parse_type_primary`
    /// (no nested `|` without parentheses), preserving the precedence
    /// `&` > `|`.
    fn parse_type_intersection(&mut self) -> Result<TypeExpr, ParserError> {
        let first = self.parse_type_primary()?;
        if !self.check(&TokenKind::Amp) {
            return Ok(first);
        }
        let mut types = vec![first];
        while self.check(&TokenKind::Amp) {
            self.advance();
            types.push(self.parse_type_primary()?);
        }
        Ok(TypeExpr::Intersection(types))
    }

    /// Accepts identifiers and the `nil`/`true`/`false` keywords as type names.
    /// Handles postfix `?` as sugar for `T | nil` after the base primary.
    pub(super) fn parse_type_primary(&mut self) -> Result<TypeExpr, ParserError> {
        let base = self.parse_type_primary_base()?;
        Ok(self.attach_optional_postfix(base))
    }

    /// Apply trailing `?` markers to the base type. Stacked `?` (e.g.
    /// `T??`) collapses idempotently because `T | nil | nil` simplifies
    /// to `T | nil` after dedupe in `parse_type_expr`. We also avoid
    /// re-wrapping `nil?` (which would produce `nil | nil`) and types
    /// whose union already contains `nil`.
    fn attach_optional_postfix(&mut self, mut ty: TypeExpr) -> TypeExpr {
        while self.check(&TokenKind::Question) {
            self.advance();
            ty = wrap_optional(ty);
        }
        ty
    }

    fn parse_type_primary_base(&mut self) -> Result<TypeExpr, ParserError> {
        self.skip_newlines();
        if self.check(&TokenKind::LBrace) {
            return self.parse_shape_type();
        }
        if self.check(&TokenKind::LBracket) {
            self.advance();
            let inner = self.parse_nested_type_expr("list type")?;
            self.consume(&TokenKind::RBracket, "]")?;
            return Ok(TypeExpr::List(Box::new(inner)));
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
                params.push(self.parse_nested_type_expr("function parameter type")?);
                self.skip_newlines();
                if self.check(&TokenKind::Comma) {
                    self.advance();
                    self.skip_newlines();
                }
            }
            self.consume(&TokenKind::RParen, ")")?;
            self.consume(&TokenKind::Arrow, "->")?;
            let return_type = self.parse_nested_type_expr("function return type")?;
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
            let mut type_args = self.parse_type_arg_list()?;
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
            } else if name == "owned" && type_args.len() == 1 {
                return Ok(TypeExpr::Owned(Box::new(type_args.remove(0))));
            }
            return Ok(TypeExpr::Applied {
                name,
                args: type_args,
            });
        }
        Ok(TypeExpr::Named(name))
    }

    /// True when the cursor sits on a `...` ellipsis (three `.` tokens),
    /// used to introduce a row tail in a shape type (`{a: T, ...R}`).
    fn at_ellipsis(&self) -> bool {
        self.check(&TokenKind::Dot)
            && self
                .tokens
                .get(self.pos + 1)
                .is_some_and(|t| t.kind == TokenKind::Dot)
            && self
                .tokens
                .get(self.pos + 2)
                .is_some_and(|t| t.kind == TokenKind::Dot)
    }

    /// Parse a shape type: `{ name: string, age: int, active?: bool }`, or an
    /// **open** record with one or more trailing row tails:
    /// `{ id: string, ...R }`, `{ ...R1, ...R2 }`. A tail is `...` followed by
    /// a type — a bare identifier is a row variable, but `...dict<string, V>`
    /// (a gradual map tail) is also accepted.
    pub(super) fn parse_shape_type(&mut self) -> Result<TypeExpr, ParserError> {
        self.consume(&TokenKind::LBrace, "{")?;
        let mut fields = Vec::new();
        let mut rests: Vec<TypeExpr> = Vec::new();
        self.skip_newlines();

        while !self.is_at_end() && !self.check(&TokenKind::RBrace) {
            if self.at_ellipsis() {
                self.advance();
                self.advance();
                self.advance();
                let tail = self.parse_nested_type_expr("row tail")?;
                rests.push(tail);
                self.skip_newlines();
                if self.check(&TokenKind::Comma) {
                    self.advance();
                    self.skip_newlines();
                }
                continue;
            }
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
            let type_expr = self.parse_nested_type_expr("shape field type")?;
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
        if rests.is_empty() {
            Ok(TypeExpr::Shape(fields))
        } else {
            Ok(TypeExpr::OpenShape { fields, rests })
        }
    }
}

fn is_nil_named(ty: &TypeExpr) -> bool {
    matches!(ty, TypeExpr::Named(n) if n == "nil")
}

fn union_contains_nil(types: &[TypeExpr]) -> bool {
    types.iter().any(is_nil_named)
}

/// Wrap `ty` in `T | nil` unless it already includes `nil`.
fn wrap_optional(ty: TypeExpr) -> TypeExpr {
    if is_nil_named(&ty) {
        return ty;
    }
    if let TypeExpr::Union(members) = &ty {
        if union_contains_nil(members) {
            return ty;
        }
    }
    TypeExpr::Union(vec![ty, TypeExpr::Named("nil".to_string())])
}

/// Append `ty` (or its members, recursively, if it is a `Union`) to `out`.
/// Used so postfix `?` and explicit `|` arms compose flatly: `T? | U`
/// becomes `T | nil | U` rather than a nested union.
fn flatten_union_into(out: &mut Vec<TypeExpr>, ty: TypeExpr) {
    match ty {
        TypeExpr::Union(members) => {
            for m in members {
                flatten_union_into(out, m);
            }
        }
        other => out.push(other),
    }
}

/// Remove duplicate type expressions while preserving order, so
/// `T | T?` collapses to `T | nil` rather than `T | T | nil`.
fn dedupe_in_order(types: &mut Vec<TypeExpr>) {
    let mut seen: Vec<TypeExpr> = Vec::with_capacity(types.len());
    types.retain(|t| {
        if seen.contains(t) {
            false
        } else {
            seen.push(t.clone());
            true
        }
    });
}
