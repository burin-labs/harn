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
use harn_parser::{DiagnosticSeverity, TypeChecker, TypeDiagnostic};
use serde::Serialize;

use crate::package::CheckConfig;
use crate::parse_source_file;

use super::check_cmd::{CheckDiagnostic, CheckFileStatus, CheckSpan};
use super::lint::path_is_stdlib_source;
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
pub(crate) fn lint_file_report(
    path: &Path,
    config: &CheckConfig,
    externally_imported_names: &HashSet<String>,
    module_graph: &harn_modules::ModuleGraph,
    require_file_header: bool,
    complexity_threshold: Option<usize>,
    persona_step_allowlist: &[String],
) -> LintFileReport {
    let path_str = path.to_string_lossy().into_owned();
    let source = match std::fs::read_to_string(&path_str) {
        Ok(text) => text,
        Err(error) => {
            return LintFileReport {
                path: path_str.clone(),
                status: CheckFileStatus::Error,
                diagnostics: vec![CheckDiagnostic {
                    source: "io",
                    severity: "error",
                    code: None,
                    message: format!("Error reading {path_str}: {error}"),
                    span: None,
                    help: None,
                }],
                fixable: 0,
                fixed: 0,
            };
        }
    };

    let program = match harn_parser::parse_source(&source) {
        Ok(program) => program,
        Err(error) => {
            let span = error.span().copied();
            let diagnostic = match error {
                harn_parser::PipelineError::Lex(error) => CheckDiagnostic {
                    source: "lexer",
                    severity: "error",
                    code: Some(harn_parser::diagnostic::lexer_error_code(&error).to_string()),
                    message: error.to_string(),
                    span: span.map(check_span),
                    help: None,
                },
                harn_parser::PipelineError::Parse(error) => CheckDiagnostic {
                    source: "parser",
                    severity: "error",
                    code: Some(harn_parser::diagnostic::parser_error_code(&error).to_string()),
                    message: harn_parser::diagnostic::parser_error_message(&error),
                    span: span.map(check_span),
                    help: harn_parser::diagnostic::parser_error_help(&error).map(str::to_string),
                },
                harn_parser::PipelineError::TypeCheck(error) => CheckDiagnostic {
                    source: "type",
                    severity: type_severity_label(error.severity),
                    code: Some(error.code.to_string()),
                    message: error.message.clone(),
                    span: error.span.map(check_span),
                    help: error.help.clone(),
                },
            };
            return LintFileReport {
                path: path_str,
                status: CheckFileStatus::Error,
                diagnostics: vec![diagnostic],
                fixable: 0,
                fixed: 0,
            };
        }
    };

    let options = harn_lint::LintOptions {
        file_path: Some(path),
        require_file_header,
        complexity_threshold,
        persona_step_allowlist,
        require_stdlib_metadata: path_is_stdlib_source(path),
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
    let type_diags = type_check_for_lint(path, config, module_graph, &program, &source);
    let type_lint_diagnostics =
        harn_lint::lint_diagnostics_from_type_diagnostics(&type_diags, &config.disable_rules);

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

fn type_check_for_lint(
    path: &Path,
    config: &CheckConfig,
    module_graph: &harn_modules::ModuleGraph,
    program: &[harn_parser::SNode],
    source: &str,
) -> Vec<TypeDiagnostic> {
    let mut checker = TypeChecker::with_strict_types(config.strict_types);
    if let Some(imported) = module_graph.imported_names_for_file(path) {
        checker = checker.with_imported_names(imported);
    }
    if let Some(imported) = module_graph.imported_type_declarations_for_file(path) {
        checker = checker.with_imported_type_decls(imported);
    }
    if let Some(imported) = module_graph.imported_callable_declarations_for_file(path) {
        checker = checker.with_imported_callable_decls(imported);
    }
    checker.check_with_source(program, source)
}

fn type_severity_label(severity: DiagnosticSeverity) -> &'static str {
    match severity {
        DiagnosticSeverity::Error => "error",
        DiagnosticSeverity::Warning => "warning",
    }
}

fn lint_severity_label(severity: LintSeverity) -> &'static str {
    match severity {
        LintSeverity::Info => "info",
        LintSeverity::Error => "error",
        LintSeverity::Warning => "warning",
    }
}

fn check_span(span: harn_lexer::Span) -> CheckSpan {
    CheckSpan {
        start: span.start,
        end: span.end,
    }
}

/// Suppress the `parse_source_file` helper's `eprintln!` rendering by
/// going through the file system directly. Kept as an
/// underscore-prefixed alias for symmetry with the human path which
/// uses [`parse_source_file`] for its richer diagnostic printing.
#[allow(dead_code)]
fn _ensure_parse_helper_imported() {
    let _ = parse_source_file;
}
