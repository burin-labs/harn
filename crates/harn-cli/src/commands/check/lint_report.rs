//! Structured `--json` report for `harn lint`.
//!
//! Lint mirrors the per-file diagnostic shape already used by
//! `harn check --json` so agent consumers can dispatch on a single
//! `CheckDiagnostic` layout regardless of whether they invoked
//! `check` or `lint`.
//!
//! See `docs/src/cli-json-contract.md` for the envelope contract and
//! `crates/harn-cli/src/json_envelope.rs` for the `JsonEnvelope`
//! wrapper.

use std::collections::HashSet;
use std::path::Path;

use harn_lint::LintSeverity;
use harn_parser::analysis::AnalysisDatabase;
use serde::Serialize;

use crate::package::CheckConfig;

use super::analysis::{analyze_file, FileAnalysisError};
use super::check_cmd::{
    check_diagnostic_from_analysis_error, check_span, CheckDiagnostic, CheckFileStatus,
};
use super::outcome::CommandOutcome;

pub(crate) const LINT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize)]
pub(crate) struct LintReport {
    pub files: Vec<LintFileReport>,
    pub summary: LintSummary,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct LintFileReport {
    pub path: String,
    pub status: CheckFileStatus,
    pub diagnostics: Vec<CheckDiagnostic>,
    /// Number of autofix edits that *would* apply if `--fix` were set
    /// (or that did apply, when `--fix` was set). Mirrors the same
    /// field on the human-readable output.
    pub fixable: usize,
    /// Number of edits actually applied by `--fix`. Always zero when
    /// `--fix` is not set.
    pub fixed: usize,
}

impl LintFileReport {
    pub(crate) fn outcome(&self) -> CommandOutcome {
        CommandOutcome {
            has_error: matches!(self.status, CheckFileStatus::Error),
            has_warning: matches!(self.status, CheckFileStatus::Warning),
            findings: self.diagnostics.len(),
            fixable: self.fixable,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct LintSummary {
    pub ok: usize,
    pub warnings: usize,
    pub errors: usize,
    pub diagnostics: usize,
    pub fixable: usize,
    pub fixed: usize,
}

impl LintReport {
    pub(crate) fn from_files(files: Vec<LintFileReport>) -> Self {
        let mut summary = LintSummary::default();
        for file in &files {
            match file.status {
                CheckFileStatus::Ok => summary.ok += 1,
                CheckFileStatus::Warning => summary.warnings += 1,
                CheckFileStatus::Error => summary.errors += 1,
            }
            summary.diagnostics += file.diagnostics.len();
            summary.fixable += file.fixable;
            summary.fixed += file.fixed;
        }
        Self { files, summary }
    }
}

/// Lint one `.harn` file and return a structured report. Mirrors
/// [`super::lint::lint_file_inner`] but suppresses human-readable
/// stderr rendering and captures every diagnostic into a serializable
/// shape.
#[allow(clippy::too_many_arguments)]
pub(crate) fn lint_file_report(
    analysis: &mut AnalysisDatabase,
    path: &Path,
    config: &CheckConfig,
    externally_imported_names: &HashSet<String>,
    module_graph: &harn_modules::ModuleGraph,
    require_file_header: bool,
    complexity_threshold: Option<usize>,
    persona_step_allowlist: &[String],
    script_rule_diagnostics: &[harn_lint::LintDiagnostic],
) -> LintFileReport {
    let path_str = path.to_string_lossy().into_owned();
    let output = match analyze_file(analysis, path, config, module_graph) {
        Ok(output) => output,
        Err(error) => return file_analysis_error_report(path_str, error),
    };
    let source = output.source;
    let program = output.program;

    let engine_rules = super::lint::project_engine_rule_sources(path);
    let native_rule_paths = super::lint::project_native_rule_paths(path);
    let options = harn_lint::LintOptions {
        file_path: Some(path),
        require_file_header,
        require_docstrings: super::harn_lint_require_docstrings(path),
        complexity_threshold,
        persona_step_allowlist,
        require_stdlib_metadata: harn_lint::path_is_stdlib_source(path),
        engine_rules: &engine_rules,
        native_rule_paths: &native_rule_paths,
        severity_overrides: super::harn_lint_severity_overrides(path),
    };
    let lint_diagnostics = harn_lint::lint_with_module_graph(
        &program,
        &config.disable_rules,
        Some(&source),
        externally_imported_names,
        module_graph,
        path,
        &options,
    );
    let type_lint_diagnostics = harn_lint::lint_diagnostics_from_type_diagnostics(
        &output.diagnostics,
        &config.disable_rules,
    );

    let mut has_error = false;
    let mut has_warning = false;
    let mut fixable = 0usize;
    let mut diagnostics: Vec<CheckDiagnostic> = Vec::new();

    for diag in &lint_diagnostics {
        match diag.severity {
            LintSeverity::Error => has_error = true,
            LintSeverity::Warning => has_warning = true,
            LintSeverity::Info => {}
        }
        if diag.fix.is_some() {
            fixable += 1;
        }
        diagnostics.push(CheckDiagnostic {
            source: "lint",
            severity: lint_severity_label(diag.severity),
            code: Some(diag.code.to_string()),
            message: diag.message.clone(),
            span: Some(check_span(diag.span)),
            help: diag.suggestion.clone(),
        });
    }

    for diag in &type_lint_diagnostics {
        match diag.severity {
            LintSeverity::Error => has_error = true,
            LintSeverity::Warning => has_warning = true,
            LintSeverity::Info => {}
        }
        if diag.fix.is_some() {
            fixable += 1;
        }
        diagnostics.push(CheckDiagnostic {
            source: "lint",
            severity: lint_severity_label(diag.severity),
            code: Some(diag.code.to_string()),
            message: diag.message.clone(),
            span: Some(check_span(diag.span)),
            help: diag.suggestion.clone(),
        });
    }

    // `.harn`-authored custom lint rules (#2850), pre-computed in the async
    // command handler (they need the VM) and merged into the report so they
    // appear and affect status exactly like built-in rules.
    for diag in script_rule_diagnostics.iter().filter(|d| {
        !config
            .disable_rules
            .iter()
            .any(|r| r.as_str() == d.rule.as_ref())
    }) {
        match diag.severity {
            LintSeverity::Error => has_error = true,
            LintSeverity::Warning => has_warning = true,
            LintSeverity::Info => {}
        }
        diagnostics.push(CheckDiagnostic {
            source: "lint",
            severity: lint_severity_label(diag.severity),
            code: Some(diag.code.to_string()),
            message: diag.message.clone(),
            span: Some(check_span(diag.span)),
            help: diag.suggestion.clone(),
        });
    }

    let status = if has_error {
        CheckFileStatus::Error
    } else if has_warning {
        CheckFileStatus::Warning
    } else {
        CheckFileStatus::Ok
    };

    LintFileReport {
        path: path_str,
        status,
        diagnostics,
        fixable,
        fixed: 0,
    }
}

fn lint_severity_label(severity: LintSeverity) -> &'static str {
    match severity {
        LintSeverity::Info => "info",
        LintSeverity::Error => "error",
        LintSeverity::Warning => "warning",
    }
}

fn file_analysis_error_report(path: String, error: FileAnalysisError) -> LintFileReport {
    let diagnostic = match error {
        FileAnalysisError::Read(error) => CheckDiagnostic {
            source: "io",
            severity: "error",
            code: None,
            message: format!("Error reading {path}: {error}"),
            span: None,
            help: None,
        },
        FileAnalysisError::Analysis(error) => check_diagnostic_from_analysis_error(error),
    };
    LintFileReport {
        path,
        status: CheckFileStatus::Error,
        diagnostics: vec![diagnostic],
        fixable: 0,
        fixed: 0,
    }
}
