use crate::ast::*;
use harn_lexer::{Span, TokenKind};

use super::error::ParserError;
use super::state::Parser;

impl Parser {
    pub(super) fn parse_pipeline_with_pub(&mut self, is_pub: bool) -> Result<SNode, ParserError> {
        let start = self.current_span();
        self.consume(&TokenKind::Pipeline, "pipeline")?;
        let name = self.consume_identifier("pipeline name")?;

        self.consume(&TokenKind::LParen, "(")?;
        let params = self.parse_param_list()?;
        self.consume(&TokenKind::RParen, ")")?;

        let return_type = if self.check(&TokenKind::Arrow) {
            self.advance();
            Some(self.parse_type_expr()?)
        } else {
            None
        };

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
                return_type,
                body,
                extends,
                is_pub,
            },
            Span::merge(start, self.prev_span()),
        ))
    }

    pub(super) fn parse_pipeline(&mut self) -> Result<SNode, ParserError> {
        self.parse_pipeline_with_pub(false)
    }

    pub(super) fn parse_import(&mut self) -> Result<SNode, ParserError> {
        self.parse_import_with_pub(false)
    }

    pub(super) fn parse_import_with_pub(&mut self, is_pub: bool) -> Result<SNode, ParserError> {
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
                        Node::SelectiveImport {
                            names,
                            path,
                            is_pub,
                        },
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
                    Node::ImportDecl { path, is_pub },
                    Span::merge(start, self.prev_span()),
                ));
            }
        }
        Err(self.error("import path string"))
    }

    /// Parse one or more `@attr` / `@attr(args)` attributes followed by a
    /// declaration. Returns an `AttributedDecl` wrapping the underlying
    /// declaration. Attributes attach to the next declaration only;
    /// statements other than declarations after `@attr` raise a parse
    /// error.
    pub(super) fn parse_attributed_decl(&mut self) -> Result<SNode, ParserError> {
        let start = self.current_span();
        let mut attributes = Vec::new();
        while self.check(&TokenKind::At) {
            attributes.push(self.parse_one_attribute()?);
            self.skip_newlines();
        }
        // `pipeline` is a top-level form that parse_statement doesn't
        // dispatch to. Route directly so `@attr pipeline foo(...)` works.
        let inner = if self.check(&TokenKind::Pipeline) {
            self.parse_pipeline()?
        } else {
            self.parse_statement()?
        };
        match &inner.node {
            Node::FnDecl { .. }
            | Node::ToolDecl { .. }
            | Node::SkillDecl { .. }
            | Node::Pipeline { .. }
            | Node::StructDecl { .. }
            | Node::EnumDecl { .. }
            | Node::TypeDecl { .. }
            | Node::InterfaceDecl { .. }
            | Node::ImplBlock { .. } => {}
            _ => {
                return Err(ParserError::Unexpected {
                    got: "non-declaration statement".into(),
                    expected:
                        "fn/tool/skill/pipeline/struct/enum/type/interface/impl declaration after `@attr`"
                            .into(),
                    span: inner.span,
                });
            }
        }
        Ok(spanned(
            Node::AttributedDecl {
                attributes,
                inner: Box::new(inner),
            },
            Span::merge(start, self.prev_span()),
        ))
    }

    pub(super) fn parse_one_attribute(&mut self) -> Result<Attribute, ParserError> {
        let at_span = self.current_span();
        self.consume(&TokenKind::At, "@")?;
        let name_span = self.current_span();
        let name = self.consume_identifier("attribute name")?;
        let mut args = Vec::new();
        if self.check(&TokenKind::LParen) {
            self.advance();
            self.skip_newlines();
            while !self.check(&TokenKind::RParen) {
                args.push(self.parse_attribute_arg()?);
                self.skip_newlines();
                if self.check(&TokenKind::Comma) {
                    self.advance();
                    self.skip_newlines();
                } else {
                    break;
                }
            }
            self.consume(&TokenKind::RParen, ")")?;
        }
        let _ = name_span;
        Ok(Attribute {
            name,
            args,
            span: Span::merge(at_span, self.prev_span()),
        })
    }

    pub(super) fn parse_attribute_arg(&mut self) -> Result<AttributeArg, ParserError> {
        let start = self.current_span();
        // Detect `key: value` form by looking ahead.
        if let (Some(t1), Some(t2)) = (self.peek_kind_at(0), self.peek_kind_at(1)) {
            if matches!(t1, TokenKind::Identifier(_)) && matches!(t2, TokenKind::Colon) {
                let key = self.consume_identifier("argument name")?;
                self.consume(&TokenKind::Colon, ":")?;
                let value = self.parse_attribute_value()?;
                return Ok(AttributeArg {
                    name: Some(key),
                    value,
                    span: Span::merge(start, self.prev_span()),
                });
            }
        }
        let value = self.parse_attribute_value()?;
        Ok(AttributeArg {
            name: None,
            value,
            span: Span::merge(start, self.prev_span()),
        })
    }

    /// Parse a literal-or-identifier expression for an attribute argument.
    /// Restricted to keep attribute evaluation purely compile-time:
    /// strings, ints, floats, bools, nil, bare identifiers (typically
    /// type names like `EditArgs` or sentinel values like `allow`), and
    /// list literals containing the same restricted values (used by Flow
    /// `@archivist(evidence: [...])` and similar provenance attributes).
    pub(super) fn parse_attribute_value(&mut self) -> Result<SNode, ParserError> {
        let span = self.current_span();
        let tok = self.current().ok_or_else(|| ParserError::UnexpectedEof {
            expected: "attribute value".into(),
            span: self.prev_span(),
        })?;
        let node = match &tok.kind {
            TokenKind::StringLiteral(s) => Node::StringLiteral(s.clone()),
            TokenKind::RawStringLiteral(s) => Node::RawStringLiteral(s.clone()),
            TokenKind::IntLiteral(i) => Node::IntLiteral(*i),
            TokenKind::FloatLiteral(f) => Node::FloatLiteral(*f),
            TokenKind::True => Node::BoolLiteral(true),
            TokenKind::False => Node::BoolLiteral(false),
            TokenKind::Nil => Node::NilLiteral,
            TokenKind::Identifier(name) => Node::Identifier(name.clone()),
            TokenKind::LBracket => {
                self.advance();
                self.skip_newlines();
                let mut items = Vec::new();
                while !self.check(&TokenKind::RBracket) {
                    items.push(self.parse_attribute_value()?);
                    self.skip_newlines();
                    if self.check(&TokenKind::Comma) {
                        self.advance();
                        self.skip_newlines();
                    } else {
                        break;
                    }
                }
                self.consume(&TokenKind::RBracket, "]")?;
                return Ok(spanned(
                    Node::ListLiteral(items),
                    Span::merge(span, self.prev_span()),
                ));
            }
            TokenKind::Minus => {
                self.advance();
                let inner_tok = self.current().ok_or_else(|| ParserError::UnexpectedEof {
                    expected: "number after '-'".into(),
                    span: self.prev_span(),
                })?;
                let n = match &inner_tok.kind {
                    TokenKind::IntLiteral(i) => Node::IntLiteral(-i),
                    TokenKind::FloatLiteral(f) => Node::FloatLiteral(-f),
                    _ => {
                        return Err(self.error("number after '-' in attribute argument"));
                    }
                };
                self.advance();
                return Ok(spanned(n, Span::merge(span, self.prev_span())));
            }
            _ => return Err(self.error("attribute argument value (literal or identifier)")),
        };
        self.advance();
        Ok(spanned(node, span))
    }

    pub(super) fn parse_fn_decl_with_pub(&mut self, is_pub: bool) -> Result<SNode, ParserError> {
        self.parse_fn_decl_with_pub_and_stream(is_pub, false)
    }

    pub(super) fn parse_gen_fn_decl_with_pub(
        &mut self,
        is_pub: bool,
    ) -> Result<SNode, ParserError> {
        self.parse_fn_decl_with_pub_and_stream(is_pub, true)
    }

    fn parse_fn_decl_with_pub_and_stream(
        &mut self,
        is_pub: bool,
        is_stream: bool,
    ) -> Result<SNode, ParserError> {
        let start = self.current_span();
        if is_stream {
            self.consume_contextual_keyword("gen", "gen")?;
        }
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
                is_stream,
            },
            Span::merge(start, self.prev_span()),
        ))
    }

    pub(super) fn parse_tool_decl(&mut self, is_pub: bool) -> Result<SNode, ParserError> {
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

        if description.is_some() {
            let desc_end_line = self.prev_span().end_line;
            let consumed_sep = self.consume_statement_separator();
            if !consumed_sep && !self.check(&TokenKind::RBrace) {
                self.require_statement_separator(desc_end_line, "tool body item")?;
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

    /// Parse a top-level `pub? skill NAME { <field> <expr> ... }` declaration.
    ///
    /// Each body entry is a `<field_name_identifier> <expression>` pair.
    /// Newlines separate entries. Lifecycle hooks are ordinary fn-literal
    /// expressions (`on_activate fn() { ... }`). No field names are
    /// reserved at the parser level — the compiler validates the schema.
    pub(super) fn parse_skill_decl(&mut self, is_pub: bool) -> Result<SNode, ParserError> {
        let start = self.current_span();
        self.consume(&TokenKind::Skill, "skill")?;
        let name = self.consume_identifier("skill name")?;
        self.consume(&TokenKind::LBrace, "{")?;

        let mut fields: Vec<(String, SNode)> = Vec::new();
        loop {
            self.skip_newlines();
            if self.check(&TokenKind::RBrace) {
                break;
            }
            let field_name = self.consume_identifier("skill field name")?;
            self.skip_newlines();
            let value = self.parse_expression()?;
            let value_end_line = value.span.end_line;
            fields.push((field_name, value));
            let consumed_sep = self.consume_statement_separator();
            if !consumed_sep && !self.check(&TokenKind::RBrace) {
                self.require_statement_separator(value_end_line, "skill field")?;
            }
        }
        self.consume(&TokenKind::RBrace, "}")?;

        Ok(spanned(
            Node::SkillDecl {
                name,
                fields,
                is_pub,
            },
            Span::merge(start, self.prev_span()),
        ))
    }

    pub(super) fn parse_type_decl(&mut self) -> Result<SNode, ParserError> {
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

    pub(super) fn parse_enum_decl_with_pub(&mut self, is_pub: bool) -> Result<SNode, ParserError> {
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

    pub(super) fn parse_enum_decl(&mut self) -> Result<SNode, ParserError> {
        self.parse_enum_decl_with_pub(false)
    }

    pub(super) fn parse_struct_decl_with_pub(
        &mut self,
        is_pub: bool,
    ) -> Result<SNode, ParserError> {
        let start = self.current_span();
        self.consume(&TokenKind::Struct, "struct")?;
        let name = self.consume_identifier("struct name")?;
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

    pub(super) fn parse_struct_decl(&mut self) -> Result<SNode, ParserError> {
        self.parse_struct_decl_with_pub(false)
    }

    pub(super) fn parse_interface_decl(&mut self) -> Result<SNode, ParserError> {
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

    pub(super) fn parse_impl_block(&mut self) -> Result<SNode, ParserError> {
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
}
