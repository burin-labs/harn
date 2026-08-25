//! Public diagnostic types emitted by the linter and caller-supplied
//! options. Kept separate from the linter's walk state so the public
//! surface is easy to locate and audit.

use std::borrow::Cow;
use std::path::PathBuf;

use harn_lexer::{FixEdit, Span};
pub use harn_modules::project_config::LintSeverity;
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
    /// Concrete fix edits (applied in order, non-overlapping). The repair
    /// safety class decides whether bulk autofix may apply them.
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

    /// Return concrete edits only when the repair registry permits automatic
    /// application. Higher-safety edits remain available to explicit
    /// `harn fix --safety ...` and IDE quick-fix flows.
    pub fn machine_applicable_fix(&self) -> Option<&[FixEdit]> {
        let fix = self.fix.as_deref()?;
        self.code
            .repair_template()
            .is_none_or(|repair| repair.safety.is_machine_applicable())
            .then_some(fix)
    }
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
    /// When true, the opt-in `missing-harndoc` rule warns on public
    /// functions without a `/** */` doc comment. Off by default —
    /// enabled via `[lint] require_docstrings = true` in `harn.toml`,
    /// and implied by `require_stdlib_metadata`.
    pub require_docstrings: bool,
    /// Override the cyclomatic-complexity threshold. `None` uses
    /// [`DEFAULT_COMPLEXITY_THRESHOLD`].
    pub complexity_threshold: Option<usize>,
    /// Extra non-stdlib function names that persona bodies may call
    /// without requiring a `@step` declaration.
    pub persona_step_allowlist: &'a [String],
    /// When true, stdlib contract lints enforce an `@effects` + `@errors`
    /// metadata block (HARN-STD-101) and an explicit return type
    /// (HARN-STD-102) on every public stdlib `pub fn`. Auto-enabled by
    /// `harn lint` for files under `crates/harn-stdlib/src/stdlib/`.
    pub require_stdlib_metadata: bool,
    /// TOML sources of declarative rule-engine rules to run as lint rules,
    /// loaded from the project's `[rules] ruleDirs`. Each is compiled and
    /// wrapped as a `Rule`; an invalid one is skipped, not fatal.
    pub engine_rules: &'a [String],
    /// Trusted native rule libraries to load for this lint run. These are
    /// discovered only from explicit `[rules] nativeRuleDirs` entries in project
    /// config; loading native code is an opt-in capability decision.
    pub native_rule_paths: &'a [PathBuf],
    /// Per-rule severity overrides: a rule id → the severity to report its
    /// diagnostics at, applied after disable-filtering. Lets a project promote
    /// a rule to `error` or demote one to `info` via `[lint]`. Owned (not
    /// `&[..]`) because a `HashMap` reference has no const empty default.
    pub severity_overrides: std::collections::HashMap<String, LintSeverity>,
    /// When true, this source is a privileged artifact, so calling a builtin
    /// declared `privileged_wire` is sanctioned rather than a finding.
    ///
    /// This mirrors `harn check --trusted-host-dispatch`, which is how an
    /// embedder type-checks the scripts it serves from its own host bridge.
    /// The two surfaces answering differently is what made `HARN-LNT-072`
    /// report a host's own corpus as broken (harn#6162). Only the
    /// `privileged_wire` finding is suppressed: trusted dispatch says who may
    /// reach the wire, not that a compiler internal became source-visible.
    pub trusted_host_dispatch: bool,
    /// When true, the package manifest declares this file as a provider's
    /// connector module, so its runtime exports keep the root `Harness` the
    /// connector contract pins.
    ///
    /// The attenuation rule otherwise infers connector-ness from the file's
    /// own contents, which is evidence rather than a declaration: move one
    /// metadata export to a sibling module and every runtime export in the
    /// file silently becomes attenuable (harn#6149).
    pub connector_runtime_module: bool,
}
