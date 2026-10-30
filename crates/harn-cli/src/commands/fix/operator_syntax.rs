//! `harn fix --migrate-operator-syntax`: one-shot rewrite of the pre-0.11
//! operator grammar (`a to b` ranges and the old `??` precedence).
//!
//! The rewrite itself lives in `harn_parser::operator_migration`, which proves
//! each file keeps its meaning. This driver walks targets and reports a
//! census, so a run that saw nothing is distinguishable from a clean one.

use std::path::PathBuf;

use harn_parser::operator_migration::migrate_legacy_operators;
use serde::Serialize;

use crate::commands;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct OperatorSyntaxFailure {
    pub(super) file: String,
    pub(super) reason: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct OperatorSyntaxReport {
    pub(super) mode: &'static str,
    pub(super) dry_run: bool,
    pub(super) scanned_file_count: usize,
    pub(super) changed_file_count: usize,
    pub(super) changed_files: Vec<String>,
    pub(super) failed_file_count: usize,
    pub(super) failed_files: Vec<OperatorSyntaxFailure>,
    /// Intentionally invalid fixtures (a sibling `.error` file) that could
    /// not be migrated. They do not fail the run.
    pub(super) excluded_file_count: usize,
    pub(super) excluded_files: Vec<String>,
}

pub(super) fn migrate(targets: &[PathBuf], dry_run: bool) -> Result<OperatorSyntaxReport, String> {
    let target_strings: Vec<String> = targets
        .iter()
        .map(|target| target.to_string_lossy().into_owned())
        .collect();
    let target_refs: Vec<&str> = target_strings.iter().map(String::as_str).collect();
    let files = commands::check::collect_harn_targets(&target_refs);
    if files.is_empty() {
        return Err("no .harn files found under the given target(s)".to_string());
    }
    let mut report = OperatorSyntaxReport {
        mode: "migrate-operator-syntax",
        dry_run,
        scanned_file_count: files.len(),
        changed_file_count: 0,
        changed_files: Vec::new(),
        failed_file_count: 0,
        failed_files: Vec::new(),
        excluded_file_count: 0,
        excluded_files: Vec::new(),
    };
    for file in files {
        let label = file.to_string_lossy().into_owned();
        let source = std::fs::read_to_string(&file)
            .map_err(|error| format!("failed to read {label}: {error}"))?;
        match migrate_legacy_operators(&source) {
            Ok(None) => {}
            Ok(Some(migrated)) => {
                if !dry_run {
                    std::fs::write(&file, migrated)
                        .map_err(|error| format!("failed to write {label}: {error}"))?;
                }
                report.changed_files.push(label);
            }
            Err(_) if commands::declares_expected_invalid(&file) => {
                report.excluded_files.push(label);
            }
            Err(error) => report.failed_files.push(OperatorSyntaxFailure {
                file: label,
                reason: error.to_string(),
            }),
        }
    }
    report.changed_file_count = report.changed_files.len();
    report.failed_file_count = report.failed_files.len();
    report.excluded_file_count = report.excluded_files.len();
    Ok(report)
}

pub(super) fn print_report(report: &OperatorSyntaxReport) {
    let verb = if report.dry_run {
        "would change"
    } else {
        "changed"
    };
    println!(
        "{}",
        crate::localization::formatted!(
            "operator syntax migration: scanned={} {verb}={} failed={} excluded={}",
            report.scanned_file_count,
            report.changed_file_count,
            report.failed_file_count,
            report.excluded_file_count,
            verb = verb
        )
    );
    for file in &report.changed_files {
        println!("  {verb} {file}");
    }
    for failure in &report.failed_files {
        println!(
            "{}",
            crate::localization::formatted!("  failed {}: {}", failure.file, failure.reason)
        );
    }
}
