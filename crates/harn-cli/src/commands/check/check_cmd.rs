use std::path::Path;

use harn_lint::LintSeverity;
use harn_parser::analysis::{AnalysisDatabase, AnalysisError};
use harn_parser::DiagnosticSeverity;
use serde::Serialize;

use crate::package::{CheckConfig, PreflightSeverity};

use super::analysis::{
    analyze_file, render_file_analysis_error_or_exit, span_from_lexer_error,
    span_from_parser_error, FileAnalysisError,
};
use super::outcome::{print_lint_diagnostics, CommandOutcome};
use super::preflight::{collect_preflight_diagnostics, is_preflight_allowed};

pub(crate) const CHECK_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CheckReport {
    pub files: Vec<CheckFileReport>,
    pub summary: CheckSummary,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CheckFileReport {
    pub path: String,
    pub status: CheckFileStatus,
    pub diagnostics: Vec<CheckDiagnostic>,
}

impl CheckFileReport {
    pub(crate) fn outcome(&self) -> CommandOutcome {
        CommandOutcome {
            has_error: matches!(self.status, CheckFileStatus::Error),
            has_warning: matches!(self.status, CheckFileStatus::Warning),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CheckFileStatus {
    Ok,
    Warning,
    Error,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CheckDiagnostic {
    pub source: &'static str,
    pub severity: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span: Option<CheckSpan>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub(crate) struct CheckSpan {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct CheckSummary {
    pub ok: usize,
    pub warnings: usize,
    pub errors: usize,
    pub diagnostics: usize,
}

impl CheckReport {
    pub(crate) fn from_files(files: Vec<CheckFileReport>) -> Self {
        let mut summary = CheckSummary::default();
        for file in &files {
            match file.status {
                CheckFileStatus::Ok => summary.ok += 1,
                CheckFileStatus::Warning => summary.warnings += 1,
                CheckFileStatus::Error => summary.errors += 1,
            }
            summary.diagnostics += file.diagnostics.len();
        }
        Self { files, summary }
    }
}

pub(crate) fn check_file_inner(
    analysis: &mut AnalysisDatabase,
    path: &Path,
    config: &CheckConfig,
    externally_imported_names: &std::collections::HashSet<String>,
    module_graph: &harn_modules::ModuleGraph,
    check_invariants: bool,
) -> CommandOutcome {
    check_file_report_inner(
        analysis,
        path,
        config,
        externally_imported_names,
        module_graph,
        check_invariants,
        true,
    )
    .outcome()
}

pub(crate) fn check_file_report(
    analysis: &mut AnalysisDatabase,
    path: &Path,
    config: &CheckConfig,
    externally_imported_names: &std::collections::HashSet<String>,
    module_graph: &harn_modules::ModuleGraph,
    check_invariants: bool,
) -> CheckFileReport {
    check_file_report_inner(
        analysis,
        path,
        config,
        externally_imported_names,
        module_graph,
        check_invariants,
        false,
    )
}

fn check_file_report_inner(
    analysis: &mut AnalysisDatabase,
    path: &Path,
    config: &CheckConfig,
    externally_imported_names: &std::collections::HashSet<String>,
    module_graph: &harn_modules::ModuleGraph,
    check_invariants: bool,
    emit_text: bool,
) -> CheckFileReport {
    let path_str = path.to_string_lossy().into_owned();
    let output = match analyze_file(analysis, path, config, module_graph) {
        Ok(output) => output,
        Err(error) => {
            if emit_text {
                render_file_analysis_error_or_exit(&path_str, error);
            }
            return file_analysis_error_report(&path_str, error);
        }
    };
    let source = output.source;
    let program = output.program;

    let mut has_error = false;
    let mut has_warning = false;
    let mut diagnostic_count = 0;
    let mut diagnostics = Vec::new();

    for diag in &output.diagnostics {
        if harn_lint::type_diagnostic_lint_disabled(diag, &config.disable_rules) {
            continue;
        }
        match diag.severity {
            DiagnosticSeverity::Error => has_error = true,
            DiagnosticSeverity::Warning => has_warning = true,
        }
        diagnostic_count += 1;
        if emit_text {
            let rendered =
                harn_parser::diagnostic::render_type_diagnostic(&source, &path_str, diag);
            eprint!("{rendered}");
        }
        diagnostics.push(CheckDiagnostic {
            source: "type",
            severity: type_severity_label(diag.severity),
            code: Some(diag.code.to_string()),
            message: diag.message.clone(),
            span: diag.span.map(check_span),
            help: diag.help.clone(),
        });
    }

    let lint_diagnostics = harn_lint::lint_with_module_graph(
        &program,
        &config.disable_rules,
        Some(&source),
        externally_imported_names,
        module_graph,
        path,
        &harn_lint::LintOptions {
            file_path: Some(path),
            ..Default::default()
        },
    );
    diagnostic_count += lint_diagnostics.len();
    if lint_diagnostics
        .iter()
        .any(|d| d.severity == LintSeverity::Warning)
    {
        has_warning = true;
    }
    if emit_text {
        if print_lint_diagnostics(&path_str, &source, &lint_diagnostics) {
            has_error = true;
        }
    } else if lint_diagnostics
        .iter()
        .any(|d| d.severity == LintSeverity::Error)
    {
        has_error = true;
    }
    diagnostics.extend(lint_diagnostics.iter().map(|diag| CheckDiagnostic {
        source: "lint",
        severity: lint_severity_label(diag.severity),
        code: Some(diag.code.to_string()),
        message: diag.message.clone(),
        span: Some(check_span(diag.span)),
        help: diag.suggestion.clone(),
    }));

    let preflight_diagnostics = collect_preflight_diagnostics(path, &source, &program, config);
    let preflight_severity = PreflightSeverity::from_opt(config.preflight_severity.as_deref());
    if preflight_severity != PreflightSeverity::Off {
        let (severity_label, category) = match preflight_severity {
            PreflightSeverity::Warning => ("warning", "preflight"),
            _ => ("error", "preflight"),
        };
        for diag in &preflight_diagnostics {
            if is_preflight_allowed(&diag.tags, &config.preflight_allow) {
                continue;
            }
            match preflight_severity {
                PreflightSeverity::Warning => has_warning = true,
                PreflightSeverity::Error => has_error = true,
                PreflightSeverity::Off => unreachable!(),
            }
            diagnostic_count += 1;
            if emit_text {
                let rendered = harn_parser::diagnostic::render_diagnostic_with_code(
                    &diag.source,
                    &diag.path,
                    &diag.span,
                    severity_label,
                    diag.code,
                    &diag.message,
                    Some(category),
                    diag.help.as_deref(),
                );
                eprint!("{rendered}");
            }
            diagnostics.push(CheckDiagnostic {
                source: category,
                severity: severity_label,
                code: Some(diag.code.to_string()),
                message: diag.message.clone(),
                span: Some(check_span(diag.span)),
                help: diag.help.clone(),
            });
        }
    }

    if check_invariants {
        let report = harn_ir::analyze_program(&program);
        for diag in &report.diagnostics {
            has_error = true;
            diagnostic_count += 1;
            if emit_text {
                let rendered = harn_parser::diagnostic::render_diagnostic(
                    &source,
                    &path_str,
                    &diag.span,
                    "error",
                    &diag.message,
                    Some(&format!("invariant[{}]", diag.invariant)),
                    diag.help.as_deref(),
                );
                eprint!("{rendered}");
            }
            diagnostics.push(CheckDiagnostic {
                source: "invariant",
                severity: "error",
                code: None,
                message: diag.message.clone(),
                span: Some(check_span(diag.span)),
                help: diag.help.clone(),
            });
        }
    }

    if diagnostic_count == 0 && emit_text {
        println!("{path_str}: ok");
    }

    let status = if has_error {
        CheckFileStatus::Error
    } else if has_warning {
        CheckFileStatus::Warning
    } else {
        CheckFileStatus::Ok
    };
    CheckFileReport {
        path: path_str,
        status,
        diagnostics,
    }
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

pub(crate) fn check_span(span: harn_lexer::Span) -> CheckSpan {
    CheckSpan {
        start: span.start,
        end: span.end,
    }
}

fn file_analysis_error_report(path: &str, error: FileAnalysisError) -> CheckFileReport {
    match error {
        FileAnalysisError::Read(error) => CheckFileReport {
            path: path.to_string(),
            status: CheckFileStatus::Error,
            diagnostics: vec![CheckDiagnostic {
                source: "io",
                severity: "error",
                code: None,
                message: format!("Error reading {path}: {error}"),
                span: None,
                help: None,
            }],
        },
        FileAnalysisError::Analysis(error) => analysis_diagnostic_report(path, error),
    }
}

fn analysis_diagnostic_report(path: &str, error: AnalysisError) -> CheckFileReport {
    let diagnostic = check_diagnostic_from_analysis_error(error);
    CheckFileReport {
        path: path.to_string(),
        status: CheckFileStatus::Error,
        diagnostics: vec![diagnostic],
    }
}

pub(crate) fn check_diagnostic_from_analysis_error(error: AnalysisError) -> CheckDiagnostic {
    match error {
        AnalysisError::MissingSource(id) => CheckDiagnostic {
            source: "analysis",
            severity: "error",
            code: None,
            message: format!("missing analysis source {}", id.as_str()),
            span: None,
            help: None,
        },
        AnalysisError::Lex { error, .. } => CheckDiagnostic {
            source: "lexer",
            severity: "error",
            code: Some(harn_parser::diagnostic::lexer_error_code(&error).to_string()),
            message: error.to_string(),
            span: Some(check_span(span_from_lexer_error(&error))),
            help: None,
        },
        AnalysisError::Parse { errors, .. } => {
            // Defensive: if the parser ever returns AnalysisError::Parse with
            // an empty errors vec, fall back to a synthetic diagnostic rather
            // than panicking the `harn check` process.
            match errors.first() {
                Some(error) => CheckDiagnostic {
                    source: "parser",
                    severity: "error",
                    code: Some(harn_parser::diagnostic::parser_error_code(error).to_string()),
                    message: harn_parser::diagnostic::parser_error_message(error),
                    span: Some(check_span(span_from_parser_error(error))),
                    help: harn_parser::diagnostic::parser_error_help(error).map(str::to_string),
                },
                None => CheckDiagnostic {
                    source: "parser",
                    severity: "error",
                    code: None,
                    message: "parser reported failure without a specific diagnostic".to_string(),
                    span: None,
                    help: None,
                },
            }
        }
    }
}
