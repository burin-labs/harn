use std::process;

use harn_fmt::format_source;
use harn_lint::{lint_with_config, LintSeverity};
use harn_parser::{DiagnosticSeverity, TypeChecker};

use crate::package::CheckConfig;
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

pub(crate) fn check_file(path: &str, config: &CheckConfig) {
    let (source, program) = parse_source_file(path);

    let mut has_error = false;
    let mut has_warning = false;
    let mut diagnostic_count = 0;

    // Type checking
    let type_diagnostics = TypeChecker::new().check(&program);
    for diag in &type_diagnostics {
        let severity = match diag.severity {
            DiagnosticSeverity::Error => {
                has_error = true;
                "error"
            }
            DiagnosticSeverity::Warning => {
                has_warning = true;
                "warning"
            }
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
                diag.help.as_deref(),
            );
            eprint!("{rendered}");
        } else {
            eprintln!("{severity}: {}", diag.message);
        }
    }

    // Linting
    let lint_diagnostics = lint_with_config(&program, &config.disable_rules);
    diagnostic_count += lint_diagnostics.len();
    if lint_diagnostics
        .iter()
        .any(|d| d.severity == LintSeverity::Warning)
    {
        has_warning = true;
    }
    if print_lint_diagnostics(path, &lint_diagnostics) {
        has_error = true;
    }

    if diagnostic_count == 0 {
        println!("{path}: ok");
    }

    if has_error || (config.strict && has_warning) {
        process::exit(1);
    }
}

pub(crate) fn lint_file(path: &str, config: &CheckConfig) {
    let (_source, program) = parse_source_file(path);

    let diagnostics = lint_with_config(&program, &config.disable_rules);

    if diagnostics.is_empty() {
        println!("{path}: no issues found");
        return;
    }

    let has_warning = diagnostics
        .iter()
        .any(|d| d.severity == LintSeverity::Warning);
    let has_error = print_lint_diagnostics(path, &diagnostics);

    if has_error || (config.strict && has_warning) {
        process::exit(1);
    }
}

/// Format one or more files or directories. Accepts multiple targets.
pub(crate) fn fmt_targets(targets: &[&str], check_mode: bool) {
    let mut files = Vec::new();
    for target in targets {
        let path = std::path::Path::new(target);
        if path.is_dir() {
            collect_harn_files(path, &mut files);
        } else {
            files.push(path.to_path_buf());
        }
    }
    if files.is_empty() {
        eprintln!("No .harn files found");
        process::exit(1);
    }
    let mut has_error = false;
    for file in &files {
        let path_str = file.to_string_lossy();
        if !fmt_file_inner(&path_str, check_mode) {
            has_error = true;
        }
    }
    if has_error {
        process::exit(1);
    }
}

/// Recursively collect .harn files in a directory.
fn collect_harn_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
        entries.sort_by_key(|e| e.path());
        for entry in entries {
            let path = entry.path();
            if path.is_dir() {
                collect_harn_files(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "harn") {
                out.push(path);
            }
        }
    }
}

/// Format a single file. Returns true on success, false on error.
fn fmt_file_inner(path: &str, check_mode: bool) -> bool {
    let source = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error reading {path}: {e}");
            return false;
        }
    };

    let formatted = match format_source(&source) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("{path}: {e}");
            return false;
        }
    };

    if check_mode {
        if source != formatted {
            eprintln!("{path}: would be reformatted");
            return false;
        }
    } else if source != formatted {
        match std::fs::write(path, &formatted) {
            Ok(()) => println!("formatted {path}"),
            Err(e) => {
                eprintln!("Error writing {path}: {e}");
                return false;
            }
        }
    }
    true
}
