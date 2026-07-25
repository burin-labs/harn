//! Git added-line filtering for `harn lint --changed-from`.
//!
//! This module owns the entire Git boundary. The linter continues to produce
//! ordinary UTF-8 byte spans; this layer validates those spans, maps them to
//! physical source lines, and retains only warning/error diagnostics that
//! intersect lines added by the evaluated commit range.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output};

use serde::Serialize;

use crate::json_envelope::{JsonEnvelope, JsonError};

use super::check_cmd::{CheckDiagnostic, CheckFileStatus};
use super::lint_report::{
    run_lint_json, LintFileReport, LintJsonCommandOutcome, LintJsonOptions, LintReport,
    LINT_SCHEMA_VERSION,
};

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ChangedLintScope {
    pub from: EvaluatedRevision,
    pub to: EvaluatedRevision,
    pub files: Vec<ChangedSourceFile>,
    #[serde(skip)]
    repo_root: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct EvaluatedRevision {
    pub requested: String,
    pub commit: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ChangedSourceFile {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_path: Option<String>,
    pub status: ChangedSourceStatus,
    pub added_lines: Vec<AddedLineRange>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ChangedSourceStatus {
    Added,
    Copied,
    Deleted,
    Modified,
    Renamed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct AddedLineRange {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug)]
pub(crate) struct ChangedLintError {
    code: &'static str,
    message: String,
}

impl ChangedLintError {
    pub(crate) fn code(&self) -> &'static str {
        self.code
    }

    pub(crate) fn message(&self) -> &str {
        &self.message
    }

    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for ChangedLintError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

struct LintTarget {
    relative_path: String,
    absolute_path: PathBuf,
    added_lines: Vec<AddedLineRange>,
}

struct ChangedLintPlan {
    scope: ChangedLintScope,
    targets: Vec<LintTarget>,
}

pub(crate) async fn run_changed_lint_command(args: &crate::cli::PathTargetsArgs) -> bool {
    let Some(changed_from) = args.changed_from.as_deref() else {
        return false;
    };
    let options = LintJsonOptions {
        strict: args.strict,
        require_file_header: args.require_file_header,
        require_public_api_types: args.require_public_api_types,
    };
    let outcome = match run_changed_lint(changed_from, args.changed_to.as_deref(), options).await {
        Ok(outcome) => outcome,
        Err(error) => {
            if args.json {
                let envelope: JsonEnvelope<LintReport> =
                    JsonEnvelope::err(LINT_SCHEMA_VERSION, error.code(), error.message());
                println!("{}", crate::json_envelope::to_string_pretty(&envelope));
            } else {
                eprintln!("error: {error}");
            }
            std::process::exit(1);
        }
    };
    if args.json {
        println!(
            "{}",
            crate::json_envelope::to_string_pretty(&outcome.envelope)
        );
    } else {
        render_changed_lint(&outcome);
    }
    if outcome.exit_code != 0 {
        std::process::exit(outcome.exit_code);
    }
    true
}

pub(crate) async fn run_changed_lint(
    from: &str,
    to: Option<&str>,
    options: LintJsonOptions,
) -> Result<LintJsonCommandOutcome, ChangedLintError> {
    let cwd = std::env::current_dir().map_err(|error| {
        ChangedLintError::new(
            "changed_lint_cwd_failed",
            format!("failed to determine the current directory: {error}"),
        )
    })?;
    let plan = build_plan(&cwd, from, to.unwrap_or("HEAD"))?;
    let paths: Vec<PathBuf> = plan
        .targets
        .iter()
        .map(|target| target.absolute_path.clone())
        .collect();

    let mut report = if paths.is_empty() {
        LintReport::from_files(Vec::new())
    } else {
        run_lint_json(&paths, options)
            .await
            .envelope
            .data
            .ok_or_else(|| {
                ChangedLintError::new(
                    "changed_lint_report_failed",
                    "lint did not produce a structured report",
                )
            })?
    };

    if report.files.len() != plan.targets.len() {
        return Err(ChangedLintError::new(
            "changed_lint_report_failed",
            "lint report file count did not match the evaluated changed-source set",
        ));
    }

    let mut filtered_files = Vec::with_capacity(report.files.len());
    let mut should_fail = false;
    for (file, target) in report.files.into_iter().zip(&plan.targets) {
        let source = std::fs::read_to_string(&target.absolute_path).map_err(|error| {
            ChangedLintError::new(
                "changed_lint_source_failed",
                format!(
                    "failed to read changed source {}: {error}",
                    target.relative_path
                ),
            )
        })?;
        let mut file = filter_file_report(file, &source, &target.added_lines).map_err(|error| {
            ChangedLintError::new(
                "changed_lint_span_failed",
                format!("{}: {error}", target.relative_path),
            )
        })?;
        file.path.clone_from(&target.relative_path);
        let config = crate::package::load_check_config(Some(&target.absolute_path));
        should_fail |= file.outcome().should_fail(config.strict || options.strict);
        filtered_files.push(file);
    }

    report = LintReport::from_files(filtered_files);
    report.changed = Some(plan.scope);
    let envelope = if should_fail {
        JsonEnvelope {
            schema_version: LINT_SCHEMA_VERSION,
            ok: false,
            data: Some(report),
            error: Some(JsonError {
                code: "lint_failed".to_string(),
                message: "one or more changed lines failed `harn lint`".to_string(),
                details: serde_json::Value::Null,
            }),
            warnings: Vec::new(),
        }
    } else {
        JsonEnvelope::ok(LINT_SCHEMA_VERSION, report)
    };

    Ok(LintJsonCommandOutcome {
        envelope,
        exit_code: i32::from(should_fail),
    })
}

fn render_changed_lint(outcome: &LintJsonCommandOutcome) {
    let Some(report) = outcome.envelope.data.as_ref() else {
        return;
    };
    for file in &report.files {
        if file.diagnostics.is_empty() {
            println!("{}: no issues found on added lines", file.path);
            continue;
        }
        let source = report
            .changed
            .as_ref()
            .map(|changed| changed.repo_root.join(&file.path))
            .and_then(|path| std::fs::read_to_string(path).ok());
        for diagnostic in &file.diagnostics {
            let location = diagnostic
                .span
                .and_then(|span| {
                    source
                        .as_deref()
                        .and_then(|source| line_interval(source, span.start, span.end).ok())
                        .map(|interval| format!("{}:{}", file.path, interval.start))
                })
                .unwrap_or_else(|| file.path.clone());
            let code = diagnostic
                .code
                .as_deref()
                .map(|code| format!("[{code}]"))
                .unwrap_or_default();
            eprintln!(
                "{location}: {}{code}: {}",
                diagnostic.severity, diagnostic.message
            );
            if let Some(help) = &diagnostic.help {
                eprintln!("  help: {help}");
            }
        }
    }
}

fn build_plan(
    repo_anchor: &Path,
    from: &str,
    to: &str,
) -> Result<ChangedLintPlan, ChangedLintError> {
    let root_output = run_git(repo_anchor, ["rev-parse", "--show-toplevel"])?;
    let root_text = output_text(&root_output, "locate the Git repository")?;
    let repo_root = std::fs::canonicalize(root_text.trim()).map_err(|error| {
        ChangedLintError::new(
            "changed_lint_repo_failed",
            format!("failed to resolve Git repository root: {error}"),
        )
    })?;
    let from_revision = resolve_revision(&repo_root, from)?;
    let to_revision = resolve_revision(&repo_root, to)?;
    let status_output = run_git(
        &repo_root,
        [
            OsStr::new("diff"),
            OsStr::new("--name-status"),
            OsStr::new("-z"),
            OsStr::new("--find-renames"),
            OsStr::new(&from_revision.commit),
            OsStr::new(&to_revision.commit),
            OsStr::new("--"),
        ],
    )?;
    ensure_success(&status_output, "obtain changed paths")?;
    let changed_paths = parse_name_status(&status_output.stdout)?;

    let mut files = Vec::new();
    let mut targets = Vec::new();
    for changed in changed_paths {
        if !is_harn_source(&changed.path) {
            continue;
        }
        validate_relative_path(&changed.path)?;
        if let Some(previous) = &changed.previous_path {
            validate_relative_path(previous)?;
        }
        let absolute_path = repo_root.join(&changed.path);
        let added_lines = if changed.status == ChangedSourceStatus::Deleted {
            Vec::new()
        } else {
            validate_source_path(&repo_root, &absolute_path, &changed.path)?;
            ensure_matches_revision(&repo_root, &to_revision.commit, &changed.path)?;
            added_ranges(
                &repo_root,
                &from_revision.commit,
                &to_revision.commit,
                &changed.path,
                changed.previous_path.as_deref(),
            )?
        };
        files.push(ChangedSourceFile {
            path: changed.path.clone(),
            previous_path: changed.previous_path,
            status: changed.status,
            added_lines: added_lines.clone(),
        });
        if changed.status != ChangedSourceStatus::Deleted && !added_lines.is_empty() {
            targets.push(LintTarget {
                relative_path: changed.path,
                absolute_path,
                added_lines,
            });
        }
    }

    Ok(ChangedLintPlan {
        scope: ChangedLintScope {
            from: from_revision,
            to: to_revision,
            files,
            repo_root,
        },
        targets,
    })
}

fn resolve_revision(root: &Path, requested: &str) -> Result<EvaluatedRevision, ChangedLintError> {
    if requested.is_empty() || requested.starts_with('-') || requested.contains('\0') {
        return Err(ChangedLintError::new(
            "changed_lint_revision_invalid",
            format!("invalid Git revision {requested:?}"),
        ));
    }
    let revspec = format!("{requested}^{{commit}}");
    let output = run_git(
        root,
        [
            OsStr::new("rev-parse"),
            OsStr::new("--verify"),
            OsStr::new("--end-of-options"),
            OsStr::new(&revspec),
        ],
    )?;
    let commit = output_text(&output, &format!("resolve Git revision {requested:?}"))?
        .trim()
        .to_string();
    if commit.len() != 40 || !commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ChangedLintError::new(
            "changed_lint_revision_invalid",
            format!("Git returned an invalid commit id for {requested:?}"),
        ));
    }
    Ok(EvaluatedRevision {
        requested: requested.to_string(),
        commit,
    })
}

struct ParsedChangedPath {
    path: String,
    previous_path: Option<String>,
    status: ChangedSourceStatus,
}

fn parse_name_status(bytes: &[u8]) -> Result<Vec<ParsedChangedPath>, ChangedLintError> {
    let mut fields = bytes
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty());
    let mut paths = Vec::new();
    while let Some(status_field) = fields.next() {
        let status_text = std::str::from_utf8(status_field).map_err(|_| {
            ChangedLintError::new(
                "changed_lint_diff_invalid",
                "Git emitted a non-UTF-8 status record",
            )
        })?;
        let status_byte = status_text.as_bytes().first().copied().ok_or_else(|| {
            ChangedLintError::new("changed_lint_diff_invalid", "Git emitted an empty status")
        })?;
        let (status, has_source_path) = match status_byte {
            b'A' => (ChangedSourceStatus::Added, false),
            b'C' => (ChangedSourceStatus::Copied, true),
            b'D' => (ChangedSourceStatus::Deleted, false),
            b'M' => (ChangedSourceStatus::Modified, false),
            b'R' => (ChangedSourceStatus::Renamed, true),
            _ => {
                return Err(ChangedLintError::new(
                    "changed_lint_diff_invalid",
                    format!("unsupported Git change status {status_text:?}"),
                ))
            }
        };
        let first = next_utf8_path(&mut fields)?;
        let (previous_path, path) = if has_source_path {
            (Some(first), next_utf8_path(&mut fields)?)
        } else {
            (None, first)
        };
        paths.push(ParsedChangedPath {
            path,
            previous_path,
            status,
        });
    }
    Ok(paths)
}

fn next_utf8_path<'a>(
    fields: &mut impl Iterator<Item = &'a [u8]>,
) -> Result<String, ChangedLintError> {
    let field = fields.next().ok_or_else(|| {
        ChangedLintError::new(
            "changed_lint_diff_invalid",
            "Git status record ended before its path",
        )
    })?;
    std::str::from_utf8(field).map(str::to_string).map_err(|_| {
        ChangedLintError::new(
            "changed_lint_path_invalid",
            "changed source path is not valid UTF-8",
        )
    })
}

fn validate_relative_path(path: &str) -> Result<(), ChangedLintError> {
    let parsed = Path::new(path);
    if parsed.as_os_str().is_empty()
        || parsed
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(ChangedLintError::new(
            "changed_lint_path_invalid",
            format!("Git returned an unsafe changed path {path:?}"),
        ));
    }
    Ok(())
}

fn validate_source_path(
    repo_root: &Path,
    absolute_path: &Path,
    relative_path: &str,
) -> Result<(), ChangedLintError> {
    let canonical = std::fs::canonicalize(absolute_path).map_err(|error| {
        ChangedLintError::new(
            "changed_lint_source_failed",
            format!("failed to resolve changed source {relative_path}: {error}"),
        )
    })?;
    let file_type = std::fs::symlink_metadata(absolute_path)
        .map_err(|error| {
            ChangedLintError::new(
                "changed_lint_source_failed",
                format!("failed to inspect changed source {relative_path}: {error}"),
            )
        })?
        .file_type();
    if file_type.is_symlink() || !canonical.starts_with(repo_root) || !canonical.is_file() {
        return Err(ChangedLintError::new(
            "changed_lint_path_invalid",
            format!(
                "changed source is a symlink, escapes the repository, or is not a file: {relative_path}"
            ),
        ));
    }
    Ok(())
}

fn ensure_matches_revision(
    root: &Path,
    revision: &str,
    path: &str,
) -> Result<(), ChangedLintError> {
    let output = run_git(
        root,
        [
            OsStr::new("diff"),
            OsStr::new("--quiet"),
            OsStr::new("--no-ext-diff"),
            OsStr::new(revision),
            OsStr::new("--"),
            OsStr::new(path),
        ],
    )?;
    match output.status.code() {
        Some(0) => Ok(()),
        Some(1) => Err(ChangedLintError::new(
            "changed_lint_source_mismatch",
            format!("changed source does not match evaluated revision {revision}: {path}"),
        )),
        _ => Err(command_failure(&output, "verify changed source contents")),
    }
}

fn added_ranges(
    root: &Path,
    from: &str,
    to: &str,
    path: &str,
    previous_path: Option<&str>,
) -> Result<Vec<AddedLineRange>, ChangedLintError> {
    let mut args = vec![
        OsString::from("diff"),
        OsString::from("--no-color"),
        OsString::from("--no-ext-diff"),
        OsString::from("--unified=0"),
        OsString::from("--find-renames"),
        OsString::from(from),
        OsString::from(to),
        OsString::from("--"),
    ];
    if let Some(previous_path) = previous_path {
        args.push(OsString::from(previous_path));
    }
    args.push(OsString::from(path));
    let output = run_git(root, args)?;
    ensure_success(&output, &format!("obtain added-line ranges for {path}"))?;
    parse_added_ranges(&output.stdout)
}

fn parse_added_ranges(patch: &[u8]) -> Result<Vec<AddedLineRange>, ChangedLintError> {
    let mut ranges = Vec::new();
    for line in patch.split(|byte| *byte == b'\n') {
        if !line.starts_with(b"@@") {
            continue;
        }
        let header = std::str::from_utf8(line).map_err(|_| {
            ChangedLintError::new(
                "changed_lint_diff_invalid",
                "Git emitted a non-UTF-8 hunk header",
            )
        })?;
        let (_, added) = header
            .strip_prefix("@@ -")
            .and_then(|rest| rest.split_once(" +"))
            .ok_or_else(|| malformed_hunk(header))?;
        let (added, _) = added
            .split_once(" @@")
            .ok_or_else(|| malformed_hunk(header))?;
        let (start, count) = match added.split_once(',') {
            Some((start, count)) => (
                parse_hunk_number(start, header)?,
                parse_hunk_number(count, header)?,
            ),
            None => (parse_hunk_number(added, header)?, 1),
        };
        if count == 0 {
            continue;
        }
        if start == 0 {
            return Err(malformed_hunk(header));
        }
        let end = start
            .checked_add(count - 1)
            .ok_or_else(|| malformed_hunk(header))?;
        ranges.push(AddedLineRange { start, end });
    }
    Ok(ranges)
}

fn parse_hunk_number(value: &str, header: &str) -> Result<usize, ChangedLintError> {
    value.parse().map_err(|_| malformed_hunk(header))
}

fn malformed_hunk(header: &str) -> ChangedLintError {
    ChangedLintError::new(
        "changed_lint_diff_invalid",
        format!("Git emitted a malformed hunk header: {header}"),
    )
}

fn is_harn_source(path: &str) -> bool {
    path.ends_with(".harn") || path.ends_with(".harn.txt")
}

fn filter_file_report(
    mut file: LintFileReport,
    source: &str,
    ranges: &[AddedLineRange],
) -> Result<LintFileReport, String> {
    let old_fixable: HashSet<usize> = file.fixable_diagnostics.iter().copied().collect();
    let mut diagnostics = Vec::new();
    let mut fixable_diagnostics = Vec::new();
    for (index, diagnostic) in file.diagnostics.into_iter().enumerate() {
        let keep = match diagnostic.severity {
            "info" => true,
            "warning" | "error" => {
                let span = diagnostic.span.ok_or_else(|| {
                    format!(
                        "{} diagnostic has no source span",
                        diagnostic.code.as_deref().unwrap_or("lint")
                    )
                })?;
                let interval = line_interval(source, span.start, span.end)?;
                ranges
                    .iter()
                    .any(|range| interval.start <= range.end && interval.end >= range.start)
            }
            severity => return Err(format!("unknown lint severity {severity:?}")),
        };
        if keep {
            if old_fixable.contains(&index) {
                fixable_diagnostics.push(diagnostics.len());
            }
            diagnostics.push(diagnostic);
        }
    }
    file.status = status_from_diagnostics(&diagnostics);
    file.fixable = fixable_diagnostics.len();
    file.fixed = 0;
    file.diagnostics = diagnostics;
    file.fixable_diagnostics = fixable_diagnostics;
    Ok(file)
}

fn status_from_diagnostics(diagnostics: &[CheckDiagnostic]) -> CheckFileStatus {
    if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == "error")
    {
        CheckFileStatus::Error
    } else if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == "warning")
    {
        CheckFileStatus::Warning
    } else {
        CheckFileStatus::Ok
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SourceLineInterval {
    start: usize,
    end: usize,
}

fn line_interval(source: &str, start: usize, end: usize) -> Result<SourceLineInterval, String> {
    if start > end || end > source.len() {
        return Err(format!(
            "diagnostic span {start}..{end} is outside the UTF-8 source byte range 0..{}",
            source.len()
        ));
    }
    if !source.is_char_boundary(start) || !source.is_char_boundary(end) {
        return Err(format!(
            "diagnostic span {start}..{end} is not aligned to UTF-8 character boundaries"
        ));
    }
    let final_position = if start == end { start } else { end - 1 };
    Ok(SourceLineInterval {
        start: line_at_byte(source, start),
        end: line_at_byte(source, final_position),
    })
}

fn line_at_byte(source: &str, offset: usize) -> usize {
    1 + source
        .char_indices()
        .take_while(|(index, _)| *index < offset)
        .filter(|(_, character)| *character == '\n')
        .count()
}

fn run_git<I, S>(cwd: &Path, args: I) -> Result<Output, ChangedLintError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Command::new("git")
        .current_dir(cwd)
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .map_err(|error| {
            ChangedLintError::new(
                "changed_lint_git_failed",
                format!("failed to run Git: {error}"),
            )
        })
}

fn output_text<'a>(output: &'a Output, operation: &str) -> Result<&'a str, ChangedLintError> {
    ensure_success(output, operation)?;
    std::str::from_utf8(&output.stdout).map_err(|_| {
        ChangedLintError::new(
            "changed_lint_git_failed",
            format!("Git emitted non-UTF-8 output while attempting to {operation}"),
        )
    })
}

fn ensure_success(output: &Output, operation: &str) -> Result<(), ChangedLintError> {
    if output.status.success() {
        Ok(())
    } else {
        Err(command_failure(output, operation))
    }
}

fn command_failure(output: &Output, operation: &str) -> ChangedLintError {
    let detail = String::from_utf8_lossy(&output.stderr);
    ChangedLintError::new(
        "changed_lint_git_failed",
        format!(
            "failed to {operation} (Git exit {}): {}",
            output.status.code().unwrap_or(-1),
            detail.trim()
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::super::check_cmd::CheckSpan;
    use super::*;

    fn diagnostic(severity: &'static str, span: Option<(usize, usize)>) -> CheckDiagnostic {
        CheckDiagnostic {
            source: "lint",
            severity,
            code: Some("HARN-LNT-TEST".to_string()),
            message: "fixture".to_string(),
            span: span.map(|(start, end)| CheckSpan { start, end }),
            help: None,
        }
    }

    fn report(diagnostics: Vec<CheckDiagnostic>) -> LintFileReport {
        LintFileReport {
            path: "test.harn".to_string(),
            status: CheckFileStatus::Warning,
            diagnostics,
            fixable: 0,
            fixed: 0,
            fixable_diagnostics: Vec::new(),
        }
    }

    #[test]
    fn parses_multiple_added_hunks_and_ignores_deletion_hunks() {
        let patch =
            b"@@ -1 +1,2 @@\n-old\n+new\n+newer\n@@ -8,2 +9,0 @@\n-old\n@@ -12 +20 @@ label\n";
        assert_eq!(
            parse_added_ranges(patch).unwrap(),
            vec![
                AddedLineRange { start: 1, end: 2 },
                AddedLineRange { start: 20, end: 20 },
            ]
        );
    }

    #[test]
    fn maps_half_open_multiline_and_utf8_spans() {
        let source = "α\nfirst\nsecond\n";
        assert_eq!(
            line_interval(source, 3, 9).unwrap(),
            SourceLineInterval { start: 2, end: 2 }
        );
        assert_eq!(
            line_interval(source, 3, 10).unwrap(),
            SourceLineInterval { start: 2, end: 3 }
        );
        assert_eq!(
            line_interval(source, source.len(), source.len()).unwrap(),
            SourceLineInterval { start: 4, end: 4 }
        );
    }

    #[test]
    fn rejects_invalid_utf8_boundaries_and_out_of_range_spans() {
        let source = "α\n";
        assert!(line_interval(source, 1, 2).is_err());
        assert!(line_interval(source, 0, source.len() + 1).is_err());
        assert!(line_interval(source, 2, 1).is_err());
    }

    #[test]
    fn filters_warning_error_but_retains_information() {
        let source = "legacy\nadded\n";
        let filtered = filter_file_report(
            report(vec![
                diagnostic("warning", Some((0, 6))),
                diagnostic("error", Some((7, 12))),
                diagnostic("info", None),
            ]),
            source,
            &[AddedLineRange { start: 2, end: 2 }],
        )
        .unwrap();
        assert_eq!(filtered.diagnostics.len(), 2);
        assert_eq!(filtered.diagnostics[0].severity, "error");
        assert_eq!(filtered.diagnostics[1].severity, "info");
        assert!(matches!(filtered.status, CheckFileStatus::Error));
    }

    #[test]
    fn matches_added_line_anywhere_in_multiline_span() {
        let source = "first\nsecond\nthird\n";
        let filtered = filter_file_report(
            report(vec![diagnostic("warning", Some((0, 12)))]),
            source,
            &[AddedLineRange { start: 2, end: 2 }],
        )
        .unwrap();
        assert_eq!(filtered.diagnostics.len(), 1);

        let filtered = filter_file_report(
            report(vec![diagnostic("warning", Some((0, 6)))]),
            source,
            &[AddedLineRange { start: 2, end: 2 }],
        )
        .unwrap();
        assert!(filtered.diagnostics.is_empty());
    }

    #[test]
    fn warning_without_span_fails_closed() {
        let error = filter_file_report(
            report(vec![diagnostic("warning", None)]),
            "added\n",
            &[AddedLineRange { start: 1, end: 1 }],
        )
        .unwrap_err();
        assert!(error.contains("no source span"));
    }

    #[test]
    fn parses_spaces_and_rename_status_records() {
        let records = b"M\0dir/a file.harn\0R100\0old.harn\0new name.harn\0D\0gone.harn\0";
        let parsed = parse_name_status(records).unwrap();
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].path, "dir/a file.harn");
        assert_eq!(parsed[1].previous_path.as_deref(), Some("old.harn"));
        assert_eq!(parsed[1].path, "new name.harn");
        assert_eq!(parsed[2].status, ChangedSourceStatus::Deleted);
    }
}
