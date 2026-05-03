use std::path::Path;

use harn_parser::{Parser, SNode};

pub(super) fn parse_resolved_module(path: &Path) -> Option<(String, Vec<SNode>)> {
    let source = harn_modules::read_module_source(path)?;
    let mut lexer = harn_lexer::Lexer::new(&source);
    let tokens = lexer.tokenize().ok()?;
    let mut parser = Parser::new(tokens);
    let program = parser.parse().ok()?;
    Some((source, program))
}
