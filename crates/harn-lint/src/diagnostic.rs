//! Public diagnostic types emitted by the linter and caller-supplied
//! options. Kept separate from the linter's walk state so the public
//! surface is easy to locate and audit.

use std::borrow::Cow;

use harn_lexer::{FixEdit, Span};
use harn_parser::{DiagnosticCode as Code, Repair};

/// A lint diagnostic reported by the linter.
#[derive(Debug, Clone)]
pub struct LintDiagnostic {
    pub code: Code,
    /// Stable rule identifier. Owned (`Cow`) so dynamically-registered
    /// rules — engine patterns, `.harn`-authored rules — can carry a
    /// runtime id, while built-in rules stay zero-cost `Cow::Borrowed`.
    pub rule: Cow<'static, str>,
    pub message: String,
    pub span: Span,
    pub severity: LintSeverity,
    pub suggestion: Option<String>,
    /// Machine-applicable fix edits (applied in order, non-overlapping).
    pub fix: Option<Vec<FixEdit>>,
}

impl LintDiagnostic {
    /// Materialize the structured [`Repair`] for this lint, derived from
    /// the central diagnostic-code registry. Returns `None` when no
    /// actionable repair shape is registered.
    ///
    /// `LintDiagnostic` does not store `repair` inline (unlike
    /// `TypeDiagnostic`) because every lint's safety class is purely a
    /// function of its `code`; storing a per-site copy would invite
    /// drift. Callers that need the dispatch handle ask for it here.
    pub fn repair(&self) -> Option<Repair> {
        self.code.repair_template().map(Repair::from_template)
    }
}

/// Severity level for lint diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LintSeverity {
    Info,
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
    /// Extra non-stdlib function names that persona bodies may call
    /// without requiring a `@step` declaration.
    pub persona_step_allowlist: &'a [String],
    /// When true, the `HARN-STD-101` lint enforces a complete
    /// `@effects`/`@allocation`/`@errors`/`@api_stability`/`@example`
    /// block on every `pub fn`. Auto-enabled by `harn lint` for files
    /// under `crates/harn-stdlib/src/stdlib/`.
    pub require_stdlib_metadata: bool,
}
