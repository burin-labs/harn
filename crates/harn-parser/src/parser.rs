use crate::ast::*;
use harn_lexer::{Span, Token, TokenKind};
use std::collections::HashSet;
use std::fmt;

/// Parser errors.
#[derive(Debug, Clone, PartialEq)]
pub enum ParserError {
    Unexpected {
        got: String,
        expected: String,
        span: Span,
    },
    UnexpectedEof {
        expected: String,
        span: Span,
    },
}

impl fmt::Display for ParserError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParserError::Unexpected {
                got,
                expected,
                span,
            } => write!(
                f,
                "Expected {expected}, got {got} at {}:{}",
                span.line, span.column
            ),
            ParserError::UnexpectedEof { expected, .. } => {
                write!(f, "Unexpected end of file, expected {expected}")
            }
        }
    }
}

impl std::error::Error for ParserError {}

/// Recursive descent parser for Harn.
pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    errors: Vec<ParserError>,
    struct_names: HashSet<String>,
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

    fn current_span(&self) -> Span {
        self.tokens
            .get(self.pos)
            .map(|t| t.span)
            .unwrap_or(Span::dummy())
    }

    fn current_kind(&self) -> Option<&TokenKind> {
        self.tokens.get(self.pos).map(|t| &t.kind)
    }

    fn prev_span(&self) -> Span {
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
    fn is_statement_start(&self) -> bool {
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
    fn synchronize(&mut self) {
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

    /// Parse a single expression (for string interpolation).
    pub fn parse_single_expression(&mut self) -> Result<SNode, ParserError> {
        self.skip_newlines();
        self.parse_expression()
    }

    fn parse_pipeline_with_pub(&mut self, is_pub: bool) -> Result<SNode, ParserError> {
        let start = self.current_span();
        self.consume(&TokenKind::Pipeline, "pipeline")?;
        let name = self.consume_identifier("pipeline name")?;

        self.consume(&TokenKind::LParen, "(")?;
        let params = self.parse_param_list()?;
        self.consume(&TokenKind::RParen, ")")?;

        let extends = if self.check(&TokenKind::Extends) {
            self.advance();
            Some(self.consume_identifier("parent pipeline name")?)
        } else {
            None
        };

        self.consume(&TokenKind::LBrace, "{")?;
        let body = self.parse_block()?;
        self.consume(&TokenKind::RBrace, "}")?;

        Ok(spanned(
            Node::Pipeline {
                name,
                params,
                body,
                extends,
                is_pub,
            },
            Span::merge(start, self.prev_span()),
        ))
    }

    fn parse_pipeline(&mut self) -> Result<SNode, ParserError> {
        self.parse_pipeline_with_pub(false)
    }

    fn parse_import(&mut self) -> Result<SNode, ParserError> {
        let start = self.current_span();
        self.consume(&TokenKind::Import, "import")?;

        // Selective import: `import { foo, bar } from "module"`.
        if self.check(&TokenKind::LBrace) {
            self.advance();
            self.skip_newlines();
            let mut names = Vec::new();
            while !self.is_at_end() && !self.check(&TokenKind::RBrace) {
                let name = self.consume_identifier("import name")?;
                names.push(name);
                self.skip_newlines();
                if self.check(&TokenKind::Comma) {
                    self.advance();
                    self.skip_newlines();
                }
            }
            self.consume(&TokenKind::RBrace, "}")?;
            self.consume(&TokenKind::From, "from")?;
            if let Some(tok) = self.current() {
                if let TokenKind::StringLiteral(path) = &tok.kind {
                    let path = path.clone();
                    self.advance();
                    return Ok(spanned(
                        Node::SelectiveImport { names, path },
                        Span::merge(start, self.prev_span()),
                    ));
                }
            }
            return Err(self.error("import path string"));
        }

        if let Some(tok) = self.current() {
            if let TokenKind::StringLiteral(path) = &tok.kind {
                let path = path.clone();
                self.advance();
                return Ok(spanned(
                    Node::ImportDecl { path },
                    Span::merge(start, self.prev_span()),
                ));
            }
        }
        Err(self.error("import path string"))
    }

    fn parse_block(&mut self) -> Result<Vec<SNode>, ParserError> {
        let mut stmts = Vec::new();
        self.skip_newlines();

        while !self.is_at_end() && !self.check(&TokenKind::RBrace) {
            stmts.push(self.parse_statement()?);
            self.skip_newlines();
        }
        Ok(stmts)
    }

    fn parse_statement(&mut self) -> Result<SNode, ParserError> {
        self.skip_newlines();

        let tok = self.current().ok_or_else(|| ParserError::UnexpectedEof {
            expected: "statement".into(),
            span: self.prev_span(),
        })?;

        match &tok.kind {
            TokenKind::Let => self.parse_let_binding(),
            TokenKind::Var => self.parse_var_binding(),
            TokenKind::If => self.parse_if_else(),
            TokenKind::For => self.parse_for_in(),
            TokenKind::Match => self.parse_match(),
            TokenKind::Retry => self.parse_retry(),
            TokenKind::While => self.parse_while_loop(),
            TokenKind::Parallel => self.parse_parallel(),
            TokenKind::Return => self.parse_return(),
            TokenKind::Throw => self.parse_throw(),
            TokenKind::Override => self.parse_override(),
            TokenKind::Try => self.parse_try_catch(),
            TokenKind::Select => self.parse_select(),
            TokenKind::Fn => self.parse_fn_decl_with_pub(false),
            TokenKind::Tool => self.parse_tool_decl(false),
            TokenKind::Pub => {
                self.advance(); // consume 'pub'
                let tok = self.current().ok_or_else(|| ParserError::UnexpectedEof {
                    expected: "fn, struct, enum, or pipeline after pub".into(),
                    span: self.prev_span(),
                })?;
                match &tok.kind {
                    TokenKind::Fn => self.parse_fn_decl_with_pub(true),
                    TokenKind::Tool => self.parse_tool_decl(true),
                    TokenKind::Pipeline => self.parse_pipeline_with_pub(true),
                    TokenKind::Enum => self.parse_enum_decl_with_pub(true),
                    TokenKind::Struct => self.parse_struct_decl_with_pub(true),
                    _ => Err(self.error("fn, tool, struct, enum, or pipeline after pub")),
                }
            }
            TokenKind::TypeKw => self.parse_type_decl(),
            TokenKind::Enum => self.parse_enum_decl(),
            TokenKind::Struct => self.parse_struct_decl(),
            TokenKind::Interface => self.parse_interface_decl(),
            TokenKind::Impl => self.parse_impl_block(),
            TokenKind::Guard => self.parse_guard(),
            TokenKind::Require => self.parse_require(),
            TokenKind::Deadline => self.parse_deadline(),
            TokenKind::Yield => self.parse_yield(),
            TokenKind::Mutex => self.parse_mutex(),
            TokenKind::Defer => self.parse_defer(),
            TokenKind::Break => {
                let span = self.current_span();
                self.advance();
                Ok(spanned(Node::BreakStmt, span))
            }
            TokenKind::Continue => {
                let span = self.current_span();
                self.advance();
                Ok(spanned(Node::ContinueStmt, span))
            }
            _ => self.parse_expression_statement(),
        }
    }

    fn parse_let_binding(&mut self) -> Result<SNode, ParserError> {
        let start = self.current_span();
        self.consume(&TokenKind::Let, "let")?;
        let pattern = self.parse_binding_pattern()?;
        let type_ann = if matches!(pattern, BindingPattern::Identifier(_)) {
            self.try_parse_type_annotation()?
        } else {
            None
        };
        self.consume(&TokenKind::Assign, "=")?;
        let value = self.parse_expression()?;
        Ok(spanned(
            Node::LetBinding {
                pattern,
                type_ann,
                value: Box::new(value),
            },
            Span::merge(start, self.prev_span()),
        ))
    }

    fn parse_var_binding(&mut self) -> Result<SNode, ParserError> {
        let start = self.current_span();
        self.consume(&TokenKind::Var, "var")?;
        let pattern = self.parse_binding_pattern()?;
        let type_ann = if matches!(pattern, BindingPattern::Identifier(_)) {
            self.try_parse_type_annotation()?
        } else {
            None
        };
        self.consume(&TokenKind::Assign, "=")?;
        let value = self.parse_expression()?;
        Ok(spanned(
            Node::VarBinding {
                pattern,
                type_ann,
                value: Box::new(value),
            },
            Span::merge(start, self.prev_span()),
        ))
    }

    fn parse_if_else(&mut self) -> Result<SNode, ParserError> {
        let start = self.current_span();
        self.consume(&TokenKind::If, "if")?;
        let condition = self.parse_expression()?;
        self.consume(&TokenKind::LBrace, "{")?;
        let then_body = self.parse_block()?;
        self.consume(&TokenKind::RBrace, "}")?;
        self.skip_newlines();

        let else_body = if self.check(&TokenKind::Else) {
            self.advance();
            if self.check(&TokenKind::If) {
                Some(vec![self.parse_if_else()?])
            } else {
                self.consume(&TokenKind::LBrace, "{")?;
                let body = self.parse_block()?;
                self.consume(&TokenKind::RBrace, "}")?;
                Some(body)
            }
        } else {
            None
        };

        Ok(spanned(
            Node::IfElse {
                condition: Box::new(condition),
                then_body,
                else_body,
            },
            Span::merge(start, self.prev_span()),
        ))
    }

    fn parse_for_in(&mut self) -> Result<SNode, ParserError> {
        let start = self.current_span();
        self.consume(&TokenKind::For, "for")?;
        let pattern = if self.check(&TokenKind::LParen) {
            // `for (a, b) in ...` pair destructuring.
            self.advance();
            let first = self.consume_identifier("pair pattern element")?;
            self.consume(&TokenKind::Comma, ",")?;
            let second = self.consume_identifier("pair pattern element")?;
            self.consume(&TokenKind::RParen, ")")?;
            BindingPattern::Pair(first, second)
        } else {
            self.parse_binding_pattern()?
        };
        self.consume(&TokenKind::In, "in")?;
        let iterable = self.parse_expression()?;
        self.consume(&TokenKind::LBrace, "{")?;
        let body = self.parse_block()?;
        self.consume(&TokenKind::RBrace, "}")?;
        Ok(spanned(
            Node::ForIn {
                pattern,
                iterable: Box::new(iterable),
                body,
            },
            Span::merge(start, self.prev_span()),
        ))
    }

    /// Parse a binding pattern for let/var/for-in:
    ///   identifier | { fields } | [ elements ]
    fn parse_binding_pattern(&mut self) -> Result<BindingPattern, ParserError> {
        self.skip_newlines();
        if self.check(&TokenKind::LBrace) {
            self.advance();
            let mut fields = Vec::new();
            while !self.is_at_end() && !self.check(&TokenKind::RBrace) {
                if self.check(&TokenKind::Dot) {
                    self.advance();
                    self.consume(&TokenKind::Dot, ".")?;
                    self.consume(&TokenKind::Dot, ".")?;
                    let name = self.consume_identifier("rest variable name")?;
                    fields.push(DictPatternField {
                        key: name,
                        alias: None,
                        is_rest: true,
                        default_value: None,
                    });
                    // Rest pattern must be the last element.
                    break;
                }
                let key = self.consume_identifier("field name")?;
                let alias = if self.check(&TokenKind::Colon) {
                    self.advance();
                    Some(self.consume_identifier("alias name")?)
                } else {
                    None
                };
                let default_value = if self.check(&TokenKind::Assign) {
                    self.advance();
                    Some(Box::new(self.parse_expression()?))
                } else {
                    None
                };
                fields.push(DictPatternField {
                    key,
                    alias,
                    is_rest: false,
                    default_value,
                });
                if self.check(&TokenKind::Comma) {
                    self.advance();
                }
            }
            self.consume(&TokenKind::RBrace, "}")?;
            Ok(BindingPattern::Dict(fields))
        } else if self.check(&TokenKind::LBracket) {
            self.advance();
            let mut elements = Vec::new();
            while !self.is_at_end() && !self.check(&TokenKind::RBracket) {
                if self.check(&TokenKind::Dot) {
                    self.advance();
                    self.consume(&TokenKind::Dot, ".")?;
                    self.consume(&TokenKind::Dot, ".")?;
                    let name = self.consume_identifier("rest variable name")?;
                    elements.push(ListPatternElement {
                        name,
                        is_rest: true,
                        default_value: None,
                    });
                    break;
                }
                let name = self.consume_identifier("element name")?;
                let default_value = if self.check(&TokenKind::Assign) {
                    self.advance();
                    Some(Box::new(self.parse_expression()?))
                } else {
                    None
                };
                elements.push(ListPatternElement {
                    name,
                    is_rest: false,
                    default_value,
                });
                if self.check(&TokenKind::Comma) {
                    self.advance();
                }
            }
            self.consume(&TokenKind::RBracket, "]")?;
            Ok(BindingPattern::List(elements))
        } else {
            let name = self.consume_identifier("variable name or destructuring pattern")?;
            Ok(BindingPattern::Identifier(name))
        }
    }

    fn parse_match(&mut self) -> Result<SNode, ParserError> {
        let start = self.current_span();
        self.consume(&TokenKind::Match, "match")?;
        let value = self.parse_expression()?;
        self.consume(&TokenKind::LBrace, "{")?;
        self.skip_newlines();

        let mut arms = Vec::new();
        while !self.is_at_end() && !self.check(&TokenKind::RBrace) {
            let pattern = self.parse_expression()?;
            let guard = if self.check(&TokenKind::If) {
                self.advance();
                Some(Box::new(self.parse_expression()?))
            } else {
                None
            };
            self.consume(&TokenKind::Arrow, "->")?;
            self.consume(&TokenKind::LBrace, "{")?;
            let body = self.parse_block()?;
            self.consume(&TokenKind::RBrace, "}")?;
            arms.push(MatchArm {
                pattern,
                guard,
                body,
            });
            self.skip_newlines();
        }

        self.consume(&TokenKind::RBrace, "}")?;
        Ok(spanned(
            Node::MatchExpr {
                value: Box::new(value),
                arms,
            },
            Span::merge(start, self.prev_span()),
        ))
    }

    fn parse_while_loop(&mut self) -> Result<SNode, ParserError> {
        let start = self.current_span();
        self.consume(&TokenKind::While, "while")?;
        let condition = if self.check(&TokenKind::LParen) {
            self.advance();
            let c = self.parse_expression()?;
            self.consume(&TokenKind::RParen, ")")?;
            c
        } else {
            self.parse_expression()?
        };
        self.consume(&TokenKind::LBrace, "{")?;
        let body = self.parse_block()?;
        self.consume(&TokenKind::RBrace, "}")?;
        Ok(spanned(
            Node::WhileLoop {
                condition: Box::new(condition),
                body,
            },
            Span::merge(start, self.prev_span()),
        ))
    }

    fn parse_retry(&mut self) -> Result<SNode, ParserError> {
        let start = self.current_span();
        self.consume(&TokenKind::Retry, "retry")?;
        let count = if self.check(&TokenKind::LParen) {
            self.advance();
            let c = self.parse_expression()?;
            self.consume(&TokenKind::RParen, ")")?;
            c
        } else {
            self.parse_primary()?
        };
        self.consume(&TokenKind::LBrace, "{")?;
        let body = self.parse_block()?;
        self.consume(&TokenKind::RBrace, "}")?;
        Ok(spanned(
            Node::Retry {
                count: Box::new(count),
                body,
            },
            Span::merge(start, self.prev_span()),
        ))
    }

    fn parse_parallel(&mut self) -> Result<SNode, ParserError> {
        let start = self.current_span();
        self.consume(&TokenKind::Parallel, "parallel")?;

        let mode = if self.check_identifier("each") {
            self.advance();
            ParallelMode::Each
        } else if self.check_identifier("settle") {
            self.advance();
            ParallelMode::Settle
        } else {
            ParallelMode::Count
        };

        let expr = self.parse_expression()?;

        // Parse the optional `with { ... }` block before the body's `{` so the
        // two braces don't collide. Only `max_concurrent` is accepted today.
        let options = if self.check_identifier("with") {
            self.advance();
            self.consume(&TokenKind::LBrace, "{")?;
            let mut options = Vec::new();
            loop {
                self.skip_newlines();
                if matches!(
                    self.current().map(|t| &t.kind),
                    Some(&TokenKind::RBrace) | None
                ) {
                    break;
                }
                let key_span = self.current_span();
                let key = match self.current().map(|t| &t.kind) {
                    Some(TokenKind::Identifier(name)) => name.clone(),
                    _ => {
                        return Err(ParserError::Unexpected {
                            got: self
                                .current()
                                .map(|t| format!("{:?}", t.kind))
                                .unwrap_or_else(|| "end of input".to_string()),
                            expected: "option name in `parallel ... with { ... }` block"
                                .to_string(),
                            span: key_span,
                        });
                    }
                };
                self.advance();
                if key != "max_concurrent" {
                    return Err(ParserError::Unexpected {
                        got: key.clone(),
                        expected: format!(
                            "known option (only `max_concurrent` is supported in \
                             `parallel ... with {{ ... }}`; got `{key}`)"
                        ),
                        span: key_span,
                    });
                }
                self.consume(&TokenKind::Colon, ":")?;
                let value = self.parse_expression()?;
                options.push((key, value));
                self.skip_newlines();
                if matches!(self.current().map(|t| &t.kind), Some(&TokenKind::Comma)) {
                    self.advance();
                    self.skip_newlines();
                }
            }
            self.consume(&TokenKind::RBrace, "}")?;
            options
        } else {
            Vec::new()
        };

        self.consume(&TokenKind::LBrace, "{")?;

        let mut variable = None;
        self.skip_newlines();
        if let Some(tok) = self.current() {
            if let TokenKind::Identifier(name) = &tok.kind {
                if self.peek_kind() == Some(&TokenKind::Arrow) {
                    let name = name.clone();
                    self.advance();
                    self.advance();
                    variable = Some(name);
                }
            }
        }

        let body = self.parse_block()?;
        self.consume(&TokenKind::RBrace, "}")?;
        Ok(spanned(
            Node::Parallel {
                mode,
                expr: Box::new(expr),
                variable,
                body,
                options,
            },
            Span::merge(start, self.prev_span()),
        ))
    }

    fn parse_return(&mut self) -> Result<SNode, ParserError> {
        let start = self.current_span();
        self.consume(&TokenKind::Return, "return")?;
        if self.is_at_end() || self.check(&TokenKind::Newline) || self.check(&TokenKind::RBrace) {
            return Ok(spanned(
                Node::ReturnStmt { value: None },
                Span::merge(start, self.prev_span()),
            ));
        }
        let value = self.parse_expression()?;
        Ok(spanned(
            Node::ReturnStmt {
                value: Some(Box::new(value)),
            },
            Span::merge(start, self.prev_span()),
        ))
    }

    fn parse_throw(&mut self) -> Result<SNode, ParserError> {
        let start = self.current_span();
        self.consume(&TokenKind::Throw, "throw")?;
        let value = self.parse_expression()?;
        Ok(spanned(
            Node::ThrowStmt {
                value: Box::new(value),
            },
            Span::merge(start, self.prev_span()),
        ))
    }

    fn parse_override(&mut self) -> Result<SNode, ParserError> {
        let start = self.current_span();
        self.consume(&TokenKind::Override, "override")?;
        let name = self.consume_identifier("override name")?;
        self.consume(&TokenKind::LParen, "(")?;
        let params = self.parse_param_list()?;
        self.consume(&TokenKind::RParen, ")")?;
        self.consume(&TokenKind::LBrace, "{")?;
        let body = self.parse_block()?;
        self.consume(&TokenKind::RBrace, "}")?;
        Ok(spanned(
            Node::OverrideDecl { name, params, body },
            Span::merge(start, self.prev_span()),
        ))
    }

    fn parse_try_catch(&mut self) -> Result<SNode, ParserError> {
        let start = self.current_span();
        self.consume(&TokenKind::Try, "try")?;
        if self.check(&TokenKind::Star) {
            self.advance();
            let operand = self.parse_unary()?;
            return Ok(spanned(
                Node::TryStar {
                    operand: Box::new(operand),
                },
                Span::merge(start, self.prev_span()),
            ));
        }
        self.consume(&TokenKind::LBrace, "{")?;
        let body = self.parse_block()?;
        self.consume(&TokenKind::RBrace, "}")?;
        self.skip_newlines();

        let has_catch = self.check(&TokenKind::Catch);
        let (error_var, error_type, catch_body) = if has_catch {
            self.advance();
            let (ev, et) = if self.check(&TokenKind::LParen) {
                self.advance();
                let name = self.consume_identifier("error variable")?;
                let ty = self.try_parse_type_annotation()?;
                self.consume(&TokenKind::RParen, ")")?;
                (Some(name), ty)
            } else if matches!(
                self.current().map(|t| &t.kind),
                Some(TokenKind::Identifier(_))
            ) {
                let name = self.consume_identifier("error variable")?;
                (Some(name), None)
            } else {
                (None, None)
            };
            self.consume(&TokenKind::LBrace, "{")?;
            let cb = self.parse_block()?;
            self.consume(&TokenKind::RBrace, "}")?;
            (ev, et, cb)
        } else {
            (None, None, Vec::new())
        };

        self.skip_newlines();

        let finally_body = if self.check(&TokenKind::Finally) {
            self.advance();
            self.consume(&TokenKind::LBrace, "{")?;
            let fb = self.parse_block()?;
            self.consume(&TokenKind::RBrace, "}")?;
            Some(fb)
        } else {
            None
        };

        // Bare `try { ... }` with neither catch nor finally is a try-expression returning Result.
        if !has_catch && finally_body.is_none() {
            return Ok(spanned(
                Node::TryExpr { body },
                Span::merge(start, self.prev_span()),
            ));
        }

        Ok(spanned(
            Node::TryCatch {
                body,
                error_var,
                error_type,
                catch_body,
                finally_body,
            },
            Span::merge(start, self.prev_span()),
        ))
    }

    fn parse_select(&mut self) -> Result<SNode, ParserError> {
        let start = self.current_span();
        self.consume(&TokenKind::Select, "select")?;
        self.consume(&TokenKind::LBrace, "{")?;
        self.skip_newlines();

        let mut cases = Vec::new();
        let mut timeout = None;
        let mut default_body = None;

        while !self.is_at_end() && !self.check(&TokenKind::RBrace) {
            self.skip_newlines();
            // `timeout` and `default` are contextual keywords (not reserved tokens).
            if let Some(tok) = self.current() {
                if let TokenKind::Identifier(ref id) = tok.kind {
                    if id == "timeout" {
                        self.advance();
                        let duration = self.parse_expression()?;
                        self.consume(&TokenKind::LBrace, "{")?;
                        let body = self.parse_block()?;
                        self.consume(&TokenKind::RBrace, "}")?;
                        timeout = Some((Box::new(duration), body));
                        self.skip_newlines();
                        continue;
                    }
                    if id == "default" {
                        self.advance();
                        self.consume(&TokenKind::LBrace, "{")?;
                        let body = self.parse_block()?;
                        self.consume(&TokenKind::RBrace, "}")?;
                        default_body = Some(body);
                        self.skip_newlines();
                        continue;
                    }
                }
            }
            let variable = self.consume_identifier("select case variable")?;
            self.consume(&TokenKind::From, "from")?;
            let channel = self.parse_expression()?;
            self.consume(&TokenKind::LBrace, "{")?;
            let body = self.parse_block()?;
            self.consume(&TokenKind::RBrace, "}")?;
            cases.push(SelectCase {
                variable,
                channel: Box::new(channel),
                body,
            });
            self.skip_newlines();
        }

        self.consume(&TokenKind::RBrace, "}")?;

        if cases.is_empty() && timeout.is_none() && default_body.is_none() {
            return Err(self.error("at least one select case"));
        }
        if timeout.is_some() && default_body.is_some() {
            return Err(self.error("select cannot have both timeout and default"));
        }

        Ok(spanned(
            Node::SelectExpr {
                cases,
                timeout,
                default_body,
            },
            Span::merge(start, self.prev_span()),
        ))
    }

    fn parse_fn_decl_with_pub(&mut self, is_pub: bool) -> Result<SNode, ParserError> {
        let start = self.current_span();
        self.consume(&TokenKind::Fn, "fn")?;
        let name = self.consume_identifier("function name")?;

        let type_params = if self.check(&TokenKind::Lt) {
            self.advance();
            self.parse_type_param_list()?
        } else {
            Vec::new()
        };

        self.consume(&TokenKind::LParen, "(")?;
        let params = self.parse_typed_param_list()?;
        self.consume(&TokenKind::RParen, ")")?;
        let return_type = if self.check(&TokenKind::Arrow) {
            self.advance();
            Some(self.parse_type_expr()?)
        } else {
            None
        };

        let where_clauses = self.parse_where_clauses()?;

        self.consume(&TokenKind::LBrace, "{")?;
        let body = self.parse_block()?;
        self.consume(&TokenKind::RBrace, "}")?;
        Ok(spanned(
            Node::FnDecl {
                name,
                type_params,
                params,
                return_type,
                where_clauses,
                body,
                is_pub,
            },
            Span::merge(start, self.prev_span()),
        ))
    }

    fn parse_tool_decl(&mut self, is_pub: bool) -> Result<SNode, ParserError> {
        let start = self.current_span();
        self.consume(&TokenKind::Tool, "tool")?;
        let name = self.consume_identifier("tool name")?;

        self.consume(&TokenKind::LParen, "(")?;
        let params = self.parse_typed_param_list()?;
        self.consume(&TokenKind::RParen, ")")?;

        let return_type = if self.check(&TokenKind::Arrow) {
            self.advance();
            Some(self.parse_type_expr()?)
        } else {
            None
        };

        self.consume(&TokenKind::LBrace, "{")?;

        // Optional `description "..."` metadata preceding the tool body.
        self.skip_newlines();
        let mut description = None;
        if let Some(TokenKind::Identifier(id)) = self.current_kind().cloned() {
            if id == "description" {
                let saved_pos = self.pos;
                self.advance();
                self.skip_newlines();
                if let Some(TokenKind::StringLiteral(s)) = self.current_kind().cloned() {
                    description = Some(s);
                    self.advance();
                } else {
                    self.pos = saved_pos;
                }
            }
        }

        let body = self.parse_block()?;
        self.consume(&TokenKind::RBrace, "}")?;

        Ok(spanned(
            Node::ToolDecl {
                name,
                description,
                params,
                return_type,
                body,
                is_pub,
            },
            Span::merge(start, self.prev_span()),
        ))
    }

    fn parse_type_decl(&mut self) -> Result<SNode, ParserError> {
        let start = self.current_span();
        self.consume(&TokenKind::TypeKw, "type")?;
        let name = self.consume_identifier("type name")?;
        let type_params = if self.check(&TokenKind::Lt) {
            self.advance();
            self.parse_type_param_list()?
        } else {
            Vec::new()
        };
        self.consume(&TokenKind::Assign, "=")?;
        let type_expr = self.parse_type_expr()?;
        Ok(spanned(
            Node::TypeDecl {
                name,
                type_params,
                type_expr,
            },
            Span::merge(start, self.prev_span()),
        ))
    }

    fn parse_enum_decl_with_pub(&mut self, is_pub: bool) -> Result<SNode, ParserError> {
        let start = self.current_span();
        self.consume(&TokenKind::Enum, "enum")?;
        let name = self.consume_identifier("enum name")?;
        let type_params = if self.check(&TokenKind::Lt) {
            self.advance();
            self.parse_type_param_list()?
        } else {
            Vec::new()
        };
        self.consume(&TokenKind::LBrace, "{")?;
        self.skip_newlines();

        let mut variants = Vec::new();
        while !self.is_at_end() && !self.check(&TokenKind::RBrace) {
            let variant_name = self.consume_identifier("variant name")?;
            let fields = if self.check(&TokenKind::LParen) {
                self.advance();
                let params = self.parse_typed_param_list()?;
                self.consume(&TokenKind::RParen, ")")?;
                params
            } else {
                Vec::new()
            };
            variants.push(EnumVariant {
                name: variant_name,
                fields,
            });
            self.skip_newlines();
            if self.check(&TokenKind::Comma) {
                self.advance();
                self.skip_newlines();
            }
        }

        self.consume(&TokenKind::RBrace, "}")?;
        Ok(spanned(
            Node::EnumDecl {
                name,
                type_params,
                variants,
                is_pub,
            },
            Span::merge(start, self.prev_span()),
        ))
    }

    fn parse_enum_decl(&mut self) -> Result<SNode, ParserError> {
        self.parse_enum_decl_with_pub(false)
    }

    fn parse_struct_decl_with_pub(&mut self, is_pub: bool) -> Result<SNode, ParserError> {
        let start = self.current_span();
        self.consume(&TokenKind::Struct, "struct")?;
        let name = self.consume_identifier("struct name")?;
        self.struct_names.insert(name.clone());
        let type_params = if self.check(&TokenKind::Lt) {
            self.advance();
            self.parse_type_param_list()?
        } else {
            Vec::new()
        };
        self.consume(&TokenKind::LBrace, "{")?;
        self.skip_newlines();

        let mut fields = Vec::new();
        while !self.is_at_end() && !self.check(&TokenKind::RBrace) {
            let field_name = self.consume_identifier("field name")?;
            let optional = if self.check(&TokenKind::Question) {
                self.advance();
                true
            } else {
                false
            };
            let type_expr = self.try_parse_type_annotation()?;
            fields.push(StructField {
                name: field_name,
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
        Ok(spanned(
            Node::StructDecl {
                name,
                type_params,
                fields,
                is_pub,
            },
            Span::merge(start, self.prev_span()),
        ))
    }

    fn parse_struct_decl(&mut self) -> Result<SNode, ParserError> {
        self.parse_struct_decl_with_pub(false)
    }

    fn parse_interface_decl(&mut self) -> Result<SNode, ParserError> {
        let start = self.current_span();
        self.consume(&TokenKind::Interface, "interface")?;
        let name = self.consume_identifier("interface name")?;
        let type_params = if self.check(&TokenKind::Lt) {
            self.advance();
            self.parse_type_param_list()?
        } else {
            Vec::new()
        };
        self.consume(&TokenKind::LBrace, "{")?;
        self.skip_newlines();

        let mut associated_types = Vec::new();
        let mut methods = Vec::new();
        while !self.is_at_end() && !self.check(&TokenKind::RBrace) {
            if self.check(&TokenKind::TypeKw) {
                self.advance();
                let assoc_name = self.consume_identifier("associated type name")?;
                let assoc_type = if self.check(&TokenKind::Assign) {
                    self.advance();
                    Some(self.parse_type_expr()?)
                } else {
                    None
                };
                associated_types.push((assoc_name, assoc_type));
            } else {
                self.consume(&TokenKind::Fn, "fn")?;
                let method_name = self.consume_identifier("method name")?;
                let method_type_params = if self.check(&TokenKind::Lt) {
                    self.advance();
                    self.parse_type_param_list()?
                } else {
                    Vec::new()
                };
                self.consume(&TokenKind::LParen, "(")?;
                let params = self.parse_typed_param_list()?;
                self.consume(&TokenKind::RParen, ")")?;
                let return_type = if self.check(&TokenKind::Arrow) {
                    self.advance();
                    Some(self.parse_type_expr()?)
                } else {
                    None
                };
                methods.push(InterfaceMethod {
                    name: method_name,
                    type_params: method_type_params,
                    params,
                    return_type,
                });
            }
            self.skip_newlines();
        }

        self.consume(&TokenKind::RBrace, "}")?;
        Ok(spanned(
            Node::InterfaceDecl {
                name,
                type_params,
                associated_types,
                methods,
            },
            Span::merge(start, self.prev_span()),
        ))
    }

    fn parse_impl_block(&mut self) -> Result<SNode, ParserError> {
        let start = self.current_span();
        self.consume(&TokenKind::Impl, "impl")?;
        let type_name = self.consume_identifier("type name")?;
        self.consume(&TokenKind::LBrace, "{")?;
        self.skip_newlines();

        let mut methods = Vec::new();
        while !self.is_at_end() && !self.check(&TokenKind::RBrace) {
            let is_pub = self.check(&TokenKind::Pub);
            if is_pub {
                self.advance();
            }
            let method = self.parse_fn_decl_with_pub(is_pub)?;
            methods.push(method);
            self.skip_newlines();
        }

        self.consume(&TokenKind::RBrace, "}")?;
        Ok(spanned(
            Node::ImplBlock { type_name, methods },
            Span::merge(start, self.prev_span()),
        ))
    }

    fn parse_guard(&mut self) -> Result<SNode, ParserError> {
        let start = self.current_span();
        self.consume(&TokenKind::Guard, "guard")?;
        let condition = self.parse_expression()?;
        self.consume(&TokenKind::Else, "else")?;
        self.consume(&TokenKind::LBrace, "{")?;
        let else_body = self.parse_block()?;
        self.consume(&TokenKind::RBrace, "}")?;
        Ok(spanned(
            Node::GuardStmt {
                condition: Box::new(condition),
                else_body,
            },
            Span::merge(start, self.prev_span()),
        ))
    }

    fn parse_require(&mut self) -> Result<SNode, ParserError> {
        let start = self.current_span();
        self.consume(&TokenKind::Require, "require")?;
        let condition = self.parse_expression()?;
        let message = if self.check(&TokenKind::Comma) {
            self.advance();
            Some(Box::new(self.parse_expression()?))
        } else {
            None
        };
        Ok(spanned(
            Node::RequireStmt {
                condition: Box::new(condition),
                message,
            },
            Span::merge(start, self.prev_span()),
        ))
    }

    fn parse_deadline(&mut self) -> Result<SNode, ParserError> {
        let start = self.current_span();
        self.consume(&TokenKind::Deadline, "deadline")?;
        let duration = self.parse_primary()?;
        self.consume(&TokenKind::LBrace, "{")?;
        let body = self.parse_block()?;
        self.consume(&TokenKind::RBrace, "}")?;
        Ok(spanned(
            Node::DeadlineBlock {
                duration: Box::new(duration),
                body,
            },
            Span::merge(start, self.prev_span()),
        ))
    }

    fn parse_yield(&mut self) -> Result<SNode, ParserError> {
        let start = self.current_span();
        self.consume(&TokenKind::Yield, "yield")?;
        if self.is_at_end() || self.check(&TokenKind::Newline) || self.check(&TokenKind::RBrace) {
            return Ok(spanned(
                Node::YieldExpr { value: None },
                Span::merge(start, self.prev_span()),
            ));
        }
        let value = self.parse_expression()?;
        Ok(spanned(
            Node::YieldExpr {
                value: Some(Box::new(value)),
            },
            Span::merge(start, self.prev_span()),
        ))
    }

    fn parse_mutex(&mut self) -> Result<SNode, ParserError> {
        let start = self.current_span();
        self.consume(&TokenKind::Mutex, "mutex")?;
        self.consume(&TokenKind::LBrace, "{")?;
        let body = self.parse_block()?;
        self.consume(&TokenKind::RBrace, "}")?;
        Ok(spanned(
            Node::MutexBlock { body },
            Span::merge(start, self.prev_span()),
        ))
    }

    fn parse_defer(&mut self) -> Result<SNode, ParserError> {
        let start = self.current_span();
        self.consume(&TokenKind::Defer, "defer")?;
        self.consume(&TokenKind::LBrace, "{")?;
        let body = self.parse_block()?;
        self.consume(&TokenKind::RBrace, "}")?;
        Ok(spanned(
            Node::DeferStmt { body },
            Span::merge(start, self.prev_span()),
        ))
    }

    fn parse_expression_statement(&mut self) -> Result<SNode, ParserError> {
        let start = self.current_span();
        let expr = self.parse_expression()?;

        // Only identifiers, property accesses, and subscript accesses are valid
        // assignment targets.
        let is_assignable = matches!(
            expr.node,
            Node::Identifier(_) | Node::PropertyAccess { .. } | Node::SubscriptAccess { .. }
        );
        if is_assignable {
            if self.check(&TokenKind::Assign) {
                self.advance();
                let value = self.parse_expression()?;
                return Ok(spanned(
                    Node::Assignment {
                        target: Box::new(expr),
                        value: Box::new(value),
                        op: None,
                    },
                    Span::merge(start, self.prev_span()),
                ));
            }
            let compound_op = if self.check(&TokenKind::PlusAssign) {
                Some("+")
            } else if self.check(&TokenKind::MinusAssign) {
                Some("-")
            } else if self.check(&TokenKind::StarAssign) {
                Some("*")
            } else if self.check(&TokenKind::SlashAssign) {
                Some("/")
            } else if self.check(&TokenKind::PercentAssign) {
                Some("%")
            } else {
                None
            };
            if let Some(op) = compound_op {
                self.advance();
                let value = self.parse_expression()?;
                return Ok(spanned(
                    Node::Assignment {
                        target: Box::new(expr),
                        value: Box::new(value),
                        op: Some(op.into()),
                    },
                    Span::merge(start, self.prev_span()),
                ));
            }
        }

        Ok(expr)
    }

    fn parse_expression(&mut self) -> Result<SNode, ParserError> {
        self.skip_newlines();
        self.parse_pipe()
    }

    fn parse_pipe(&mut self) -> Result<SNode, ParserError> {
        let mut left = self.parse_range()?;
        while self.check_skip_newlines(&TokenKind::Pipe) {
            let start = left.span;
            self.advance();
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

    fn parse_range(&mut self) -> Result<SNode, ParserError> {
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

    fn parse_ternary(&mut self) -> Result<SNode, ParserError> {
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
    fn parse_nil_coalescing(&mut self) -> Result<SNode, ParserError> {
        let mut left = self.parse_multiplicative()?;
        while self.check(&TokenKind::NilCoal) {
            let start = left.span;
            self.advance();
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

    fn parse_logical_or(&mut self) -> Result<SNode, ParserError> {
        let mut left = self.parse_logical_and()?;
        while self.check_skip_newlines(&TokenKind::Or) {
            let start = left.span;
            self.advance();
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

    fn parse_logical_and(&mut self) -> Result<SNode, ParserError> {
        let mut left = self.parse_equality()?;
        while self.check_skip_newlines(&TokenKind::And) {
            let start = left.span;
            self.advance();
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

    fn parse_equality(&mut self) -> Result<SNode, ParserError> {
        let mut left = self.parse_comparison()?;
        while self.check(&TokenKind::Eq) || self.check(&TokenKind::Neq) {
            let start = left.span;
            let op = if self.check(&TokenKind::Eq) {
                "=="
            } else {
                "!="
            };
            self.advance();
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

    fn parse_comparison(&mut self) -> Result<SNode, ParserError> {
        let mut left = self.parse_additive()?;
        loop {
            if self.check(&TokenKind::Lt)
                || self.check(&TokenKind::Gt)
                || self.check(&TokenKind::Lte)
                || self.check(&TokenKind::Gte)
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

    fn parse_additive(&mut self) -> Result<SNode, ParserError> {
        let mut left = self.parse_nil_coalescing()?;
        while self.check_skip_newlines(&TokenKind::Plus) || self.check(&TokenKind::Minus) {
            let start = left.span;
            let op = if self.check(&TokenKind::Plus) {
                "+"
            } else {
                "-"
            };
            self.advance();
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

    fn parse_multiplicative(&mut self) -> Result<SNode, ParserError> {
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

    fn parse_exponent(&mut self) -> Result<SNode, ParserError> {
        let left = self.parse_unary()?;
        if !self.check_skip_newlines(&TokenKind::Pow) {
            return Ok(left);
        }

        let start = left.span;
        self.advance();
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

    fn parse_unary(&mut self) -> Result<SNode, ParserError> {
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

    fn parse_postfix(&mut self) -> Result<SNode, ParserError> {
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
            } else if self.check(&TokenKind::LBrace)
                && matches!(&expr.node, Node::Identifier(name) if self.struct_names.contains(name))
            {
                let start = expr.span;
                let struct_name = match expr.node {
                    Node::Identifier(name) => name,
                    _ => unreachable!("checked above"),
                };
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
            } else if self.check(&TokenKind::LParen) && matches!(expr.node, Node::Identifier(_)) {
                let start = expr.span;
                self.advance();
                let args = self.parse_arg_list()?;
                self.consume(&TokenKind::RParen, ")")?;
                if let Node::Identifier(name) = expr.node {
                    expr = spanned(
                        Node::FunctionCall { name, args },
                        Span::merge(start, self.prev_span()),
                    );
                }
            } else if self.check(&TokenKind::Question) {
                // Postfix try `expr?` vs ternary `expr ? a : b`: if the next token
                // could start a ternary branch, let parse_ternary handle the `?`.
                let next_pos = self.pos + 1;
                let is_ternary = self.tokens.get(next_pos).is_some_and(|t| {
                    matches!(
                        t.kind,
                        TokenKind::Identifier(_)
                            | TokenKind::IntLiteral(_)
                            | TokenKind::FloatLiteral(_)
                            | TokenKind::StringLiteral(_)
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

    fn parse_primary(&mut self) -> Result<SNode, ParserError> {
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
    fn parse_fn_expr(&mut self) -> Result<SNode, ParserError> {
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

    fn parse_spawn_expr(&mut self) -> Result<SNode, ParserError> {
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

    fn parse_list_literal(&mut self) -> Result<SNode, ParserError> {
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

    fn parse_dict_or_closure(&mut self) -> Result<SNode, ParserError> {
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

    /// Caller must save/restore `pos`; this advances while scanning.
    fn is_closure_lookahead(&mut self) -> bool {
        let mut depth = 0;
        while !self.is_at_end() {
            if let Some(tok) = self.current() {
                match &tok.kind {
                    TokenKind::Arrow if depth == 0 => return true,
                    TokenKind::LBrace | TokenKind::LParen | TokenKind::LBracket => depth += 1,
                    TokenKind::RBrace if depth == 0 => return false,
                    TokenKind::RBrace => depth -= 1,
                    TokenKind::RParen | TokenKind::RBracket => {
                        if depth > 0 {
                            depth -= 1;
                        }
                    }
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
    fn parse_closure_body(&mut self, start: Span) -> Result<SNode, ParserError> {
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
    fn parse_typed_param_list_until_arrow(&mut self) -> Result<Vec<TypedParam>, ParserError> {
        self.parse_typed_params_until(|tok| tok == &TokenKind::Arrow)
    }

    fn parse_dict_literal(&mut self, start: Span) -> Result<SNode, ParserError> {
        let entries = self.parse_dict_entries()?;
        Ok(spanned(
            Node::DictLiteral(entries),
            Span::merge(start, self.prev_span()),
        ))
    }

    fn parse_dict_entries(&mut self) -> Result<Vec<DictEntry>, ParserError> {
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
    fn parse_param_list(&mut self) -> Result<Vec<String>, ParserError> {
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
    fn parse_typed_param_list(&mut self) -> Result<Vec<TypedParam>, ParserError> {
        self.parse_typed_params_until(|tok| tok == &TokenKind::RParen)
    }

    /// Shared implementation: parse typed params with optional defaults until
    /// a terminator token is reached.
    fn parse_typed_params_until(
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

    /// Parse a comma-separated list of type parameters until `>`.
    ///
    /// Each parameter may be prefixed with a variance marker:
    /// `in T` (contravariant) or `out T` (covariant). Unannotated
    /// parameters default to `Invariant`.
    fn parse_type_param_list(&mut self) -> Result<Vec<TypeParam>, ParserError> {
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
    fn parse_optional_variance_marker(&mut self) -> Variance {
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
    fn parse_where_clauses(&mut self) -> Result<Vec<WhereClause>, ParserError> {
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
    fn try_parse_type_annotation(&mut self) -> Result<Option<TypeExpr>, ParserError> {
        if !self.check(&TokenKind::Colon) {
            return Ok(None);
        }
        self.advance();
        Ok(Some(self.parse_type_expr()?))
    }

    /// Parse a type expression: `int`, `string | nil`, `{name: string, age?: int}`.
    fn parse_type_expr(&mut self) -> Result<TypeExpr, ParserError> {
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
    fn parse_type_primary(&mut self) -> Result<TypeExpr, ParserError> {
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
            }
            return Ok(TypeExpr::Applied {
                name,
                args: type_args,
            });
        }
        Ok(TypeExpr::Named(name))
    }

    /// Parse a shape type: `{ name: string, age: int, active?: bool }`.
    fn parse_shape_type(&mut self) -> Result<TypeExpr, ParserError> {
        self.consume(&TokenKind::LBrace, "{")?;
        let mut fields = Vec::new();
        self.skip_newlines();

        while !self.is_at_end() && !self.check(&TokenKind::RBrace) {
            let name = self.consume_identifier("field name")?;
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

    fn parse_arg_list(&mut self) -> Result<Vec<SNode>, ParserError> {
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

    fn is_at_end(&self) -> bool {
        self.pos >= self.tokens.len()
            || matches!(self.tokens.get(self.pos), Some(t) if t.kind == TokenKind::Eof)
    }

    fn current(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn peek_kind(&self) -> Option<&TokenKind> {
        self.tokens.get(self.pos + 1).map(|t| &t.kind)
    }

    fn peek_kind_at(&self, offset: usize) -> Option<&TokenKind> {
        self.tokens.get(self.pos + offset).map(|t| &t.kind)
    }

    fn check(&self, kind: &TokenKind) -> bool {
        self.current()
            .map(|t| std::mem::discriminant(&t.kind) == std::mem::discriminant(kind))
            .unwrap_or(false)
    }

    /// Check for `kind`, skipping newlines first; used for binary operators
    /// like `||` and `&&` that can span lines.
    fn check_skip_newlines(&mut self, kind: &TokenKind) -> bool {
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
    fn check_identifier(&self, name: &str) -> bool {
        matches!(self.current().map(|t| &t.kind), Some(TokenKind::Identifier(s)) if s == name)
    }

    fn advance(&mut self) {
        if self.pos < self.tokens.len() {
            self.pos += 1;
        }
    }

    fn consume(&mut self, kind: &TokenKind, expected: &str) -> Result<Token, ParserError> {
        self.skip_newlines();
        let tok = self.current().ok_or_else(|| self.make_error(expected))?;
        if std::mem::discriminant(&tok.kind) != std::mem::discriminant(kind) {
            return Err(self.make_error(expected));
        }
        let tok = tok.clone();
        self.advance();
        Ok(tok)
    }

    fn consume_identifier(&mut self, expected: &str) -> Result<String, ParserError> {
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
    fn consume_identifier_or_keyword(&mut self, expected: &str) -> Result<String, ParserError> {
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

    fn skip_newlines(&mut self) {
        while self.pos < self.tokens.len() && self.tokens[self.pos].kind == TokenKind::Newline {
            self.pos += 1;
        }
    }

    fn make_error(&self, expected: &str) -> ParserError {
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

    fn error(&self, expected: &str) -> ParserError {
        self.make_error(expected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
