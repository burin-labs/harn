mod formatter;
mod helpers;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;

use harn_lexer::{Lexer, TokenKind};
use harn_parser::Parser;

pub(crate) use formatter::{Comment, Formatter};

/// Options controlling formatter behavior.
#[derive(Debug, Clone)]
pub struct FmtOptions {
    /// Maximum line width before wrapping (default: 100).
    pub line_width: usize,
    /// Total width of `// ----` separator bars rendered by the formatter
    /// when it normalizes section-header comment blocks (default: 80).
    pub separator_width: usize,
}

impl Default for FmtOptions {
    fn default() -> Self {
        Self {
            line_width: 100,
            separator_width: 80,
        }
    }
}

/// Format Harn source code to canonical style using default options.
pub fn format_source(source: &str) -> Result<String, String> {
    format_source_opts(source, &FmtOptions::default())
}

/// Format Harn source code with explicit options.
pub fn format_source_opts(source: &str, opts: &FmtOptions) -> Result<String, String> {
    // Lex with comments preserved, then split into (comments by line, parser tokens).
    let mut lexer = Lexer::new(source);
    let all_tokens = lexer.tokenize_with_comments().map_err(|e| e.to_string())?;

    let mut comments: BTreeMap<usize, Vec<Comment>> = BTreeMap::new();
    let mut parser_tokens = Vec::with_capacity(all_tokens.len());
    for tok in all_tokens {
        match &tok.kind {
            TokenKind::LineComment { text, is_doc } => {
                comments.entry(tok.span.line).or_default().push(Comment {
                    text: text.clone(),
                    is_block: false,
                    is_doc: *is_doc,
                });
            }
            TokenKind::BlockComment { text, is_doc } => {
                comments.entry(tok.span.line).or_default().push(Comment {
                    text: text.clone(),
                    is_block: true,
                    is_doc: *is_doc,
                });
            }
            _ => parser_tokens.push(tok),
        }
    }

    let mut parser = Parser::new(parser_tokens);
    let program = parser.parse().map_err(|e| e.to_string())?;

    let mut fmt = Formatter::new(comments, opts.line_width, opts.separator_width);
    fmt.format_program(&program);
    Ok(fmt.finish())
}
