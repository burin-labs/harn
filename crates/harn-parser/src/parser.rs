use crate::ast::*;
use harn_lexer::{Token, TokenKind};
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

    /// Parse a complete .harn file.
    pub fn parse(&mut self) -> Result<Vec<Node>, ParserError> {
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
    pub fn parse_single_expression(&mut self) -> Result<Node, ParserError> {
        self.skip_newlines();
        self.parse_expression()
    }

    // --- Declarations ---

    fn parse_pipeline(&mut self) -> Result<Node, ParserError> {
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

        Ok(Node::Pipeline {
            name,
            params,
            body,
            extends,
        })
    }

    fn parse_import(&mut self) -> Result<Node, ParserError> {
        self.consume(&TokenKind::Import, "import")?;
        if let Some(tok) = self.current() {
            if let TokenKind::StringLiteral(path) = &tok.kind {
                let path = path.clone();
                self.advance();
                return Ok(Node::ImportDecl { path });
            }
        }
        Err(self.error("import path string"))
    }

    // --- Statements ---

    fn parse_block(&mut self) -> Result<Vec<Node>, ParserError> {
        let mut stmts = Vec::new();
        self.skip_newlines();

        while !self.is_at_end() && !self.check(&TokenKind::RBrace) {
            stmts.push(self.parse_statement()?);
            self.skip_newlines();
        }
        Ok(stmts)
    }

    fn parse_statement(&mut self) -> Result<Node, ParserError> {
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
            _ => self.parse_expression_statement(),
        }
    }

    fn parse_let_binding(&mut self) -> Result<Node, ParserError> {
        self.consume(&TokenKind::Let, "let")?;
        let name = self.consume_identifier("variable name")?;
        self.consume(&TokenKind::Assign, "=")?;
        let value = self.parse_expression()?;
        Ok(Node::LetBinding {
            name,
            value: Box::new(value),
        })
    }

    fn parse_var_binding(&mut self) -> Result<Node, ParserError> {
        self.consume(&TokenKind::Var, "var")?;
        let name = self.consume_identifier("variable name")?;
        self.consume(&TokenKind::Assign, "=")?;
        let value = self.parse_expression()?;
        Ok(Node::VarBinding {
            name,
            value: Box::new(value),
        })
    }

    fn parse_if_else(&mut self) -> Result<Node, ParserError> {
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

        Ok(Node::IfElse {
            condition: Box::new(condition),
            then_body,
            else_body,
        })
    }

    fn parse_for_in(&mut self) -> Result<Node, ParserError> {
        self.consume(&TokenKind::For, "for")?;
        let variable = self.consume_identifier("loop variable")?;
        self.consume(&TokenKind::In, "in")?;
        let iterable = self.parse_expression()?;
        self.consume(&TokenKind::LBrace, "{")?;
        let body = self.parse_block()?;
        self.consume(&TokenKind::RBrace, "}")?;
        Ok(Node::ForIn {
            variable,
            iterable: Box::new(iterable),
            body,
        })
    }

    fn parse_match(&mut self) -> Result<Node, ParserError> {
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
        Ok(Node::MatchExpr {
            value: Box::new(value),
            arms,
        })
    }

    fn parse_while_loop(&mut self) -> Result<Node, ParserError> {
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
        Ok(Node::WhileLoop {
            condition: Box::new(condition),
            body,
        })
    }

    fn parse_retry(&mut self) -> Result<Node, ParserError> {
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
        Ok(Node::Retry {
            count: Box::new(count),
            body,
        })
    }

    fn parse_parallel(&mut self) -> Result<Node, ParserError> {
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
        Ok(Node::Parallel {
            count: Box::new(count),
            variable,
            body,
        })
    }

    fn parse_parallel_map(&mut self) -> Result<Node, ParserError> {
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
        Ok(Node::ParallelMap {
            list: Box::new(list),
            variable,
            body,
        })
    }

    fn parse_return(&mut self) -> Result<Node, ParserError> {
        self.consume(&TokenKind::Return, "return")?;
        if self.is_at_end() || self.check(&TokenKind::Newline) || self.check(&TokenKind::RBrace) {
            return Ok(Node::ReturnStmt { value: None });
        }
        let value = self.parse_expression()?;
        Ok(Node::ReturnStmt {
            value: Some(Box::new(value)),
        })
    }

    fn parse_throw(&mut self) -> Result<Node, ParserError> {
        self.consume(&TokenKind::Throw, "throw")?;
        let value = self.parse_expression()?;
        Ok(Node::ThrowStmt {
            value: Box::new(value),
        })
    }

    fn parse_override(&mut self) -> Result<Node, ParserError> {
        self.consume(&TokenKind::Override, "override")?;
        let name = self.consume_identifier("override name")?;
        self.consume(&TokenKind::LParen, "(")?;
        let params = self.parse_param_list()?;
        self.consume(&TokenKind::RParen, ")")?;
        self.consume(&TokenKind::LBrace, "{")?;
        let body = self.parse_block()?;
        self.consume(&TokenKind::RBrace, "}")?;
        Ok(Node::OverrideDecl { name, params, body })
    }

    fn parse_try_catch(&mut self) -> Result<Node, ParserError> {
        self.consume(&TokenKind::Try, "try")?;
        self.consume(&TokenKind::LBrace, "{")?;
        let body = self.parse_block()?;
        self.consume(&TokenKind::RBrace, "}")?;
        self.skip_newlines();
        self.consume(&TokenKind::Catch, "catch")?;

        let error_var = if self.check(&TokenKind::LParen) {
            self.advance();
            let name = self.consume_identifier("error variable")?;
            self.consume(&TokenKind::RParen, ")")?;
            Some(name)
        } else {
            None
        };

        self.consume(&TokenKind::LBrace, "{")?;
        let catch_body = self.parse_block()?;
        self.consume(&TokenKind::RBrace, "}")?;
        Ok(Node::TryCatch {
            body,
            error_var,
            catch_body,
        })
    }

    fn parse_fn_decl(&mut self) -> Result<Node, ParserError> {
        self.consume(&TokenKind::Fn, "fn")?;
        let name = self.consume_identifier("function name")?;
        self.consume(&TokenKind::LParen, "(")?;
        let params = self.parse_param_list()?;
        self.consume(&TokenKind::RParen, ")")?;
        self.consume(&TokenKind::LBrace, "{")?;
        let body = self.parse_block()?;
        self.consume(&TokenKind::RBrace, "}")?;
        Ok(Node::FnDecl { name, params, body })
    }

    // --- Expressions (precedence climbing) ---

    fn parse_expression_statement(&mut self) -> Result<Node, ParserError> {
        let expr = self.parse_expression()?;

        // Check for assignment: identifier = value
        if self.check(&TokenKind::Assign) {
            if matches!(expr, Node::Identifier(_)) {
                self.advance();
                let value = self.parse_expression()?;
                return Ok(Node::Assignment {
                    target: Box::new(expr),
                    value: Box::new(value),
                });
            }
        }

        Ok(expr)
    }

    fn parse_expression(&mut self) -> Result<Node, ParserError> {
        self.skip_newlines();
        self.parse_pipe()
    }

    fn parse_pipe(&mut self) -> Result<Node, ParserError> {
        let mut left = self.parse_ternary()?;
        while self.check(&TokenKind::Pipe) {
            self.advance();
            let right = self.parse_ternary()?;
            left = Node::BinaryOp {
                op: "|>".into(),
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_ternary(&mut self) -> Result<Node, ParserError> {
        let condition = self.parse_nil_coalescing()?;
        if !self.check(&TokenKind::Question) {
            return Ok(condition);
        }
        self.advance(); // skip ?
        let true_val = self.parse_nil_coalescing()?;
        self.consume(&TokenKind::Colon, ":")?;
        let false_val = self.parse_nil_coalescing()?;
        Ok(Node::Ternary {
            condition: Box::new(condition),
            true_expr: Box::new(true_val),
            false_expr: Box::new(false_val),
        })
    }

    fn parse_nil_coalescing(&mut self) -> Result<Node, ParserError> {
        let mut left = self.parse_logical_or()?;
        while self.check(&TokenKind::NilCoal) {
            self.advance();
            let right = self.parse_logical_or()?;
            left = Node::BinaryOp {
                op: "??".into(),
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_logical_or(&mut self) -> Result<Node, ParserError> {
        let mut left = self.parse_logical_and()?;
        while self.check(&TokenKind::Or) {
            self.advance();
            let right = self.parse_logical_and()?;
            left = Node::BinaryOp {
                op: "||".into(),
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_logical_and(&mut self) -> Result<Node, ParserError> {
        let mut left = self.parse_equality()?;
        while self.check(&TokenKind::And) {
            self.advance();
            let right = self.parse_equality()?;
            left = Node::BinaryOp {
                op: "&&".into(),
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_equality(&mut self) -> Result<Node, ParserError> {
        let mut left = self.parse_comparison()?;
        while self.check(&TokenKind::Eq) || self.check(&TokenKind::Neq) {
            let op = if self.check(&TokenKind::Eq) {
                "=="
            } else {
                "!="
            };
            self.advance();
            let right = self.parse_comparison()?;
            left = Node::BinaryOp {
                op: op.into(),
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_comparison(&mut self) -> Result<Node, ParserError> {
        let mut left = self.parse_additive()?;
        while self.check(&TokenKind::Lt)
            || self.check(&TokenKind::Gt)
            || self.check(&TokenKind::Lte)
            || self.check(&TokenKind::Gte)
        {
            let op = match self.current().unwrap().kind {
                TokenKind::Lt => "<",
                TokenKind::Gt => ">",
                TokenKind::Lte => "<=",
                TokenKind::Gte => ">=",
                _ => "<",
            };
            self.advance();
            let right = self.parse_additive()?;
            left = Node::BinaryOp {
                op: op.into(),
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_additive(&mut self) -> Result<Node, ParserError> {
        let mut left = self.parse_multiplicative()?;
        while self.check(&TokenKind::Plus) || self.check(&TokenKind::Minus) {
            let op = if self.check(&TokenKind::Plus) {
                "+"
            } else {
                "-"
            };
            self.advance();
            let right = self.parse_multiplicative()?;
            left = Node::BinaryOp {
                op: op.into(),
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_multiplicative(&mut self) -> Result<Node, ParserError> {
        let mut left = self.parse_unary()?;
        while self.check(&TokenKind::Star) || self.check(&TokenKind::Slash) {
            let op = if self.check(&TokenKind::Star) {
                "*"
            } else {
                "/"
            };
            self.advance();
            let right = self.parse_unary()?;
            left = Node::BinaryOp {
                op: op.into(),
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Node, ParserError> {
        if self.check(&TokenKind::Not) {
            self.advance();
            let operand = self.parse_unary()?;
            return Ok(Node::UnaryOp {
                op: "!".into(),
                operand: Box::new(operand),
            });
        }
        if self.check(&TokenKind::Minus) {
            self.advance();
            let operand = self.parse_unary()?;
            return Ok(Node::UnaryOp {
                op: "-".into(),
                operand: Box::new(operand),
            });
        }
        self.parse_postfix()
    }

    fn parse_postfix(&mut self) -> Result<Node, ParserError> {
        let mut expr = self.parse_primary()?;

        loop {
            if self.check(&TokenKind::Dot) {
                self.advance();
                let member = self.consume_identifier("member name")?;
                if self.check(&TokenKind::LParen) {
                    self.advance();
                    let args = self.parse_arg_list()?;
                    self.consume(&TokenKind::RParen, ")")?;
                    expr = Node::MethodCall {
                        object: Box::new(expr),
                        method: member,
                        args,
                    };
                } else {
                    expr = Node::PropertyAccess {
                        object: Box::new(expr),
                        property: member,
                    };
                }
            } else if self.check(&TokenKind::LBracket) {
                self.advance();
                let index = self.parse_expression()?;
                self.consume(&TokenKind::RBracket, "]")?;
                expr = Node::SubscriptAccess {
                    object: Box::new(expr),
                    index: Box::new(index),
                };
            } else if self.check(&TokenKind::LParen) && matches!(expr, Node::Identifier(_)) {
                self.advance();
                let args = self.parse_arg_list()?;
                self.consume(&TokenKind::RParen, ")")?;
                if let Node::Identifier(name) = expr {
                    expr = Node::FunctionCall { name, args };
                }
            } else {
                break;
            }
        }

        Ok(expr)
    }

    fn parse_primary(&mut self) -> Result<Node, ParserError> {
        let tok = self.current().ok_or_else(|| ParserError::UnexpectedEof {
            expected: "expression".into(),
        })?;

        match &tok.kind {
            TokenKind::StringLiteral(s) => {
                let s = s.clone();
                self.advance();
                Ok(Node::StringLiteral(s))
            }
            TokenKind::InterpolatedString(segments) => {
                let segments = segments.clone();
                self.advance();
                Ok(Node::InterpolatedString(segments))
            }
            TokenKind::IntLiteral(n) => {
                let n = *n;
                self.advance();
                Ok(Node::IntLiteral(n))
            }
            TokenKind::FloatLiteral(n) => {
                let n = *n;
                self.advance();
                Ok(Node::FloatLiteral(n))
            }
            TokenKind::True => {
                self.advance();
                Ok(Node::BoolLiteral(true))
            }
            TokenKind::False => {
                self.advance();
                Ok(Node::BoolLiteral(false))
            }
            TokenKind::Nil => {
                self.advance();
                Ok(Node::NilLiteral)
            }
            TokenKind::Identifier(name) => {
                let name = name.clone();
                self.advance();
                Ok(Node::Identifier(name))
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
            _ => Err(self.error("expression")),
        }
    }

    fn parse_spawn_expr(&mut self) -> Result<Node, ParserError> {
        self.consume(&TokenKind::Spawn, "spawn")?;
        self.consume(&TokenKind::LBrace, "{")?;
        let body = self.parse_block()?;
        self.consume(&TokenKind::RBrace, "}")?;
        Ok(Node::SpawnExpr { body })
    }

    fn parse_list_literal(&mut self) -> Result<Node, ParserError> {
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
        Ok(Node::ListLiteral(elements))
    }

    fn parse_dict_or_closure(&mut self) -> Result<Node, ParserError> {
        self.consume(&TokenKind::LBrace, "{")?;
        self.skip_newlines();

        // Empty dict
        if self.check(&TokenKind::RBrace) {
            self.advance();
            return Ok(Node::DictLiteral(Vec::new()));
        }

        let saved = self.pos;
        if let Some(tok) = self.current() {
            if let TokenKind::Identifier(name) = &tok.kind {
                let name = name.clone();
                let next = self.peek_kind();

                if next == Some(&TokenKind::Arrow) {
                    // Single-param closure: { param -> body }
                    self.advance(); // skip identifier
                    self.advance(); // skip ->
                    let body = self.parse_block()?;
                    self.consume(&TokenKind::RBrace, "}")?;
                    return Ok(Node::Closure {
                        params: vec![name],
                        body,
                    });
                }

                if next == Some(&TokenKind::Comma) {
                    // Try multi-param closure: { a, b -> body }
                    let param_saved = self.pos;
                    let mut params = vec![name];
                    self.advance(); // skip first identifier
                    while self.check(&TokenKind::Comma) {
                        self.advance(); // skip comma
                        self.skip_newlines();
                        if let Some(tok) = self.current() {
                            if let TokenKind::Identifier(p) = &tok.kind {
                                params.push(p.clone());
                                self.advance();
                            } else {
                                // Not a closure — restore
                                self.pos = saved;
                                return self.parse_dict_literal();
                            }
                        } else {
                            self.pos = saved;
                            return self.parse_dict_literal();
                        }
                    }
                    if self.check(&TokenKind::Arrow) {
                        self.advance(); // skip ->
                        let body = self.parse_block()?;
                        self.consume(&TokenKind::RBrace, "}")?;
                        return Ok(Node::Closure { params, body });
                    }
                    // Not a closure — restore
                    let _ = param_saved;
                    self.pos = saved;
                    return self.parse_dict_literal();
                }

                if next == Some(&TokenKind::Colon) {
                    self.pos = saved;
                    return self.parse_dict_literal();
                }
            }
        }

        self.pos = saved;
        self.parse_dict_literal()
    }

    fn parse_dict_literal(&mut self) -> Result<Node, ParserError> {
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
                // Static key: identifier → string literal
                let name = self.consume_identifier("dict key")?;
                Node::StringLiteral(name)
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
        Ok(Node::DictLiteral(entries))
    }

    // --- Helpers ---

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

    fn parse_arg_list(&mut self) -> Result<Vec<Node>, ParserError> {
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
