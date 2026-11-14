use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process;

use harn_fmt::{format_source_opts, line_width_violations, FmtOptions};
use harn_parser::DiagnosticCode as Code;
use serde::Serialize;

use crate::commands::{declares_expected_invalid, is_harn_program_file};
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
    fmt_targets_report_with_workers(targets, mode, opts, None)
}

fn fmt_targets_report_with_workers(
    targets: &[&str],
    mode: FmtMode,
    opts: &FmtOptions,
    workers: Option<usize>,
) -> FmtReport {
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
    let groups = format_file_groups(&files);
    let workers =
        workers.unwrap_or_else(|| super::worker_count_for_env(groups.len(), "HARN_FMT_JOBS"));
    let mut file_reports: Vec<_> = super::run_ordered(
        &groups,
        workers,
        || (),
        |(), group| {
            group
                .iter()
                .map(|(index, file)| (*index, fmt_file_inner(&file.to_string_lossy(), mode, opts)))
                .collect::<Vec<_>>()
        },
        |group| {
            group
                .iter()
                .map(|(index, file)| {
                    (
                        *index,
                        fmt_error(
                            &file.to_string_lossy(),
                            "internal",
                            "internal formatter failure".into(),
                        ),
                    )
                })
                .collect()
        },
    )
    .into_iter()
    .flatten()
    .collect();
    file_reports.sort_by_key(|(index, _)| *index);
    for (_, file_report) in file_reports {
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

#[derive(Eq, Hash, PartialEq)]
enum FormatFileIdentity {
    Physical(u64, u64),
    Path(PathBuf),
}

// Aliases retain their own diagnostics and report order, but must never read
// while another spelling of the same file is being truncated and rewritten.
fn format_file_groups(files: &[PathBuf]) -> Vec<Vec<(usize, &Path)>> {
    let mut identities = HashMap::new();
    let mut groups: Vec<Vec<(usize, &Path)>> = Vec::new();
    for (index, file) in files.iter().enumerate() {
        let identity = physical_file_identity(file).map_or_else(
            || FormatFileIdentity::Path(file.canonicalize().unwrap_or_else(|_| file.clone())),
            |(volume, file)| FormatFileIdentity::Physical(volume, file),
        );
        let group = identities.entry(identity).or_insert_with(|| {
            groups.push(Vec::new());
            groups.len() - 1
        });
        groups[*group].push((index, file));
    }
    groups
}

#[cfg(unix)]
fn physical_file_identity(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let metadata = path.metadata().ok()?;
    Some((metadata.dev(), metadata.ino()))
}

#[cfg(windows)]
fn physical_file_identity(path: &Path) -> Option<(u64, u64)> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };
    let file = std::fs::File::open(path).ok()?;
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // The live File owns the handle and the output has the required OS layout.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
        return None;
    }
    Some((
        u64::from(info.dwVolumeSerialNumber),
        (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
    ))
}

#[cfg(not(any(unix, windows)))]
fn physical_file_identity(_path: &Path) -> Option<(u64, u64)> {
    None
}

/// Format a single file.
fn fmt_file_inner(path: &str, mode: FmtMode, opts: &FmtOptions) -> FmtFileReport {
    if !is_harn_program_file(Path::new(path)) {
        return fmt_error(
            path,
            "unsupported_extension",
            format!("harn fmt only formats .harn files and .harn.txt files; refusing explicit non-Harn target {path}"),
        );
    }

    let source = match std::fs::read_to_string(path) {
        Ok(source) => source,
        Err(error) => return fmt_error(path, "io", format!("Error reading {path}: {error}")),
    };

    let formatted = match format_source_opts(&source, opts) {
        Ok(formatted) => formatted,
        // A fixture that declares its own unparseability is not drift, and the
        // formatter has nothing to say about it. Only the parse failure is
        // excused: a declared-invalid fixture that parses is formatted like any
        // other file, which is why this fork lives here and not in the walk.
        Err(_) if declares_expected_invalid(Path::new(path)) => {
            return FmtFileReport {
                path: path.to_string(),
                status: FmtFileStatus::Skipped,
                diff_lines_changed: 0,
                diagnostics: Vec::new(),
            };
        }
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

/// Render the format report for a human reader.
///
/// Denied per-function rather than per-module: this module's `--json` envelope
/// on stdout is correct, and the text report must not share that stream. See
/// [`super::outcome::print_lint_diagnostics`].
#[deny(clippy::print_stdout)]
fn print_text_report(report: &FmtReport) {
    if report.files.is_empty() {
        cli_diagnostic!("No .harn files found");
        return;
    }
    for file in &report.files {
        match file.status {
            FmtFileStatus::Formatted => cli_diagnostic!("formatted {}", file.path),
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
    // Say what was left alone, in the same words `harn fix` uses. A silent skip
    // is indistinguishable from a fixture the walk never reached.
    let declared_invalid: Vec<&FmtFileReport> = report
        .files
        .iter()
        .filter(|file| matches!(file.status, FmtFileStatus::Skipped))
        .collect();
    if !declared_invalid.is_empty() {
        eprintln!(
            "{}",
            crate::localization::formatted!(
                "left {} declared-invalid fixture(s) untouched (sibling `.error` file):",
                declared_invalid.len()
            )
        );
        for file in declared_invalid {
            eprintln!("  {}", file.path);
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
        eprintln!("{}", crate::localization::formatted!("\n{reformattable} file(s) would be reformatted — run `harn fmt` (without `--check`) to apply formatting.", reformattable = reformattable));
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
    fn parallel_format_serializes_absolute_relative_and_hard_link_aliases() {
        let cwd = std::env::current_dir().unwrap();
        let dir = tempfile::tempdir_in(&cwd).unwrap();
        let file = dir.path().join("source.harn");
        let linked = dir.path().join("linked.harn");
        let unsupported = dir.path().join("unsupported.json");
        let source = "fn value(){return 1}\n";
        std::fs::write(&file, source).unwrap();
        std::fs::hard_link(&file, &linked).unwrap();
        std::fs::hard_link(&file, &unsupported).unwrap();
        let relative = file.strip_prefix(&cwd).unwrap().to_path_buf();
        let files = [relative, file.clone(), linked, unsupported];
        let groups = format_file_groups(&files);
        assert_eq!(groups.len(), 1, "every spelling must share one writer");
        assert_eq!(groups[0].len(), files.len());
        let targets: Vec<_> = files.iter().map(|path| path.to_str().unwrap()).collect();
        let report = fmt_targets_report_with_workers(
            &targets,
            FmtMode::Write,
            &FmtOptions::default(),
            Some(4),
        );
        assert_eq!(report.files.len(), files.len());
        assert_eq!(report.summary.formatted, 1);
        assert_eq!(report.summary.already_formatted, 2);
        assert_eq!(report.summary.errors, 1);
        assert!(report.files.iter().any(|file| file
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "unsupported_extension")));
        assert_eq!(
            std::fs::read_to_string(file).unwrap(),
            format_source_opts(source, &FmtOptions::default()).unwrap()
        );
    }

    #[cfg(unix)]
    #[test]
    fn parallel_format_serializes_symlink_aliases_and_keeps_path_order() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("source.harn");
        std::fs::write(&file, "fn value(){return 1}\n").unwrap();
        let mut files = vec![file];
        for index in 0..24 {
            let alias = dir.path().join(format!("alias{index:02}.harn"));
            std::os::unix::fs::symlink(&files[0], &alias).unwrap();
            files.push(alias);
        }
        files.sort();
        assert_eq!(format_file_groups(&files).len(), 1);
        let targets: Vec<_> = files.iter().map(|path| path.to_str().unwrap()).collect();
        let report = fmt_targets_report_with_workers(
            &targets,
            FmtMode::Write,
            &FmtOptions::default(),
            Some(4),
        );
        assert_eq!(report.summary.formatted, 1);
        assert_eq!(report.summary.already_formatted, 24);
        assert_eq!(report.summary.errors, 0);
        assert_eq!(
            report
                .files
                .iter()
                .map(|file| file.path.as_str())
                .collect::<Vec<_>>(),
            targets
        );
    }

    #[cfg(unix)]
    #[test]
    fn parallel_format_keeps_alias_specific_invalid_fixture_diagnostics() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.harn");
        let alias = dir.path().join("alias.harn");
        std::fs::write(&source, "fn broken(\n").unwrap();
        std::os::unix::fs::symlink(&source, &alias).unwrap();
        std::fs::write(alias.with_extension("error"), "parse error\n").unwrap();
        let report = fmt_targets_report_with_workers(
            &[source.to_str().unwrap(), alias.to_str().unwrap()],
            FmtMode::Write,
            &FmtOptions::default(),
            Some(4),
        );
        assert_eq!(report.files.len(), 2);
        assert_eq!(report.summary.skipped, 1);
        assert_eq!(report.summary.errors, 1);
    }

    #[test]
    fn parallel_format_reports_are_identical_and_reach_every_file() {
        let dir = tempfile::tempdir().unwrap();
        for index in 0..24 {
            std::fs::write(
                dir.path().join(format!("file{index:02}.harn")),
                "fn value(){return 1}\n",
            )
            .unwrap();
        }
        std::fs::write(dir.path().join("broken.harn"), "fn broken(\n").unwrap();
        let targets = [dir.path().to_str().unwrap()];
        let serial = fmt_targets_report_with_workers(
            &targets,
            FmtMode::Check,
            &FmtOptions::default(),
            Some(1),
        );
        let parallel = fmt_targets_report_with_workers(
            &targets,
            FmtMode::Check,
            &FmtOptions::default(),
            Some(4),
        );
        assert_eq!(serial.files.len(), 25);
        assert_eq!(serial.summary.errors, 25);
        assert_eq!(
            serde_json::to_string(&serial).unwrap(),
            serde_json::to_string(&parallel).unwrap()
        );
        let written = fmt_targets_report_with_workers(
            &targets,
            FmtMode::Write,
            &FmtOptions::default(),
            Some(4),
        );
        assert_eq!(written.summary.formatted, 24);
        assert_eq!(written.summary.errors, 1);
        let checked = fmt_targets_report_with_workers(
            &targets,
            FmtMode::Check,
            &FmtOptions::default(),
            Some(4),
        );
        assert_eq!(checked.summary.already_formatted, 24);
        assert_eq!(checked.summary.errors, 1);
    }

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

    /// The unparseable source both fixture tests share, so the only difference
    /// between them is whether a sibling `.error` file declares it.
    const UNPARSEABLE: &str = "fn t() { return\n";

    #[test]
    fn a_declared_invalid_fixture_is_skipped_rather_than_failed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing_close_brace.harn");
        std::fs::write(&path, UNPARSEABLE).unwrap();
        std::fs::write(
            dir.path().join("missing_close_brace.error"),
            "HARN-FMT-001\n",
        )
        .unwrap();

        let report = fmt_targets_report(
            &[dir.path().to_str().unwrap()],
            FmtMode::Write,
            &FmtOptions::default(),
        );

        assert_eq!(report.summary.errors, 0, "declared fixture must not fail");
        assert_eq!(report.summary.skipped, 1);
        let file = report.files.first().expect("file report");
        assert!(matches!(file.status, FmtFileStatus::Skipped));
        assert!(file.diagnostics.is_empty());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            UNPARSEABLE,
            "a skipped fixture must be left byte-identical"
        );
    }

    /// The negative control for the test above. Without this, that test would
    /// also pass if the fix had suppressed every parse failure, which is the
    /// one outcome the declaration must not buy.
    #[test]
    fn an_undeclared_parse_failure_still_fails_the_run() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("corrupt.harn");
        std::fs::write(&path, UNPARSEABLE).unwrap();

        let report = fmt_targets_report(
            &[dir.path().to_str().unwrap()],
            FmtMode::Write,
            &FmtOptions::default(),
        );

        assert_eq!(report.summary.errors, 1);
        assert_eq!(report.summary.skipped, 0);
        let file = report.files.first().expect("file report");
        assert!(matches!(file.status, FmtFileStatus::Error));
        assert_eq!(file.diagnostics[0].code, "format");
    }

    /// The declaration excuses a parse failure, not the file. A fixture that
    /// parses is still the formatter's business — `conformance/errors` carries
    /// runtime-error fixtures that are perfectly well-formed source.
    #[test]
    fn a_declared_invalid_fixture_that_parses_is_still_formatted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("uncaught_throw.harn");
        std::fs::write(&path, "fn t(){return 1}\n").unwrap();
        std::fs::write(dir.path().join("uncaught_throw.error"), "HARN-RT-001\n").unwrap();

        let report = fmt_targets_report(
            &[dir.path().to_str().unwrap()],
            FmtMode::Write,
            &FmtOptions::default(),
        );

        assert_eq!(
            report.summary.skipped, 0,
            "parseable fixture is not skipped"
        );
        assert_eq!(report.summary.errors, 0);
        assert_eq!(report.summary.formatted, 1);
        assert_ne!(
            std::fs::read_to_string(&path).unwrap(),
            "fn t(){return 1}\n"
        );
    }
}
