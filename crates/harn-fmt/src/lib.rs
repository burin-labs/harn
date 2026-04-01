mod formatter;
mod helpers;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;

use harn_lexer::{Lexer, TokenKind};
use harn_parser::Parser;

/// A captured comment with metadata.
#[derive(Debug, Clone)]
struct Comment {
    text: String,
    is_block: bool,
}

/// Options controlling formatter behavior.
#[derive(Debug, Clone)]
pub struct FmtOptions {
    /// Maximum line width before wrapping (default: 100).
    pub line_width: usize,
}

impl Default for FmtOptions {
    fn default() -> Self {
        Self { line_width: 100 }
    }
}

/// Format Harn source code to canonical style using default options.
pub fn format_source(source: &str) -> Result<String, String> {
    format_source_opts(source, &FmtOptions::default())
}

/// Format Harn source code with explicit options.
pub fn format_source_opts(source: &str, opts: &FmtOptions) -> Result<String, String> {
    // Lex once with comments, then partition
    let mut lexer = Lexer::new(source);
    let all_tokens = lexer.tokenize_with_comments().map_err(|e| e.to_string())?;

    // Extract comments by source line, and filter to parser tokens
    let mut comments: BTreeMap<usize, Vec<Comment>> = BTreeMap::new();
    let mut parser_tokens = Vec::with_capacity(all_tokens.len());
    for tok in all_tokens {
        match &tok.kind {
            TokenKind::LineComment(text) => {
                comments.entry(tok.span.line).or_default().push(Comment {
                    text: text.clone(),
                    is_block: false,
                });
            }
            TokenKind::BlockComment(text) => {
                comments.entry(tok.span.line).or_default().push(Comment {
                    text: text.clone(),
                    is_block: true,
                });
            }
            _ => parser_tokens.push(tok),
        }
    }

    let mut parser = Parser::new(parser_tokens);
    let program = parser.parse().map_err(|e| e.to_string())?;

    let mut fmt = Formatter::new(comments, opts.line_width);
    fmt.format_program(&program);
    Ok(fmt.finish())
}

pub(crate) struct Formatter {
    output: String,
    indent: usize,
    line_width: usize,
    /// Line → comments on that line.
    comments: BTreeMap<usize, Vec<Comment>>,
    /// Track which comment lines have been emitted.
    emitted_lines: std::collections::HashSet<usize>,
}

impl Formatter {
    pub(crate) fn new(comments: BTreeMap<usize, Vec<Comment>>, line_width: usize) -> Self {
        Self {
            output: String::new(),
            indent: 0,
            line_width,
            comments,
            emitted_lines: std::collections::HashSet::new(),
        }
    }

    pub(crate) fn finish(mut self) -> String {
        // Trim trailing whitespace from each line, ensure single newline at end
        let trimmed: Vec<&str> = self.output.lines().map(|l| l.trim_end()).collect();
        self.output = trimmed.join("\n");
        if !self.output.ends_with('\n') {
            self.output.push('\n');
        }
        self.output
    }

    pub(crate) fn write_indent(&mut self) {
        for _ in 0..self.indent {
            self.output.push_str("  ");
        }
    }

    pub(crate) fn indent(&mut self) {
        self.indent += 1;
    }

    pub(crate) fn dedent(&mut self) {
        self.indent = self.indent.saturating_sub(1);
    }

    pub(crate) fn writeln(&mut self, s: &str) {
        self.write_indent();
        self.output.push_str(s);
        self.output.push('\n');
    }

    /// Emit any comments on the given source line that haven't been emitted yet.
    pub(crate) fn emit_comments_for_line(&mut self, line: usize) {
        if self.emitted_lines.contains(&line) {
            return;
        }
        if let Some(comments) = self.comments.get(&line).cloned() {
            self.emitted_lines.insert(line);
            for c in &comments {
                if c.is_block {
                    self.writeln(&format!("/*{}*/", c.text));
                } else {
                    self.writeln(&format!("//{}", c.text));
                }
            }
        }
    }

    /// Emit any standalone comments whose line is between `from` and `to` (exclusive).
    pub(crate) fn emit_comments_in_range(&mut self, from: usize, to: usize) {
        let lines: Vec<usize> = self
            .comments
            .keys()
            .filter(|&&l| l >= from && l < to && !self.emitted_lines.contains(&l))
            .copied()
            .collect();
        for line in lines {
            self.emit_comments_for_line(line);
        }
    }
}
