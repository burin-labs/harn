//! Source-bound matrix coverage, independent of worker capacity and pass totals.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use harn_test_runner::{ShardMatrixPlan, TestSourceIdentity};
use serde::{Deserialize, Serialize};

fn git(path: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let cwd = if path.is_dir() { path } else { path.parent()? };
    let mut command = Command::new("git");
    command
        .args([
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.untrackedCache=false",
        ])
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("GIT_OPTIONAL_LOCKS", "0");
    // Honor Git's normal user/platform configuration, including file modes
    // and line endings, without inheriting repository redirections or keys.
    for name in [
        "PATH",
        "HOME",
        "USERPROFILE",
        "HOMEDRIVE",
        "HOMEPATH",
        "SYSTEMROOT",
        "XDG_CONFIG_HOME",
        "TMP",
        "TEMP",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    let output = command.output().ok()?;
    output.status.success().then_some(output.stdout)
}

fn git_text(path: &Path, args: &[&str]) -> Option<String> {
    String::from_utf8(git(path, args)?)
        .ok()
        .map(|text| text.trim().to_string())
}

pub(crate) fn clean_source(path: &Path) -> Option<TestSourceIdentity> {
    if !git(
        path,
        &["status", "--porcelain=v1", "--untracked-files=normal"],
    )?
    .is_empty()
    {
        return None;
    }
    let commit = git_text(path, &["rev-parse", "--verify", "HEAD"])?;
    // Git's tree-dereference syntax, not Rust interpolation.
    #[allow(clippy::literal_string_with_formatting_args)]
    let tree = git_text(path, &["rev-parse", "--verify", "HEAD^{tree}"])?;
    (git_id(&commit) && git_id(&tree)).then_some(TestSourceIdentity { commit, tree })
}

pub(crate) fn tracked_source(files: &[PathBuf]) -> Option<TestSourceIdentity> {
    let first = files.first()?;
    let root = PathBuf::from(git_text(first, &["rev-parse", "--show-toplevel"])?)
        .canonicalize()
        .ok()?;
    let tracked = git(&root, &["ls-files", "-z"])?;
    let tracked = tracked.split(|byte| *byte == 0).collect::<BTreeSet<_>>();
    for file in files {
        let relative = file.strip_prefix(&root).ok()?.to_str()?.replace('\\', "/");
        if !tracked.contains(relative.as_bytes()) {
            return None;
        }
    }
    clean_source(&root)
}

fn git_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[derive(Deserialize)]
struct Report {
    #[serde(rename = "schemaVersion")]
    schema_version: u32,
    suite: String,
    summary: Counts,
    cases: Vec<ObservedCase>,
    shard_plan: ReportPlan,
}

#[derive(Deserialize)]
struct Counts {
    total: usize,
    passed: usize,
    failed: usize,
    timed_out: usize,
    skipped: usize,
}

#[derive(Deserialize)]
struct ObservedCase {
    file: String,
    name: String,
    outcome: String,
}

#[derive(Deserialize)]
struct ReportPlan {
    index: usize,
    total: usize,
    matrix: ShardMatrixPlan,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub(crate) struct CaseIdentity {
    file: String,
    name: String,
}

impl CaseIdentity {
    fn new(file: &str, name: &str) -> Self {
        Self {
            file: file.into(),
            name: name.into(),
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct CoverageVerdict {
    kind: &'static str,
    schema_version: u32,
    pub(crate) status: &'static str,
    expected_source: String,
    expected_shards: usize,
    report_count: usize,
    planned_cases: usize,
    observed_cases: usize,
    unique_cases: usize,
    pending_count: usize,
    invalid_count: usize,
    failed_case_count: usize,
    plan_identity: Option<String>,
    missing_shards: Vec<usize>,
    missing_cases: Vec<CaseIdentity>,
    duplicate_cases: Vec<CaseIdentity>,
    unexpected_cases: Vec<CaseIdentity>,
    failed_cases: Vec<CaseIdentity>,
    errors: Vec<String>,
}

pub(crate) const SCHEMA_VERSION: u32 = 1;

/// Verify a complete matrix against independently supplied source and size.
/// Reports retain the pre-selection inventory so a green omitted partition is
/// distinguishable from a measured zero. This does not authenticate a CI author.
pub(crate) fn verify(source: &str, total: usize, paths: &[String]) -> CoverageVerdict {
    let mut errors = Vec::new();
    if !git_id(source) || !(1..=1024).contains(&total) {
        errors.push(
            "expected source must be a full Git identity and shard count must be 1..1024".into(),
        );
    }
    let mut matrix: Option<ShardMatrixPlan> = None;
    let mut indices = BTreeSet::new();
    let mut seen = BTreeMap::<CaseIdentity, usize>::new();
    let mut failed = BTreeSet::new();
    let mut bytes_read = 0usize;
    for path in paths {
        let report = match read_report(path, &mut bytes_read) {
            Ok(report) => report,
            Err(error) => {
                errors.push(error);
                continue;
            }
        };
        let plan = &report.shard_plan;
        if report.schema_version != crate::test_report::USER_TEST_REPORT_SCHEMA_VERSION
            || report.suite != "user"
            || plan.total != total
            || plan.index == 0
            || plan.index > total
            || !indices.insert(plan.index)
        {
            errors.push(format!(
                "{path}: wrong report schema, suite, or duplicate/foreign shard"
            ));
        }
        if plan.matrix.schema_version != 1
            || !plan.matrix.source_confirmed
            || plan
                .matrix
                .source
                .as_ref()
                .is_none_or(|identity| identity.commit != source || !git_id(&identity.tree))
        {
            errors.push(format!("{path}: no confirmed clean expected source"));
        }
        if let Some(expected) = &matrix {
            if expected != &plan.matrix {
                errors.push(format!("{path}: source or complete partition plan differs"));
            }
        } else {
            matrix = Some(plan.matrix.clone());
        }
        let expected = plan
            .matrix
            .cases
            .iter()
            .map(|case| (CaseIdentity::new(&case.file, &case.name), case.shard))
            .collect::<BTreeMap<_, _>>();
        let mut actual_counts = [0usize; 4];
        for case in &report.cases {
            let identity = CaseIdentity::new(&case.file, &case.name);
            *seen.entry(identity.clone()).or_default() += 1;
            if expected.get(&identity) != Some(&plan.index) {
                errors.push(format!(
                    "{path}: case is outside its planned shard: {}::{}",
                    case.file, case.name
                ));
            }
            match case.outcome.as_str() {
                "passed" => actual_counts[0] += 1,
                "failed" => actual_counts[1] += 1,
                "timed_out" => actual_counts[2] += 1,
                "skipped" => actual_counts[3] += 1,
                _ => errors.push(format!(
                    "{path}: unknown outcome for {}::{}",
                    case.file, case.name
                )),
            }
            if case.outcome != "passed" {
                failed.insert(identity);
            }
        }
        if report.summary.total != report.cases.len()
            || actual_counts
                != [
                    report.summary.passed,
                    report.summary.failed,
                    report.summary.timed_out,
                    report.summary.skipped,
                ]
        {
            errors.push(format!("{path}: summary does not match observed cases"));
        }
    }
    let mut expected = BTreeSet::new();
    if let Some(plan) = &matrix {
        for case in &plan.cases {
            let path = Path::new(&case.file);
            if case.file.is_empty()
                || path.is_absolute()
                || path
                    .components()
                    .any(|part| !matches!(part, std::path::Component::Normal(_)))
                || case.name.is_empty()
                || case.shard == 0
                || case.shard > total
                || case.declared_weight == 0
                || case.estimated_ms == 0
                || !expected.insert(CaseIdentity::new(&case.file, &case.name))
            {
                errors.push(format!(
                    "invalid or duplicate planned case: {}::{}",
                    case.file, case.name
                ));
            }
        }
    }
    if expected.is_empty() {
        errors.push("no nonempty complete inventory was measured".into());
    }
    let missing_shards = (1..=total.min(1024))
        .filter(|index| !indices.contains(index))
        .collect::<Vec<_>>();
    let missing_cases = expected
        .iter()
        .filter(|case| !seen.contains_key(*case))
        .cloned()
        .collect::<Vec<_>>();
    let duplicate_cases = seen
        .iter()
        .filter(|(_, count)| **count > 1)
        .map(|(case, _)| case.clone())
        .collect::<Vec<_>>();
    let unexpected_cases = seen
        .keys()
        .filter(|case| !expected.contains(*case))
        .cloned()
        .collect::<Vec<_>>();
    let status = if errors.is_empty()
        && missing_shards.is_empty()
        && missing_cases.is_empty()
        && duplicate_cases.is_empty()
        && unexpected_cases.is_empty()
        && failed.is_empty()
    {
        "complete"
    } else {
        "incomplete"
    };
    CoverageVerdict {
        kind: "harn.test.shard_coverage",
        schema_version: SCHEMA_VERSION,
        status,
        expected_source: source.into(),
        expected_shards: total,
        report_count: paths.len(),
        planned_cases: expected.len(),
        observed_cases: seen.values().sum(),
        unique_cases: seen.len(),
        pending_count: missing_shards.len() + missing_cases.len(),
        invalid_count: errors.len() + duplicate_cases.len() + unexpected_cases.len(),
        failed_case_count: failed.len(),
        plan_identity: matrix.as_ref().map(|plan| {
            format!(
                "blake3:{}",
                blake3::hash(&serde_json::to_vec(plan).expect("serializable plan"))
            )
        }),
        missing_shards,
        missing_cases,
        duplicate_cases,
        unexpected_cases,
        failed_cases: failed.into_iter().collect(),
        errors,
    }
}

fn read_report(path: &str, bytes_read: &mut usize) -> Result<Report, String> {
    const MAX_BYTES: usize = 128 * 1024 * 1024;
    let file = std::fs::File::open(path).map_err(|error| format!("{path}: {error}"))?;
    use std::io::Read;
    let mut bytes = Vec::new();
    file.take((MAX_BYTES.saturating_sub(*bytes_read) + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("{path}: {error}"))?;
    *bytes_read = bytes_read.saturating_add(bytes.len());
    if *bytes_read > MAX_BYTES {
        return Err("matrix reports exceed the 128 MiB input limit".into());
    }
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("{path}: incomplete or invalid report: {error}"))
}
