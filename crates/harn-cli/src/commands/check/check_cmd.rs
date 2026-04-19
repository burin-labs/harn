use std::path::Path;

use harn_lint::LintSeverity;
use harn_parser::{DiagnosticSeverity, TypeChecker};

use crate::package::{CheckConfig, PreflightSeverity};
use crate::parse_source_file;

use super::outcome::{print_lint_diagnostics, CommandOutcome};
use super::preflight::{collect_preflight_diagnostics, is_preflight_allowed};

pub(crate) fn check_file_inner(
    path: &Path,
    config: &CheckConfig,
    externally_imported_names: &std::collections::HashSet<String>,
    module_graph: &harn_modules::ModuleGraph,
) -> CommandOutcome {
    let path_str = path.to_string_lossy().into_owned();
    let (source, program) = parse_source_file(&path_str);

    let mut has_error = false;
    let mut has_warning = false;
    let mut diagnostic_count = 0;

    let mut checker = TypeChecker::with_strict_types(config.strict_types);
    if let Some(imported) = module_graph.imported_names_for_file(path) {
        checker = checker.with_imported_names(imported);
    }
    if let Some(imported) = module_graph.imported_type_declarations_for_file(path) {
        checker = checker.with_imported_type_decls(imported);
    }
    let type_diagnostics = checker.check(&program);
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
                &path_str,
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

    let lint_diagnostics = harn_lint::lint_with_module_graph(
        &program,
        &config.disable_rules,
        Some(&source),
        externally_imported_names,
        module_graph,
        path,
        &harn_lint::LintOptions {
            file_path: Some(path),
            require_file_header: false,
            complexity_threshold: None,
        },
    );
    diagnostic_count += lint_diagnostics.len();
    if lint_diagnostics
        .iter()
        .any(|d| d.severity == LintSeverity::Warning)
    {
        has_warning = true;
    }
    if print_lint_diagnostics(&path_str, &source, &lint_diagnostics) {
        has_error = true;
    }

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
            let rendered = harn_parser::diagnostic::render_diagnostic(
                &diag.source,
                &diag.path,
                &diag.span,
                severity_label,
                &diag.message,
                Some(category),
                diag.help.as_deref(),
            );
            eprint!("{rendered}");
        }
    }

    if diagnostic_count == 0 {
        println!("{path_str}: ok");
    }

    CommandOutcome {
        has_error,
        has_warning,
    }
}
