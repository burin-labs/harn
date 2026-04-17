use crate::ast::*;
use harn_lexer::TokenKind;

use super::error::ParserError;
use super::state::Parser;

impl Parser {
    /// Parse a binding pattern for let/var/for-in:
    ///   identifier | { fields } | [ elements ]
    pub(super) fn parse_binding_pattern(&mut self) -> Result<BindingPattern, ParserError> {
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
}
