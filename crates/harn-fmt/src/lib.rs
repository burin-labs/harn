mod formatter;
mod helpers;
#[cfg(test)]
mod tests;
mod trailing_comma;

use std::collections::BTreeMap;
use std::fmt;

use harn_lexer::{Lexer, TokenKind};
use harn_parser::{DiagnosticCode as Code, Parser, Repair};

pub(crate) use formatter::{Comment, Formatter};
pub use trailing_comma::{
    apply_trailing_comma_fixes, trailing_comma_issues, TrailingCommaIssue, TrailingCommaKind,
};

/// `FmtOptions::separator_width` value that resolves section-header bars from
/// `line_width` minus the current indent.
pub const AUTO_SEPARATOR_WIDTH: usize = 0;

/// Maximum line width the formatter targets.
///
/// This is the single budget every wrap decision spends from: calls, method
/// chains, signatures, and struct literals all break against it, and none of
/// them may emit a line past it.
pub const LINE_WIDTH_DEFAULT: usize = 100;

/// Error returned when formatting cannot proceed.
#[derive(Debug, Clone)]
pub struct FormatError {
    pub code: Code,
    pub message: String,
}

impl FormatError {
    fn new(code: Code, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// Materialize the structured repair classifier for this error.
    /// Derived from the central diagnostic-code registry so the autonomy
    /// surface is identical to `TypeDiagnostic::repair` and
    /// `LintDiagnostic::repair`.
    pub fn repair(&self) -> Option<Repair> {
        self.code.repair_template().map(Repair::from_template)
    }
}

impl fmt::Display for FormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for FormatError {}

/// Options controlling formatter behavior.
#[derive(Debug, Clone)]
pub struct FmtOptions {
    /// Maximum line width before wrapping (default: 100).
    pub line_width: usize,
    /// Total width of `// ----` separator bars rendered by the formatter
    /// when it normalizes section-header comment blocks. Use
    /// `AUTO_SEPARATOR_WIDTH` to resolve from `line_width` minus the current
    /// indent.
    pub separator_width: usize,
}

impl Default for FmtOptions {
    fn default() -> Self {
        Self {
            line_width: LINE_WIDTH_DEFAULT,
            separator_width: AUTO_SEPARATOR_WIDTH,
        }
    }
}

/// Format Harn source code to canonical style using default options.
pub fn format_source(source: &str) -> Result<String, FormatError> {
    format_source_opts(source, &FmtOptions::default())
}

/// Format Harn source code with explicit options.
pub fn format_source_opts(source: &str, opts: &FmtOptions) -> Result<String, FormatError> {
    // Preserve a shebang line (`#!...`) at offset 0 — the lexer skips it
    // entirely, so without explicit reattachment the formatter would drop it.
    let (shebang, source_for_lex) = if source.starts_with("#!") {
        match source.find('\n') {
            Some(i) => (Some(&source[..=i]), &source[i + 1..]),
            None => (Some(source), ""),
        }
    } else {
        (None, source)
    };

    // Lex with comments preserved, then split into (comments by line, parser tokens).
    let mut lexer = Lexer::new(source_for_lex);
    let all_tokens = lexer
        .tokenize_with_comments()
        .map_err(|e| FormatError::new(Code::FormatterParseFailed, e.to_string()))?;

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
    let program = parser
        .parse()
        .map_err(|e| FormatError::new(Code::FormatterParseFailed, e.to_string()))?;

    let mut fmt = Formatter::new(
        source_for_lex,
        comments,
        opts.line_width,
        opts.separator_width,
    );
    fmt.format_program(&program);
    let formatted = fmt.finish();
    // Token-based surface-format pass: catches any trailing-comma cases
    // the AST formatter did not normalize (e.g. the original layout was
    // preserved verbatim because the formatter's wrap heuristic happened
    // to keep it). Without this pass `harn fmt` would leave behind
    // diagnostics that `harn lint --fix` is willing to repair.
    let formatted = apply_trailing_comma_fixes(&formatted);
    Ok(match shebang {
        Some(line) => {
            let trailing = if line.ends_with('\n') { "" } else { "\n" };
            format!("{line}{trailing}{formatted}")
        }
        None => formatted,
    })
}
