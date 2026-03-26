use crate::ast::*;
use harn_lexer::{Span, Token, TokenKind};
use std::fmt;

/// Parser errors.
#[derive(Debug, Clone, PartialEq)]
pub enum ParserError {
    Unexpected {
        got: String,
        expected: String,
        line: usize,
        column: usize,
    },
    UnexpectedEof {
        expected: String,
    },
}

impl fmt::Display for ParserError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParserError::Unexpected {
                got,
                expected,
                line,
                column,
            } => write!(f, "Expected {expected}, got {got} at {line}:{column}"),
            ParserError::UnexpectedEof { expected } => {
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
}

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, pos: 0 }
    }

    fn current_span(&self) -> Span {
        self.tokens
            .get(self.pos)
            .map(|t| t.span)
            .unwrap_or(Span::dummy())
    }

    fn prev_span(&self) -> Span {
        if self.pos > 0 {
            self.tokens[self.pos - 1].span
        } else {
            Span::dummy()
        }
    }

    /// Parse a complete .harn file.
    pub fn parse(&mut self) -> Result<Vec<SNode>, ParserError> {
        let mut nodes = Vec::new();
        self.skip_newlines();

        while !self.is_at_end() {
            if self.check(&TokenKind::Import) {
                nodes.push(self.parse_import()?);
            } else if self.check(&TokenKind::Pipeline) {
                nodes.push(self.parse_pipeline()?);
            } else {
                // Allow top-level statements (for error reporting on malformed files)
                nodes.push(self.parse_statement()?);
            }
            self.skip_newlines();
        }
        Ok(nodes)
    }

    /// Parse a single expression (for string interpolation).
    pub fn parse_single_expression(&mut self) -> Result<SNode, ParserError> {
        self.skip_newlines();
        self.parse_expression()
    }

    // --- Declarations ---

    fn parse_pipeline(&mut self) -> Result<SNode, ParserError> {
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
            },
            Span::merge(start, self.prev_span()),
        ))
    }

    fn parse_import(&mut self) -> Result<SNode, ParserError> {
        let start = self.current_span();
        self.consume(&TokenKind::Import, "import")?;
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

    // --- Statements ---

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
            TokenKind::ParallelMap => self.parse_parallel_map(),
            TokenKind::Return => self.parse_return(),
            TokenKind::Throw => self.parse_throw(),
            TokenKind::Override => self.parse_override(),
            TokenKind::Try => self.parse_try_catch(),
            TokenKind::Fn => self.parse_fn_decl(),
            TokenKind::TypeKw => self.parse_type_decl(),
            TokenKind::Enum => self.parse_enum_decl(),
            TokenKind::Struct => self.parse_struct_decl(),
            TokenKind::Guard => self.parse_guard(),
            TokenKind::Deadline => self.parse_deadline(),
            TokenKind::Yield => self.parse_yield(),
            TokenKind::Mutex => self.parse_mutex(),
            _ => self.parse_expression_statement(),
        }
    }

    fn parse_let_binding(&mut self) -> Result<SNode, ParserError> {
        let start = self.current_span();
        self.consume(&TokenKind::Let, "let")?;
        let name = self.consume_identifier("variable name")?;
        let type_ann = self.try_parse_type_annotation()?;
        self.consume(&TokenKind::Assign, "=")?;
        let value = self.parse_expression()?;
        Ok(spanned(
            Node::LetBinding {
                name,
                type_ann,
                value: Box::new(value),
            },
            Span::merge(start, self.prev_span()),
        ))
    }

    fn parse_var_binding(&mut self) -> Result<SNode, ParserError> {
        let start = self.current_span();
        self.consume(&TokenKind::Var, "var")?;
        let name = self.consume_identifier("variable name")?;
        let type_ann = self.try_parse_type_annotation()?;
        self.consume(&TokenKind::Assign, "=")?;
        let value = self.parse_expression()?;
        Ok(spanned(
            Node::VarBinding {
                name,
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
        let variable = self.consume_identifier("loop variable")?;
        self.consume(&TokenKind::In, "in")?;
        let iterable = self.parse_expression()?;
        self.consume(&TokenKind::LBrace, "{")?;
        let body = self.parse_block()?;
        self.consume(&TokenKind::RBrace, "}")?;
        Ok(spanned(
            Node::ForIn {
                variable,
                iterable: Box::new(iterable),
                body,
            },
            Span::merge(start, self.prev_span()),
        ))
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
            self.consume(&TokenKind::Arrow, "->")?;
            self.consume(&TokenKind::LBrace, "{")?;
            let body = self.parse_block()?;
            self.consume(&TokenKind::RBrace, "}")?;
            arms.push(MatchArm { pattern, body });
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
        self.consume(&TokenKind::LParen, "(")?;
        let count = self.parse_expression()?;
        self.consume(&TokenKind::RParen, ")")?;
        self.consume(&TokenKind::LBrace, "{")?;

        // Optional closure parameter: { i ->
        let mut variable = None;
        self.skip_newlines();
        if let Some(tok) = self.current() {
            if let TokenKind::Identifier(name) = &tok.kind {
                if self.peek_kind() == Some(&TokenKind::Arrow) {
                    let name = name.clone();
                    self.advance(); // skip identifier
                    self.advance(); // skip ->
                    variable = Some(name);
                }
            }
        }

        let body = self.parse_block()?;
        self.consume(&TokenKind::RBrace, "}")?;
        Ok(spanned(
            Node::Parallel {
                count: Box::new(count),
                variable,
                body,
            },
            Span::merge(start, self.prev_span()),
        ))
    }

    fn parse_parallel_map(&mut self) -> Result<SNode, ParserError> {
        let start = self.current_span();
        self.consume(&TokenKind::ParallelMap, "parallel_map")?;
        self.consume(&TokenKind::LParen, "(")?;
        let list = self.parse_expression()?;
        self.consume(&TokenKind::RParen, ")")?;
        self.consume(&TokenKind::LBrace, "{")?;

        self.skip_newlines();
        let variable = self.consume_identifier("map variable")?;
        self.consume(&TokenKind::Arrow, "->")?;

        let body = self.parse_block()?;
        self.consume(&TokenKind::RBrace, "}")?;
        Ok(spanned(
            Node::ParallelMap {
                list: Box::new(list),
                variable,
                body,
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
        self.consume(&TokenKind::LBrace, "{")?;
        let body = self.parse_block()?;
        self.consume(&TokenKind::RBrace, "}")?;
        self.skip_newlines();
        self.consume(&TokenKind::Catch, "catch")?;

        let (error_var, error_type) = if self.check(&TokenKind::LParen) {
            self.advance();
            let name = self.consume_identifier("error variable")?;
            let ty = self.try_parse_type_annotation()?;
            self.consume(&TokenKind::RParen, ")")?;
            (Some(name), ty)
        } else {
            (None, None)
        };

        self.consume(&TokenKind::LBrace, "{")?;
        let catch_body = self.parse_block()?;
        self.consume(&TokenKind::RBrace, "}")?;
        Ok(spanned(
            Node::TryCatch {
                body,
                error_var,
                error_type,
                catch_body,
            },
            Span::merge(start, self.prev_span()),
        ))
    }

    fn parse_fn_decl(&mut self) -> Result<SNode, ParserError> {
        let start = self.current_span();
        self.consume(&TokenKind::Fn, "fn")?;
        let name = self.consume_identifier("function name")?;
        self.consume(&TokenKind::LParen, "(")?;
        let params = self.parse_typed_param_list()?;
        self.consume(&TokenKind::RParen, ")")?;
        // Optional return type: -> type
        let return_type = if self.check(&TokenKind::Arrow) {
            self.advance();
            Some(self.parse_type_expr()?)
        } else {
            None
        };
        self.consume(&TokenKind::LBrace, "{")?;
        let body = self.parse_block()?;
        self.consume(&TokenKind::RBrace, "}")?;
        Ok(spanned(
            Node::FnDecl {
                name,
                params,
                return_type,
                body,
            },
            Span::merge(start, self.prev_span()),
        ))
    }

    fn parse_type_decl(&mut self) -> Result<SNode, ParserError> {
        let start = self.current_span();
        self.consume(&TokenKind::TypeKw, "type")?;
        let name = self.consume_identifier("type name")?;
        self.consume(&TokenKind::Assign, "=")?;
        let type_expr = self.parse_type_expr()?;
        Ok(spanned(
            Node::TypeDecl { name, type_expr },
            Span::merge(start, self.prev_span()),
        ))
    }

    fn parse_enum_decl(&mut self) -> Result<SNode, ParserError> {
        let start = self.current_span();
        self.consume(&TokenKind::Enum, "enum")?;
        let name = self.consume_identifier("enum name")?;
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
            Node::EnumDecl { name, variants },
            Span::merge(start, self.prev_span()),
        ))
    }

    fn parse_struct_decl(&mut self) -> Result<SNode, ParserError> {
        let start = self.current_span();
        self.consume(&TokenKind::Struct, "struct")?;
        let name = self.consume_identifier("struct name")?;
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
            Node::StructDecl { name, fields },
            Span::merge(start, self.prev_span()),
        ))
    }

    fn parse_guard(&mut self) -> Result<SNode, ParserError> {
        let start = self.current_span();
        self.consume(&TokenKind::Guard, "guard")?;
        let condition = self.parse_expression()?;
        // Consume "else" — we reuse the Else keyword
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

    fn parse_ask_expr(&mut self) -> Result<SNode, ParserError> {
        let start = self.current_span();
        self.consume(&TokenKind::Ask, "ask")?;
        self.consume(&TokenKind::LBrace, "{")?;
        // Parse as dict entries
        let mut entries = Vec::new();
        self.skip_newlines();
        while !self.is_at_end() && !self.check(&TokenKind::RBrace) {
            let key_span = self.current_span();
            let name = self.consume_identifier("ask field")?;
            let key = spanned(Node::StringLiteral(name), key_span);
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
        Ok(spanned(
            Node::AskExpr { fields: entries },
            Span::merge(start, self.prev_span()),
        ))
    }

    // --- Expressions (precedence climbing) ---

    fn parse_expression_statement(&mut self) -> Result<SNode, ParserError> {
        let start = self.current_span();
        let expr = self.parse_expression()?;

        // Check for assignment: identifier = value
        if self.check(&TokenKind::Assign) && matches!(expr.node, Node::Identifier(_)) {
            self.advance();
            let value = self.parse_expression()?;
            return Ok(spanned(
                Node::Assignment {
                    target: Box::new(expr),
                    value: Box::new(value),
                },
                Span::merge(start, self.prev_span()),
            ));
        }

        Ok(expr)
    }

    fn parse_expression(&mut self) -> Result<SNode, ParserError> {
        self.skip_newlines();
        self.parse_pipe()
    }

    fn parse_pipe(&mut self) -> Result<SNode, ParserError> {
        let mut left = self.parse_range()?;
        while self.check(&TokenKind::Pipe) {
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
        if self.check(&TokenKind::Thru) {
            let start = left.span;
            self.advance();
            let right = self.parse_ternary()?;
            return Ok(spanned(
                Node::RangeExpr {
                    start: Box::new(left),
                    end: Box::new(right),
                    inclusive: true,
                },
                Span::merge(start, self.prev_span()),
            ));
        }
        if self.check(&TokenKind::Upto) {
            let start = left.span;
            self.advance();
            let right = self.parse_ternary()?;
            return Ok(spanned(
                Node::RangeExpr {
                    start: Box::new(left),
                    end: Box::new(right),
                    inclusive: false,
                },
                Span::merge(start, self.prev_span()),
            ));
        }
        Ok(left)
    }

    fn parse_ternary(&mut self) -> Result<SNode, ParserError> {
        let condition = self.parse_nil_coalescing()?;
        if !self.check(&TokenKind::Question) {
            return Ok(condition);
        }
        let start = condition.span;
        self.advance(); // skip ?
        let true_val = self.parse_nil_coalescing()?;
        self.consume(&TokenKind::Colon, ":")?;
        let false_val = self.parse_nil_coalescing()?;
        Ok(spanned(
            Node::Ternary {
                condition: Box::new(condition),
                true_expr: Box::new(true_val),
                false_expr: Box::new(false_val),
            },
            Span::merge(start, self.prev_span()),
        ))
    }

    fn parse_nil_coalescing(&mut self) -> Result<SNode, ParserError> {
        let mut left = self.parse_logical_or()?;
        while self.check(&TokenKind::NilCoal) {
            let start = left.span;
            self.advance();
            let right = self.parse_logical_or()?;
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
        while self.check(&TokenKind::Or) {
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
        while self.check(&TokenKind::And) {
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
        while self.check(&TokenKind::Lt)
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
        }
        Ok(left)
    }

    fn parse_additive(&mut self) -> Result<SNode, ParserError> {
        let mut left = self.parse_multiplicative()?;
        while self.check(&TokenKind::Plus) || self.check(&TokenKind::Minus) {
            let start = left.span;
            let op = if self.check(&TokenKind::Plus) {
                "+"
            } else {
                "-"
            };
            self.advance();
            let right = self.parse_multiplicative()?;
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
        let mut left = self.parse_unary()?;
        while self.check(&TokenKind::Star) || self.check(&TokenKind::Slash) {
            let start = left.span;
            let op = if self.check(&TokenKind::Star) {
                "*"
            } else {
                "/"
            };
            self.advance();
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
            if self.check(&TokenKind::Dot) {
                let start = expr.span;
                self.advance();
                let member = self.consume_identifier("member name")?;
                if self.check(&TokenKind::LParen) {
                    self.advance();
                    let args = self.parse_arg_list()?;
                    self.consume(&TokenKind::RParen, ")")?;
                    expr = spanned(
                        Node::MethodCall {
                            object: Box::new(expr),
                            method: member,
                            args,
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
                let index = self.parse_expression()?;
                self.consume(&TokenKind::RBracket, "]")?;
                expr = spanned(
                    Node::SubscriptAccess {
                        object: Box::new(expr),
                        index: Box::new(index),
                    },
                    Span::merge(start, self.prev_span()),
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
            } else {
                break;
            }
        }

        Ok(expr)
    }

    fn parse_primary(&mut self) -> Result<SNode, ParserError> {
        let tok = self.current().ok_or_else(|| ParserError::UnexpectedEof {
            expected: "expression".into(),
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
            TokenKind::ParallelMap => self.parse_parallel_map(),
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
            TokenKind::Ask => self.parse_ask_expr(),
            TokenKind::Deadline => self.parse_deadline(),
            _ => Err(self.error("expression")),
        }
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
            elements.push(self.parse_expression()?);
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

        // Empty dict
        if self.check(&TokenKind::RBrace) {
            self.advance();
            return Ok(spanned(
                Node::DictLiteral(Vec::new()),
                Span::merge(start, self.prev_span()),
            ));
        }

        // Lookahead: scan for -> before } to disambiguate closure from dict.
        let saved = self.pos;
        if self.is_closure_lookahead() {
            self.pos = saved;
            return self.parse_closure_body(start);
        }
        self.pos = saved;
        self.parse_dict_literal(start)
    }

    /// Scan forward to determine if this is a closure (has -> before matching }).
    /// Does not consume tokens (caller saves/restores pos).
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
            Node::Closure { params, body },
            Span::merge(start, self.prev_span()),
        ))
    }

    /// Parse typed params until we see ->. Handles: `x`, `x: int`, `x, y`, `x: int, y: string`.
    fn parse_typed_param_list_until_arrow(&mut self) -> Result<Vec<TypedParam>, ParserError> {
        let mut params = Vec::new();
        self.skip_newlines();
        while !self.is_at_end() && !self.check(&TokenKind::Arrow) {
            let name = self.consume_identifier("parameter name")?;
            let type_expr = self.try_parse_type_annotation()?;
            params.push(TypedParam { name, type_expr });
            if self.check(&TokenKind::Comma) {
                self.advance();
                self.skip_newlines();
            }
        }
        Ok(params)
    }

    fn parse_dict_literal(&mut self, start: Span) -> Result<SNode, ParserError> {
        let mut entries = Vec::new();
        self.skip_newlines();

        while !self.is_at_end() && !self.check(&TokenKind::RBrace) {
            let key = if self.check(&TokenKind::LBracket) {
                // Computed key: [expression]
                self.advance();
                let k = self.parse_expression()?;
                self.consume(&TokenKind::RBracket, "]")?;
                k
            } else {
                // Static key: identifier -> string literal
                let key_span = self.current_span();
                let name = self.consume_identifier("dict key")?;
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
        Ok(spanned(
            Node::DictLiteral(entries),
            Span::merge(start, self.prev_span()),
        ))
    }

    // --- Helpers ---

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
        let mut params = Vec::new();
        self.skip_newlines();

        while !self.is_at_end() && !self.check(&TokenKind::RParen) {
            let name = self.consume_identifier("parameter name")?;
            let type_expr = self.try_parse_type_annotation()?;
            params.push(TypedParam { name, type_expr });
            if self.check(&TokenKind::Comma) {
                self.advance();
                self.skip_newlines();
            }
        }
        Ok(params)
    }

    /// Try to parse an optional type annotation (`: type`).
    /// Returns None if no colon follows.
    fn try_parse_type_annotation(&mut self) -> Result<Option<TypeExpr>, ParserError> {
        if !self.check(&TokenKind::Colon) {
            return Ok(None);
        }
        self.advance(); // skip :
        Ok(Some(self.parse_type_expr()?))
    }

    /// Parse a type expression: `int`, `string | nil`, `{name: string, age?: int}`.
    fn parse_type_expr(&mut self) -> Result<TypeExpr, ParserError> {
        self.skip_newlines();
        let first = self.parse_type_primary()?;

        // Check for union: type | type | ...
        if self.check(&TokenKind::Bar) {
            let mut types = vec![first];
            while self.check(&TokenKind::Bar) {
                self.advance(); // skip |
                types.push(self.parse_type_primary()?);
            }
            return Ok(TypeExpr::Union(types));
        }

        Ok(first)
    }

    /// Parse a primary type: named type or shape type.
    /// Accepts identifiers and certain keywords (nil, bool, etc.) as type names.
    fn parse_type_primary(&mut self) -> Result<TypeExpr, ParserError> {
        self.skip_newlines();
        if self.check(&TokenKind::LBrace) {
            return self.parse_shape_type();
        }
        // Accept keyword type names: nil, true, false map to their type names
        if let Some(tok) = self.current() {
            let type_name = match &tok.kind {
                TokenKind::Nil => {
                    self.advance();
                    return Ok(TypeExpr::Named("nil".to_string()));
                }
                TokenKind::True | TokenKind::False => {
                    self.advance();
                    return Ok(TypeExpr::Named("bool".to_string()));
                }
                _ => None,
            };
            if let Some(name) = type_name {
                return Ok(TypeExpr::Named(name));
            }
        }
        let name = self.consume_identifier("type name")?;
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
            args.push(self.parse_expression()?);
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

    fn check(&self, kind: &TokenKind) -> bool {
        self.current()
            .map(|t| std::mem::discriminant(&t.kind) == std::mem::discriminant(kind))
            .unwrap_or(false)
    }

    fn advance(&mut self) {
        if self.pos < self.tokens.len() {
            self.pos += 1;
        }
    }

    fn consume(&mut self, kind: &TokenKind, expected: &str) -> Result<Token, ParserError> {
        self.skip_newlines();
        let tok = self.current().ok_or_else(|| ParserError::UnexpectedEof {
            expected: expected.into(),
        })?;
        if std::mem::discriminant(&tok.kind) != std::mem::discriminant(kind) {
            if tok.kind == TokenKind::Eof {
                return Err(ParserError::UnexpectedEof {
                    expected: expected.into(),
                });
            }
            return Err(ParserError::Unexpected {
                got: tok.kind.to_string(),
                expected: expected.into(),
                line: tok.span.line,
                column: tok.span.column,
            });
        }
        let tok = tok.clone();
        self.advance();
        Ok(tok)
    }

    fn consume_identifier(&mut self, expected: &str) -> Result<String, ParserError> {
        self.skip_newlines();
        let tok = self.current().ok_or_else(|| ParserError::UnexpectedEof {
            expected: expected.into(),
        })?;
        if let TokenKind::Identifier(name) = &tok.kind {
            let name = name.clone();
            self.advance();
            Ok(name)
        } else if tok.kind == TokenKind::Eof {
            Err(ParserError::UnexpectedEof {
                expected: expected.into(),
            })
        } else {
            Err(ParserError::Unexpected {
                got: tok.kind.to_string(),
                expected: expected.into(),
                line: tok.span.line,
                column: tok.span.column,
            })
        }
    }

    fn skip_newlines(&mut self) {
        while self.pos < self.tokens.len() && self.tokens[self.pos].kind == TokenKind::Newline {
            self.pos += 1;
        }
    }

    fn error(&self, expected: &str) -> ParserError {
        if let Some(tok) = self.current() {
            if tok.kind == TokenKind::Eof {
                return ParserError::UnexpectedEof {
                    expected: expected.into(),
                };
            }
            ParserError::Unexpected {
                got: tok.kind.to_string(),
                expected: expected.into(),
                line: tok.span.line,
                column: tok.span.column,
            }
        } else {
            ParserError::UnexpectedEof {
                expected: expected.into(),
            }
        }
    }
}
