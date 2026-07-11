use std::path::Path;

use harn_lint::LintSeverity;
use harn_parser::analysis::{AnalysisDatabase, AnalysisError};
use harn_parser::DiagnosticSeverity;
use serde::Serialize;

use crate::package::{CheckConfig, PreflightSeverity};

use super::analysis::{
    analyze_file, render_file_analysis_error_to_string, span_from_lexer_error,
    span_from_parser_error, FileAnalysisError,
};
use super::outcome::{render_lint_diagnostics, CommandOutcome};
use super::preflight::{collect_preflight_diagnostics_with_module_graph, is_preflight_allowed};

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
            findings: self.diagnostics.len(),
            ..CommandOutcome::default()
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

/// Rendered per-file text output, kept separate by destination stream so a
/// parallel driver can buffer whole-file output and replay it in input order
/// without interleaving. `stdout` carries the `<path>: ok` line; `stderr`
/// carries rendered diagnostics — matching what the serial CLI always did.
#[derive(Debug, Clone, Default)]
pub(crate) struct CheckTextOutput {
    pub stdout: String,
    pub stderr: String,
}

impl CheckTextOutput {
    pub(crate) fn print(&self) {
        use std::io::Write as _;
        if !self.stderr.is_empty() {
            eprint!("{}", self.stderr);
            let _ = std::io::stderr().flush();
        }
        if !self.stdout.is_empty() {
            print!("{}", self.stdout);
            let _ = std::io::stdout().flush();
        }
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
    let mut text = CheckTextOutput::default();
    let report = check_file_report_inner(
        analysis,
        path,
        config,
        externally_imported_names,
        module_graph,
        check_invariants,
        Some(&mut text),
    );
    text.print();
    report.outcome()
}

#[cfg(test)]
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
        None,
    )
}

pub(crate) fn check_file_report_inner(
    analysis: &mut AnalysisDatabase,
    path: &Path,
    config: &CheckConfig,
    externally_imported_names: &std::collections::HashSet<String>,
    module_graph: &harn_modules::ModuleGraph,
    check_invariants: bool,
    mut text: Option<&mut CheckTextOutput>,
) -> CheckFileReport {
    let path_str = path.to_string_lossy().into_owned();
    let output = match analyze_file(analysis, path, config, module_graph) {
        Ok(output) => output,
        Err(error) => {
            if let Some(text) = text.as_mut() {
                text.stderr
                    .push_str(&render_file_analysis_error_to_string(&path_str, &error));
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

    // Imported-module compile failures. When an `import` resolves to a module
    // that itself fails to lex/parse, that module contributes no symbols — so
    // without this the type checker would flag every imported name as
    // "undefined" at this file's call sites, sending the author to debug the
    // wrong file. Surface the imported module's real error anchored at the
    // `import` statement instead. (`imported_names_for_file` returns `None`
    // for the same reason, which suppresses the misleading call-site errors.)
    for failure in module_graph.import_compile_failures(path) {
        has_error = true;
        diagnostic_count += 1;
        let code = harn_parser::diagnostic_codes::Code::ModuleImportCompileFailed;
        let message = format!(
            "imported module '{}' failed to compile ({}): {}",
            failure.import_raw_path,
            failure.module_path.display(),
            failure.error.message,
        );
        let help = format!(
            "fix the lex/parse error in {} before this import can resolve",
            failure.module_path.display(),
        );
        if let Some(text) = text.as_mut() {
            let rendered = harn_parser::diagnostic::render_diagnostic_with_code(
                &source,
                &path_str,
                &failure.import_span,
                "error",
                code,
                &message,
                None,
                Some(help.as_str()),
            );
            text.stderr.push_str(&rendered);
        }
        diagnostics.push(CheckDiagnostic {
            source: "module",
            severity: "error",
            code: Some(code.to_string()),
            message,
            span: Some(check_span(failure.import_span)),
            help: Some(help),
        });
    }

    for diag in &output.diagnostics {
        if harn_lint::type_diagnostic_lint_disabled(diag, &config.disable_rules) {
            continue;
        }
        match diag.severity {
            DiagnosticSeverity::Error => has_error = true,
            DiagnosticSeverity::Warning => has_warning = true,
        }
        diagnostic_count += 1;
        if let Some(text) = text.as_mut() {
            let rendered =
                harn_parser::diagnostic::render_type_diagnostic(&source, &path_str, diag);
            text.stderr.push_str(&rendered);
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

    // Bytecode compilation pass. `harn check` is a "will this run?" gate, so
    // it must also catch errors the type checker does not model but that stop
    // `harn run` — unsupported nested `match` patterns, `break`/`continue`
    // outside a loop, `try*` outside a function, malformed string
    // interpolation, etc. Mirror `run`'s ordering: only compile once the
    // program is type-clean, so type errors surface first without a spurious
    // compile-error cascade. The compiler takes the same `&program` `run`
    // does (imports are AST nodes), so this introduces no new false positives.
    if !has_error {
        if let Err(compile_err) = harn_vm::Compiler::new().compile(&program) {
            has_error = true;
            diagnostic_count += 1;
            let code = harn_parser::diagnostic_codes::Code::CompilerError;
            let span = harn_lexer::Span::with_offsets(0, 0, compile_err.line as usize, 1);
            if let Some(text) = text.as_mut() {
                let rendered = harn_parser::diagnostic::render_diagnostic_with_code(
                    &source,
                    &path_str,
                    &span,
                    "error",
                    code,
                    &compile_err.message,
                    None,
                    None,
                );
                text.stderr.push_str(&rendered);
            }
            diagnostics.push(CheckDiagnostic {
                source: "compile",
                severity: "error",
                code: Some(code.to_string()),
                message: compile_err.message,
                span: Some(check_span(span)),
                help: None,
            });
        }
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
    if let Some(text) = text.as_mut() {
        let (lint_has_error, _, rendered) =
            render_lint_diagnostics(&path_str, &source, &lint_diagnostics);
        text.stderr.push_str(&rendered);
        if lint_has_error {
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

    let preflight_diagnostics = collect_preflight_diagnostics_with_module_graph(
        path,
        &source,
        &program,
        config,
        module_graph,
    );
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
            if let Some(text) = text.as_mut() {
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
                text.stderr.push_str(&rendered);
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
            if let Some(text) = text.as_mut() {
                let rendered = harn_parser::diagnostic::render_diagnostic(
                    &source,
                    &path_str,
                    &diag.span,
                    "error",
                    &diag.message,
                    Some(&format!("invariant[{}]", diag.invariant)),
                    diag.help.as_deref(),
                );
                text.stderr.push_str(&rendered);
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

    if diagnostic_count == 0 {
        if let Some(text) = text.as_mut() {
            text.stdout.push_str(&format!("{path_str}: ok\n"));
        }
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
