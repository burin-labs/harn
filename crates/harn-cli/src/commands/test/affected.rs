use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;

use crate::test_runner::{self, AffectedTestFiles};

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum AffectedTestMode {
    Selected,
    Full,
}

pub(super) struct ResolvedAffectedTests {
    pub mode: AffectedTestMode,
    pub reason: String,
    pub files: Vec<String>,
}

#[derive(Serialize)]
pub(super) struct AffectedTestPlan<'a> {
    schema_version: u32,
    kind: &'static str,
    base_ref: &'a str,
    mode: AffectedTestMode,
    reason: &'a str,
    requested_targets: &'a [String],
    test_file_count: usize,
    test_files: Vec<String>,
}

impl ResolvedAffectedTests {
    pub fn plan<'a>(&'a self, base: &'a str, paths: &'a [String]) -> AffectedTestPlan<'a> {
        AffectedTestPlan {
            schema_version: 1,
            kind: "harn.test.affected_plan",
            base_ref: base,
            mode: self.mode,
            reason: &self.reason,
            requested_targets: paths,
            test_file_count: self.files.len(),
            test_files: self.files.iter().map(|path| plan_path(path)).collect(),
        }
    }
}

fn plan_path(path: &str) -> String {
    let path = Path::new(path);
    let display = std::env::current_dir()
        .ok()
        .and_then(|current| path.strip_prefix(current).ok())
        .unwrap_or(path);
    display
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/")
}

pub(super) fn resolve(paths: &[String], base: &str) -> ResolvedAffectedTests {
    let targets = paths.iter().map(PathBuf::from).collect::<Vec<_>>();
    let changed = match changed_harn_files(base) {
        Ok(changed) => changed,
        Err(reason) => {
            return resolved(AffectedTestMode::Full, reason, all_test_files(&targets));
        }
    };

    match test_runner::select_affected_test_files(&targets, &changed) {
        AffectedTestFiles::Selected { files } => resolved(
            AffectedTestMode::Selected,
            "resolved changed modules through the transitive importer graph".to_string(),
            files,
        ),
        AffectedTestFiles::Full { files, reason } => {
            resolved(AffectedTestMode::Full, reason, files)
        }
    }
}

fn resolved(mode: AffectedTestMode, reason: String, files: Vec<PathBuf>) -> ResolvedAffectedTests {
    ResolvedAffectedTests {
        mode,
        reason,
        files: files
            .into_iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect(),
    }
}

fn all_test_files(targets: &[PathBuf]) -> Vec<PathBuf> {
    test_runner::discover_test_files_for_targets(targets)
}

fn changed_harn_files(base: &str) -> Result<Vec<PathBuf>, String> {
    let root = git_stdout(&["rev-parse", "--show-toplevel"])?;
    let root = PathBuf::from(root.trim());
    let range = format!("{base}...HEAD");
    let output = Command::new("git")
        .args([
            "diff",
            "--name-status",
            "-z",
            "--find-renames",
            &range,
            "--",
        ])
        .output()
        .map_err(|error| format!("could not inspect Git changes: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "git diff {range} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    let fields = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty())
        .map(|field| String::from_utf8_lossy(field).into_owned())
        .collect::<Vec<_>>();
    let mut changed = Vec::new();
    let mut index = 0;
    while index < fields.len() {
        let status = &fields[index];
        index += 1;
        let Some(kind) = status.chars().next() else {
            return Err("git diff returned an empty change status".to_string());
        };
        if matches!(kind, 'R' | 'C') {
            return Err(format!(
                "{status} change requires the complete suite because module identity changed"
            ));
        }
        let Some(relative) = fields.get(index) else {
            return Err(format!("git diff omitted the path for status {status}"));
        };
        index += 1;
        if kind == 'D' {
            return Err(format!(
                "deleted module {relative} requires the complete suite"
            ));
        }
        if !matches!(kind, 'A' | 'M' | 'T' | 'U' | 'X' | 'B') {
            return Err(format!("unrecognized Git change status {status}"));
        }
        if Path::new(relative)
            .extension()
            .and_then(|value| value.to_str())
            != Some("harn")
        {
            return Err(format!(
                "changed non-Harn input {relative} may be read dynamically"
            ));
        }
        changed.push(root.join(relative));
    }
    changed.sort();
    changed.dedup();
    Ok(changed)
}

fn git_stdout(args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .args(args)
        .output()
        .map_err(|error| format!("could not run git {}: {error}", args.join(" ")))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}
