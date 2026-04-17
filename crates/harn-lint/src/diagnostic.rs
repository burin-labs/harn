//! Public diagnostic types emitted by the linter and caller-supplied
//! options. Kept separate from the linter's walk state so the public
//! surface is easy to locate and audit.

use harn_lexer::{FixEdit, Span};

/// A lint diagnostic reported by the linter.
#[derive(Debug, Clone)]
pub struct LintDiagnostic {
    pub rule: &'static str,
    pub message: String,
    pub span: Span,
    pub severity: LintSeverity,
    pub suggestion: Option<String>,
    /// Machine-applicable fix edits (applied in order, non-overlapping).
    pub fix: Option<Vec<FixEdit>>,
}

/// Severity level for lint diagnostics.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LintSeverity {
    Warning,
    Error,
}

/// Default cyclomatic-complexity threshold. Callers can override via
/// [`LintOptions::complexity_threshold`] (wired to
/// `[lint].complexity_threshold` in `harn.toml`). Chosen to match
/// Clippy's `cognitive_complexity` default and sit between ESLint (20)
/// and gocyclo (30); Harn's scorer counts `&&`/`||` per operator, so
/// real-world Harn functions score a notch higher than in tools that
/// only count control-flow nodes.
pub const DEFAULT_COMPLEXITY_THRESHOLD: usize = 25;

/// Extra options for source-aware lint rules (path-aware rules, opt-in
/// rules like `require-file-header`).
#[derive(Debug, Default, Clone)]
pub struct LintOptions<'a> {
    /// Filesystem path of the source being linted. Used by rules like
    /// `require-file-header` to derive a title from the basename.
    pub file_path: Option<&'a std::path::Path>,
    /// When true, the opt-in `require-file-header` rule runs.
    pub require_file_header: bool,
    /// Override the cyclomatic-complexity threshold. `None` uses
    /// [`DEFAULT_COMPLEXITY_THRESHOLD`].
    pub complexity_threshold: Option<usize>,
}
