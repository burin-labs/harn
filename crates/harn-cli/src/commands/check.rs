use std::process;

use harn_fmt::format_source;
use harn_lint::{lint, LintSeverity};
use harn_parser::{DiagnosticSeverity, TypeChecker};

use crate::parse_source_file;

fn print_lint_diagnostics(path: &str, diagnostics: &[harn_lint::LintDiagnostic]) -> bool {
    let mut has_error = false;
    for diag in diagnostics {
        let severity = match diag.severity {
            LintSeverity::Warning => "warning",
            LintSeverity::Error => {
                has_error = true;
                "error"
            }
        };
        println!(
            "{path}:{}:{}: {severity}[{}]: {}",
            diag.span.line, diag.span.column, diag.rule, diag.message
        );
        if let Some(ref suggestion) = diag.suggestion {
            println!("  suggestion: {suggestion}");
        }
    }
    has_error
}

pub(crate) fn check_file(path: &str) {
    let (source, program) = parse_source_file(path);

    let mut has_error = false;
    let mut diagnostic_count = 0;

    // Type checking
    let type_diagnostics = TypeChecker::new().check(&program);
    for diag in &type_diagnostics {
        let severity = match diag.severity {
            DiagnosticSeverity::Error => {
                has_error = true;
                "error"
            }
            DiagnosticSeverity::Warning => "warning",
        };
        diagnostic_count += 1;
        if let Some(span) = &diag.span {
            let rendered = harn_parser::diagnostic::render_diagnostic(
                &source,
                path,
                span,
                severity,
                &diag.message,
                None,
                None,
            );
            eprint!("{rendered}");
        } else {
            eprintln!("{severity}: {}", diag.message);
        }
    }

    // Linting
    let lint_diagnostics = lint(&program);
    diagnostic_count += lint_diagnostics.len();
    if print_lint_diagnostics(path, &lint_diagnostics) {
        has_error = true;
    }

    if diagnostic_count == 0 {
        println!("{path}: ok");
    }

    if has_error {
        process::exit(1);
    }
}

pub(crate) fn lint_file(path: &str) {
    let (_source, program) = parse_source_file(path);

    let diagnostics = lint(&program);

    if diagnostics.is_empty() {
        println!("{path}: no issues found");
        return;
    }

    if print_lint_diagnostics(path, &diagnostics) {
        process::exit(1);
    }
}

pub(crate) fn fmt_file(path: &str, check_mode: bool) {
    let source = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error reading {path}: {e}");
            process::exit(1);
        }
    };

    let formatted = match format_source(&source) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("{path}: {e}");
            process::exit(1);
        }
    };

    if check_mode {
        if source != formatted {
            eprintln!("{path}: would be reformatted");
            process::exit(1);
        }
        println!("{path}: ok");
    } else if source != formatted {
        match std::fs::write(path, &formatted) {
            Ok(()) => println!("formatted {path}"),
            Err(e) => {
                eprintln!("Error writing {path}: {e}");
                process::exit(1);
            }
        }
    } else {
        println!("{path}: already formatted");
    }
}
