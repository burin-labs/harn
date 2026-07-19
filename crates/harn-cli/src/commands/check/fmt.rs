use std::path::Path;
use std::process;

use harn_fmt::{format_source_opts, line_width_violations, FmtOptions};
use harn_parser::DiagnosticCode as Code;
use serde::Serialize;

use crate::json_envelope::{JsonEnvelope, JsonError};

pub(crate) const FMT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize)]
pub(crate) struct FmtReport {
    pub files: Vec<FmtFileReport>,
    pub summary: FmtSummary,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct FmtFileReport {
    pub path: String,
    pub status: FmtFileStatus,
    pub diff_lines_changed: usize,
    pub diagnostics: Vec<FmtDiagnostic>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FmtFileStatus {
    Formatted,
    AlreadyFormatted,
    #[allow(dead_code)]
    Skipped,
    Error,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct FmtDiagnostic {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct FmtSummary {
    pub formatted: usize,
    pub already_formatted: usize,
    pub skipped: usize,
    pub errors: usize,
}

/// Whether `harn fmt` should rewrite files in place or just report drift.
#[derive(Clone, Copy, Debug)]
pub(crate) enum FmtMode {
    /// Rewrite files that aren't already formatted.
    Write,
    /// Only report files that would be reformatted; never write to disk.
    Check,
}

impl FmtMode {
    pub(crate) fn from_check_flag(check: bool) -> Self {
        if check {
            Self::Check
        } else {
            Self::Write
        }
    }

    fn is_check(self) -> bool {
        matches!(self, Self::Check)
    }
}

/// Format one or more files or directories. Accepts multiple targets.
pub(crate) fn fmt_targets(targets: &[&str], mode: FmtMode, opts: &FmtOptions) {
    let report = fmt_targets_report(targets, mode, opts);
    print_text_report(&report);
    if report.summary.errors > 0 {
        process::exit(1);
    }
}

pub(crate) fn fmt_targets_json(
    targets: &[&str],
    mode: FmtMode,
    opts: &FmtOptions,
) -> JsonEnvelope<FmtReport> {
    let report = fmt_targets_report(targets, mode, opts);
    if report.summary.errors > 0 {
        JsonEnvelope {
            schema_version: FMT_SCHEMA_VERSION,
            ok: false,
            data: Some(report),
            error: Some(JsonError {
                code: "fmt_failed".to_string(),
                message: "one or more files failed formatting checks".to_string(),
                details: serde_json::Value::Null,
            }),
            warnings: Vec::new(),
        }
    } else {
        JsonEnvelope::ok(FMT_SCHEMA_VERSION, report)
    }
}

pub(crate) fn fmt_targets_report(targets: &[&str], mode: FmtMode, opts: &FmtOptions) -> FmtReport {
    let mut files = Vec::new();
    for target in targets {
        let path = Path::new(target);
        if path.is_dir() {
            files.extend(super::super::collect_source_targets(&[target], true, false).harn);
        } else {
            files.push(path.to_path_buf());
        }
    }
    files.sort();
    files.dedup();
    if files.is_empty() {
        return FmtReport {
            files: Vec::new(),
            summary: FmtSummary {
                errors: 1,
                ..FmtSummary::default()
            },
        };
    }
    let mut report = FmtReport {
        files: Vec::new(),
        summary: FmtSummary::default(),
    };
    for file in files {
        let path_str = file.to_string_lossy();
        let file_report = fmt_file_inner(&path_str, mode, opts);
        match file_report.status {
            FmtFileStatus::Formatted => report.summary.formatted += 1,
            FmtFileStatus::AlreadyFormatted => report.summary.already_formatted += 1,
            FmtFileStatus::Skipped => report.summary.skipped += 1,
            FmtFileStatus::Error => report.summary.errors += 1,
        }
        report.files.push(file_report);
    }
    report
}

/// Format a single file.
fn fmt_file_inner(path: &str, mode: FmtMode, opts: &FmtOptions) -> FmtFileReport {
    if !is_harn_source_path(Path::new(path)) {
        return fmt_error(
            path,
            "unsupported_extension",
            format!("harn fmt only formats .harn files; refusing explicit non-Harn target {path}"),
        );
    }

    let source = match std::fs::read_to_string(path) {
        Ok(source) => source,
        Err(error) => return fmt_error(path, "io", format!("Error reading {path}: {error}")),
    };

    let formatted = match format_source_opts(&source, opts) {
        Ok(formatted) => formatted,
        Err(error) => return fmt_error(path, "format", format!("{path}: {error}")),
    };

    if let Some(violation) = line_width_violations(&formatted, opts.line_width).first() {
        return fmt_error(
            path,
            "line_width",
            format!(
                "{path}: formatted line {} is {} columns wide (maximum {})",
                violation.line, violation.width, opts.line_width
            ),
        );
    }

    if mode.is_check() {
        if source != formatted {
            return FmtFileReport {
                path: path.to_string(),
                status: FmtFileStatus::Error,
                diff_lines_changed: diff_lines_changed(&source, &formatted),
                diagnostics: vec![FmtDiagnostic {
                    code: Code::FormatterWouldReformat.to_string(),
                    message: "would be reformatted".to_string(),
                }],
            };
        }
    } else if source != formatted {
        if let Err(error) = std::fs::write(path, &formatted) {
            return fmt_error(path, "io", format!("Error writing {path}: {error}"));
        }
        return FmtFileReport {
            path: path.to_string(),
            status: FmtFileStatus::Formatted,
            diff_lines_changed: diff_lines_changed(&source, &formatted),
            diagnostics: Vec::new(),
        };
    }

    FmtFileReport {
        path: path.to_string(),
        status: FmtFileStatus::AlreadyFormatted,
        diff_lines_changed: 0,
        diagnostics: Vec::new(),
    }
}

fn is_harn_source_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension == "harn")
}

fn fmt_error(path: &str, code: &str, message: String) -> FmtFileReport {
    FmtFileReport {
        path: path.to_string(),
        status: FmtFileStatus::Error,
        diff_lines_changed: 0,
        diagnostics: vec![FmtDiagnostic {
            code: code.to_string(),
            message,
        }],
    }
}

fn print_text_report(report: &FmtReport) {
    if report.files.is_empty() {
        eprintln!("No .harn files found");
        return;
    }
    for file in &report.files {
        match file.status {
            FmtFileStatus::Formatted => println!("formatted {}", file.path),
            FmtFileStatus::Error => {
                for diagnostic in &file.diagnostics {
                    if diagnostic.code == Code::FormatterWouldReformat.to_string() {
                        eprintln!(
                            "{}: {}: {}",
                            file.path,
                            Code::FormatterWouldReformat,
                            diagnostic.message
                        );
                    } else {
                        eprintln!("{}", diagnostic.message);
                    }
                }
            }
            FmtFileStatus::AlreadyFormatted | FmtFileStatus::Skipped => {}
        }
    }
    // `--check` drift is always auto-fixable by running the formatter in
    // write mode; point the user at it. In write mode these files get status
    // `Formatted` (not `Error`/`FormatterWouldReformat`), so the hint stays
    // silent — and genuine io/format errors are excluded.
    let reformattable = report
        .files
        .iter()
        .filter(|file| {
            matches!(file.status, FmtFileStatus::Error)
                && file
                    .diagnostics
                    .iter()
                    .any(|d| d.code == Code::FormatterWouldReformat.to_string())
        })
        .count();
    if reformattable > 0 {
        eprintln!(
            "\n{reformattable} file(s) would be reformatted — run `harn fmt` (without `--check`) to apply formatting."
        );
    }
}

fn diff_lines_changed(before: &str, after: &str) -> usize {
    let before_lines: Vec<&str> = before.lines().collect();
    let after_lines: Vec<&str> = after.lines().collect();
    let max_len = before_lines.len().max(after_lines.len());
    (0..max_len)
        .filter(|index| before_lines.get(*index) != after_lines.get(*index))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_non_harn_file_targets_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("language-catalog.json");
        std::fs::write(&path, "[{ id: \"zig\" }]\n").unwrap();

        let report = fmt_targets_report(
            &[path.to_str().unwrap()],
            FmtMode::Write,
            &FmtOptions::default(),
        );

        assert_eq!(report.summary.errors, 1);
        assert_eq!(report.summary.formatted, 0);
        let file = report.files.first().expect("file report");
        assert!(matches!(file.status, FmtFileStatus::Error));
        assert_eq!(file.diagnostics[0].code, "unsupported_extension");
        assert!(
            file.diagnostics[0]
                .message
                .contains("only formats .harn files"),
            "{}",
            file.diagnostics[0].message
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "[{ id: \"zig\" }]\n"
        );
    }

    #[test]
    fn width_overflow_is_reported_without_rewriting_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.harn");
        let source = "fn t() { return an_identifier_that_cannot_fit }\n";
        std::fs::write(&path, source).unwrap();

        let report = fmt_targets_report(
            &[path.to_str().unwrap()],
            FmtMode::Write,
            &FmtOptions {
                line_width: 20,
                ..FmtOptions::default()
            },
        );

        assert_eq!(report.summary.errors, 1);
        let file = report.files.first().expect("file report");
        assert_eq!(file.diagnostics[0].code, "line_width");
        assert!(file.diagnostics[0].message.contains("maximum 20"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
    }
}
