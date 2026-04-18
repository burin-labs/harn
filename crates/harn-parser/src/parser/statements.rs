use crate::ast::*;
use harn_lexer::{Span, TokenKind};

use super::error::ParserError;
use super::state::Parser;

impl Parser {
    pub(super) fn parse_block(&mut self) -> Result<Vec<SNode>, ParserError> {
        let mut stmts = Vec::new();
        self.skip_newlines();

        while !self.is_at_end() && !self.check(&TokenKind::RBrace) {
            stmts.push(self.parse_statement()?);
            self.skip_newlines();
        }
        Ok(stmts)
    }

    pub(super) fn parse_statement(&mut self) -> Result<SNode, ParserError> {
        self.skip_newlines();

        let tok = self.current().ok_or_else(|| ParserError::UnexpectedEof {
            expected: "statement".into(),
            span: self.prev_span(),
        })?;

        match &tok.kind {
            TokenKind::At => self.parse_attributed_decl(),
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
            TokenKind::Skill => self.parse_skill_decl(false),
            TokenKind::Pub => {
                self.advance(); // consume 'pub'
                let tok = self.current().ok_or_else(|| ParserError::UnexpectedEof {
                    expected: "fn, tool, skill, struct, enum, or pipeline after pub".into(),
                    span: self.prev_span(),
                })?;
                match &tok.kind {
                    TokenKind::Fn => self.parse_fn_decl_with_pub(true),
                    TokenKind::Tool => self.parse_tool_decl(true),
                    TokenKind::Skill => self.parse_skill_decl(true),
                    TokenKind::Pipeline => self.parse_pipeline_with_pub(true),
                    TokenKind::Enum => self.parse_enum_decl_with_pub(true),
                    TokenKind::Struct => self.parse_struct_decl_with_pub(true),
                    _ => Err(self.error("fn, tool, skill, struct, enum, or pipeline after pub")),
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

    pub(super) fn parse_let_binding(&mut self) -> Result<SNode, ParserError> {
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

    pub(super) fn parse_var_binding(&mut self) -> Result<SNode, ParserError> {
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

    pub(super) fn parse_if_else(&mut self) -> Result<SNode, ParserError> {
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

    pub(super) fn parse_for_in(&mut self) -> Result<SNode, ParserError> {
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

    pub(super) fn parse_match(&mut self) -> Result<SNode, ParserError> {
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

    pub(super) fn parse_while_loop(&mut self) -> Result<SNode, ParserError> {
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

    pub(super) fn parse_retry(&mut self) -> Result<SNode, ParserError> {
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

    pub(super) fn parse_parallel(&mut self) -> Result<SNode, ParserError> {
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

    pub(super) fn parse_return(&mut self) -> Result<SNode, ParserError> {
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

    pub(super) fn parse_throw(&mut self) -> Result<SNode, ParserError> {
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

    pub(super) fn parse_override(&mut self) -> Result<SNode, ParserError> {
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

    pub(super) fn parse_try_catch(&mut self) -> Result<SNode, ParserError> {
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

    pub(super) fn parse_select(&mut self) -> Result<SNode, ParserError> {
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

    pub(super) fn parse_guard(&mut self) -> Result<SNode, ParserError> {
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

    pub(super) fn parse_require(&mut self) -> Result<SNode, ParserError> {
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

    pub(super) fn parse_deadline(&mut self) -> Result<SNode, ParserError> {
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

    pub(super) fn parse_yield(&mut self) -> Result<SNode, ParserError> {
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

    pub(super) fn parse_mutex(&mut self) -> Result<SNode, ParserError> {
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

    pub(super) fn parse_defer(&mut self) -> Result<SNode, ParserError> {
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

    pub(super) fn parse_expression_statement(&mut self) -> Result<SNode, ParserError> {
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
}
