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
    is_doc: bool,
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
    ///
    /// Non-doc comments are emitted verbatim. Doc comments are coalesced with
    /// any contiguous doc-comment lines that follow (no blank line between
    /// them and no non-doc-comment line interleaved) and rendered as a
    /// canonical `/** */` block.
    pub(crate) fn emit_comments_for_line(&mut self, line: usize) {
        if self.emitted_lines.contains(&line) {
            return;
        }
        let Some(comments) = self.comments.get(&line).cloned() else {
            return;
        };
        let first_is_doc = comments.first().is_some_and(|c| c.is_doc);
        if first_is_doc {
            // Try to consume a contiguous run of doc comments starting at `line`.
            // A run is contiguous if every subsequent line that contains any
            // comment is immediately next (line + 1, line + 2, ...) AND each
            // of those comments is itself a doc comment.
            let mut run_lines: Vec<usize> = vec![line];
            let mut cursor = line + 1;
            while let Some(next_comments) = self.comments.get(&cursor) {
                if self.emitted_lines.contains(&cursor) {
                    break;
                }
                if !next_comments.iter().all(|c| c.is_doc) {
                    break;
                }
                run_lines.push(cursor);
                cursor += 1;
            }
            // Gather the textual lines of the block. A block doc comment may
            // already be multi-line; split its text on `\n`.
            let mut body_lines: Vec<String> = Vec::new();
            for l in &run_lines {
                if let Some(cs) = self.comments.get(l) {
                    for c in cs {
                        if !c.is_doc {
                            continue;
                        }
                        if c.is_block {
                            // Strip the leading/trailing artifacts of the prior
                            // canonical block: leading ` *` on each interior
                            // line, surrounding blank lines.
                            let raw = &c.text;
                            let mut first = true;
                            for raw_line in raw.split('\n') {
                                if first {
                                    first = false;
                                    let t = raw_line.trim();
                                    if t.is_empty() {
                                        continue;
                                    }
                                    body_lines.push(t.to_string());
                                    continue;
                                }
                                let trimmed = raw_line.trim();
                                let stripped = trimmed
                                    .strip_prefix('*')
                                    .map(|s| s.strip_prefix(' ').unwrap_or(s))
                                    .unwrap_or(trimmed);
                                body_lines.push(stripped.to_string());
                            }
                            // Drop trailing all-empty lines
                            while body_lines.last().is_some_and(|s| s.is_empty()) {
                                body_lines.pop();
                            }
                        } else {
                            // Line doc comment: text is everything after `///`.
                            // Trim a single leading space for canonical shape.
                            let t = c.text.strip_prefix(' ').unwrap_or(&c.text);
                            body_lines.push(t.trim_end().to_string());
                        }
                    }
                }
            }
            for l in &run_lines {
                self.emitted_lines.insert(*l);
            }
            self.emit_doc_block(&body_lines);
            return;
        }
        self.emitted_lines.insert(line);
        for c in &comments {
            if c.is_block {
                self.writeln(&format!("/*{}*/", c.text));
            } else {
                self.writeln(&format!("//{}", c.text));
            }
        }
    }

    /// Emit a canonical `/** */` doc block from the given body lines.
    /// If the block is a single non-empty line and the compact form fits
    /// within `line_width`, emit `<indent>/** <text> */`. Otherwise emit the
    /// multi-line JSDoc-style form with vertical-aligned stars.
    pub(crate) fn emit_doc_block(&mut self, body_lines: &[String]) {
        // Drop leading empty lines.
        let mut start = 0;
        while start < body_lines.len() && body_lines[start].trim().is_empty() {
            start += 1;
        }
        let mut end = body_lines.len();
        while end > start && body_lines[end - 1].trim().is_empty() {
            end -= 1;
        }
        let trimmed: Vec<String> = body_lines[start..end].to_vec();
        if trimmed.is_empty() {
            // Empty doc comment — emit minimal compact form.
            self.writeln("/** */");
            return;
        }
        if trimmed.len() == 1 {
            let only = trimmed[0].trim();
            let compact = format!("/** {only} */");
            let indent_cols = self.indent * 2;
            if indent_cols + compact.len() <= self.line_width {
                self.writeln(&compact);
                return;
            }
        }
        // Multi-line canonical form.
        self.writeln("/**");
        for line in &trimmed {
            if line.trim().is_empty() {
                self.writeln(" *");
            } else {
                self.writeln(&format!(" * {}", line.trim_end()));
            }
        }
        self.writeln(" */");
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
