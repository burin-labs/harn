//! Run records, replay fixtures, eval reports, and diff utilities.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{
    default_run_dir, new_id, now_rfc3339, parse_json_payload, parse_json_value, ArtifactRecord,
    CapabilityPolicy,
};
use crate::llm::vm_value_to_json;
use crate::value::{VmError, VmValue};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct LlmUsageRecord {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_duration_ms: i64,
    pub call_count: i64,
    pub total_cost: f64,
    pub models: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct RunStageRecord {
    pub id: String,
    pub node_id: String,
    pub kind: String,
    pub status: String,
    pub outcome: String,
    pub branch: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub visible_text: Option<String>,
    pub private_reasoning: Option<String>,
    pub transcript: Option<serde_json::Value>,
    pub verification: Option<serde_json::Value>,
    pub usage: Option<LlmUsageRecord>,
    pub artifacts: Vec<ArtifactRecord>,
    pub consumed_artifact_ids: Vec<String>,
    pub produced_artifact_ids: Vec<String>,
    pub attempts: Vec<RunStageAttemptRecord>,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct RunStageAttemptRecord {
    pub attempt: usize,
    pub status: String,
    pub outcome: String,
    pub branch: Option<String>,
    pub error: Option<String>,
    pub verification: Option<serde_json::Value>,
    pub started_at: String,
    pub finished_at: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RunTransitionRecord {
    pub id: String,
    pub from_stage_id: Option<String>,
    pub from_node_id: Option<String>,
    pub to_node_id: String,
    pub branch: Option<String>,
    pub timestamp: String,
    pub consumed_artifact_ids: Vec<String>,
    pub produced_artifact_ids: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RunCheckpointRecord {
    pub id: String,
    pub ready_nodes: Vec<String>,
    pub completed_nodes: Vec<String>,
    pub last_stage_id: Option<String>,
    pub persisted_at: String,
    pub reason: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ReplayFixture {
    #[serde(rename = "_type")]
    pub type_name: String,
    pub id: String,
    pub source_run_id: String,
    pub workflow_id: String,
    pub workflow_name: Option<String>,
    pub created_at: String,
    pub expected_status: String,
    pub stage_assertions: Vec<ReplayStageAssertion>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ReplayStageAssertion {
    pub node_id: String,
    pub expected_status: String,
    pub expected_outcome: String,
    pub expected_branch: Option<String>,
    pub required_artifact_kinds: Vec<String>,
    pub visible_text_contains: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ReplayEvalReport {
    pub pass: bool,
    pub failures: Vec<String>,
    pub stage_count: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ReplayEvalCaseReport {
    pub run_id: String,
    pub workflow_id: String,
    pub label: Option<String>,
    pub pass: bool,
    pub failures: Vec<String>,
    pub stage_count: usize,
    pub source_path: Option<String>,
    pub comparison: Option<RunDiffReport>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ReplayEvalSuiteReport {
    pub pass: bool,
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub cases: Vec<ReplayEvalCaseReport>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RunStageDiffRecord {
    pub node_id: String,
    pub change: String,
    pub details: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ToolCallDiffRecord {
    pub tool_name: String,
    pub args_hash: String,
    pub result_changed: bool,
    pub left_result: Option<String>,
    pub right_result: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RunDiffReport {
    pub left_run_id: String,
    pub right_run_id: String,
    pub identical: bool,
    pub status_changed: bool,
    pub left_status: String,
    pub right_status: String,
    pub stage_diffs: Vec<RunStageDiffRecord>,
    pub tool_diffs: Vec<ToolCallDiffRecord>,
    pub transition_count_delta: isize,
    pub artifact_count_delta: isize,
    pub checkpoint_count_delta: isize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct EvalSuiteManifest {
    #[serde(rename = "_type")]
    pub type_name: String,
    pub id: String,
    pub name: Option<String>,
    pub base_dir: Option<String>,
    pub cases: Vec<EvalSuiteCase>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct EvalSuiteCase {
    pub label: Option<String>,
    pub run_path: String,
    pub fixture_path: Option<String>,
    pub compare_to: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct RunRecord {
    #[serde(rename = "_type")]
    pub type_name: String,
    pub id: String,
    pub workflow_id: String,
    pub workflow_name: Option<String>,
    pub task: String,
    pub status: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub parent_run_id: Option<String>,
    pub root_run_id: Option<String>,
    pub stages: Vec<RunStageRecord>,
    pub transitions: Vec<RunTransitionRecord>,
    pub checkpoints: Vec<RunCheckpointRecord>,
    pub pending_nodes: Vec<String>,
    pub completed_nodes: Vec<String>,
    pub child_runs: Vec<RunChildRecord>,
    pub artifacts: Vec<ArtifactRecord>,
    pub policy: CapabilityPolicy,
    pub execution: Option<RunExecutionRecord>,
    pub transcript: Option<serde_json::Value>,
    pub usage: Option<LlmUsageRecord>,
    pub replay_fixture: Option<ReplayFixture>,
    pub trace_spans: Vec<RunTraceSpanRecord>,
    pub tool_recordings: Vec<ToolCallRecord>,
    pub metadata: BTreeMap<String, serde_json::Value>,
    pub persisted_path: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ToolCallRecord {
    pub tool_name: String,
    pub tool_use_id: String,
    pub args_hash: String,
    pub result: String,
    pub is_rejected: bool,
    pub duration_ms: u64,
    pub iteration: usize,
    pub timestamp: String,
}

/// Hash a tool invocation for fixture lookup (name + canonical args JSON).
pub fn tool_fixture_hash(tool_name: &str, args: &serde_json::Value) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    tool_name.hash(&mut hasher);
    let args_str = serde_json::to_string(args).unwrap_or_default();
    args_str.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RunTraceSpanRecord {
    pub span_id: u64,
    pub parent_id: Option<u64>,
    pub kind: String,
    pub name: String,
    pub start_ms: u64,
    pub duration_ms: u64,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RunChildRecord {
    pub worker_id: String,
    pub worker_name: String,
    pub parent_stage_id: Option<String>,
    pub session_id: Option<String>,
    pub parent_session_id: Option<String>,
    pub mutation_scope: Option<String>,
    pub approval_mode: Option<String>,
    pub task: String,
    pub status: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub run_id: Option<String>,
    pub run_path: Option<String>,
    pub snapshot_path: Option<String>,
    pub execution: Option<RunExecutionRecord>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RunExecutionRecord {
    pub cwd: Option<String>,
    pub source_dir: Option<String>,
    pub env: BTreeMap<String, String>,
    pub adapter: Option<String>,
    pub repo_path: Option<String>,
    pub worktree_path: Option<String>,
    pub branch: Option<String>,
    pub base_ref: Option<String>,
    pub cleanup: Option<String>,
}

pub fn normalize_run_record(value: &VmValue) -> Result<RunRecord, VmError> {
    let mut run: RunRecord = parse_json_payload(vm_value_to_json(value), "run_record")?;
    if run.type_name.is_empty() {
        run.type_name = "run_record".to_string();
    }
    if run.id.is_empty() {
        run.id = new_id("run");
    }
    if run.started_at.is_empty() {
        run.started_at = now_rfc3339();
    }
    if run.status.is_empty() {
        run.status = "running".to_string();
    }
    if run.root_run_id.is_none() {
        run.root_run_id = Some(run.id.clone());
    }
    if run.replay_fixture.is_none() {
        run.replay_fixture = Some(replay_fixture_from_run(&run));
    }
    Ok(run)
}

pub fn normalize_eval_suite_manifest(value: &VmValue) -> Result<EvalSuiteManifest, VmError> {
    let mut manifest: EvalSuiteManifest = parse_json_value(value)?;
    if manifest.type_name.is_empty() {
        manifest.type_name = "eval_suite_manifest".to_string();
    }
    if manifest.id.is_empty() {
        manifest.id = new_id("eval_suite");
    }
    Ok(manifest)
}

fn load_replay_fixture(path: &Path) -> Result<ReplayFixture, VmError> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| VmError::Runtime(format!("failed to read replay fixture: {e}")))?;
    serde_json::from_str(&content)
        .map_err(|e| VmError::Runtime(format!("failed to parse replay fixture: {e}")))
}

fn resolve_manifest_path(base_dir: Option<&Path>, path: &str) -> PathBuf {
    let path_buf = PathBuf::from(path);
    if path_buf.is_absolute() {
        path_buf
    } else if let Some(base_dir) = base_dir {
        base_dir.join(path_buf)
    } else {
        path_buf
    }
}

pub fn evaluate_run_suite_manifest(
    manifest: &EvalSuiteManifest,
) -> Result<ReplayEvalSuiteReport, VmError> {
    let base_dir = manifest.base_dir.as_deref().map(Path::new);
    let mut reports = Vec::new();
    for case in &manifest.cases {
        let run_path = resolve_manifest_path(base_dir, &case.run_path);
        let run = load_run_record(&run_path)?;
        let fixture = match &case.fixture_path {
            Some(path) => load_replay_fixture(&resolve_manifest_path(base_dir, path))?,
            None => run
                .replay_fixture
                .clone()
                .unwrap_or_else(|| replay_fixture_from_run(&run)),
        };
        let eval = evaluate_run_against_fixture(&run, &fixture);
        let mut pass = eval.pass;
        let mut failures = eval.failures;
        let comparison = match &case.compare_to {
            Some(path) => {
                let baseline_path = resolve_manifest_path(base_dir, path);
                let baseline = load_run_record(&baseline_path)?;
                let diff = diff_run_records(&baseline, &run);
                if !diff.identical {
                    pass = false;
                    failures.push(format!(
                        "run differs from baseline {} with {} stage changes",
                        baseline_path.display(),
                        diff.stage_diffs.len()
                    ));
                }
                Some(diff)
            }
            None => None,
        };
        reports.push(ReplayEvalCaseReport {
            run_id: run.id.clone(),
            workflow_id: run.workflow_id.clone(),
            label: case.label.clone(),
            pass,
            failures,
            stage_count: eval.stage_count,
            source_path: Some(run_path.display().to_string()),
            comparison,
        });
    }
    let total = reports.len();
    let passed = reports.iter().filter(|report| report.pass).count();
    let failed = total.saturating_sub(passed);
    Ok(ReplayEvalSuiteReport {
        pass: failed == 0,
        total,
        passed,
        failed,
        cases: reports,
    })
}

/// Edit operation in a diff sequence.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum DiffOp {
    Equal,
    Delete,
    Insert,
}

/// Compute the shortest edit script using Myers' O(nd) algorithm.
/// Returns a sequence of (DiffOp, line_index_in_before_or_after).
/// Time: O(nd) where d = edit distance. Space: O(d * n).
pub(crate) fn myers_diff(a: &[&str], b: &[&str]) -> Vec<(DiffOp, usize)> {
    let n = a.len() as isize;
    let m = b.len() as isize;
    if n == 0 && m == 0 {
        return Vec::new();
    }
    if n == 0 {
        return (0..m as usize).map(|j| (DiffOp::Insert, j)).collect();
    }
    if m == 0 {
        return (0..n as usize).map(|i| (DiffOp::Delete, i)).collect();
    }

    let max_d = (n + m) as usize;
    let offset = max_d as isize;
    let v_size = 2 * max_d + 1;
    let mut v = vec![0isize; v_size];
    // trace[d] stores v snapshot BEFORE step d was applied.
    let mut trace: Vec<Vec<isize>> = Vec::new();

    'outer: for d in 0..=max_d as isize {
        trace.push(v.clone());
        let mut new_v = v.clone();
        for k in (-d..=d).step_by(2) {
            let ki = (k + offset) as usize;
            let mut x = if k == -d || (k != d && v[ki - 1] < v[ki + 1]) {
                v[ki + 1] // insert (move down)
            } else {
                v[ki - 1] + 1 // delete (move right)
            };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            new_v[ki] = x;
            if x >= n && y >= m {
                let _ = new_v;
                break 'outer;
            }
        }
        v = new_v;
    }

    // Backtrack from (n, m) to (0, 0).
    let mut ops: Vec<(DiffOp, usize)> = Vec::new();
    let mut x = n;
    let mut y = m;
    for d in (1..trace.len() as isize).rev() {
        let k = x - y;
        let v_prev = &trace[d as usize];
        let prev_k = if k == -d
            || (k != d && v_prev[(k - 1 + offset) as usize] < v_prev[(k + 1 + offset) as usize])
        {
            k + 1 // came from insert
        } else {
            k - 1 // came from delete
        };
        let prev_x = v_prev[(prev_k + offset) as usize];
        let prev_y = prev_x - prev_k;

        // Diagonal (equal) moves
        while x > prev_x && y > prev_y {
            x -= 1;
            y -= 1;
            ops.push((DiffOp::Equal, x as usize));
        }
        // Edit move
        if prev_k < k {
            x -= 1;
            ops.push((DiffOp::Delete, x as usize));
        } else {
            y -= 1;
            ops.push((DiffOp::Insert, y as usize));
        }
    }
    // Initial diagonal at d=0
    while x > 0 && y > 0 {
        x -= 1;
        y -= 1;
        ops.push((DiffOp::Equal, x as usize));
    }
    ops.reverse();
    ops
}

pub fn render_unified_diff(path: Option<&str>, before: &str, after: &str) -> String {
    let before_lines: Vec<&str> = before.lines().collect();
    let after_lines: Vec<&str> = after.lines().collect();
    let ops = myers_diff(&before_lines, &after_lines);

    let mut diff = String::new();
    let file = path.unwrap_or("artifact");
    diff.push_str(&format!("--- a/{file}\n+++ b/{file}\n"));
    for &(op, idx) in &ops {
        match op {
            DiffOp::Equal => diff.push_str(&format!(" {}\n", before_lines[idx])),
            DiffOp::Delete => diff.push_str(&format!("-{}\n", before_lines[idx])),
            DiffOp::Insert => diff.push_str(&format!("+{}\n", after_lines[idx])),
        }
    }
    diff
}

pub fn save_run_record(run: &RunRecord, path: Option<&str>) -> Result<String, VmError> {
    let path = path
        .map(PathBuf::from)
        .unwrap_or_else(|| default_run_dir().join(format!("{}.json", run.id)));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| VmError::Runtime(format!("failed to create run directory: {e}")))?;
    }
    let json = serde_json::to_string_pretty(run)
        .map_err(|e| VmError::Runtime(format!("failed to encode run record: {e}")))?;
    // Atomic write: write to .tmp then rename to prevent corruption on kill.
    let tmp_path = path.with_extension("json.tmp");
    std::fs::write(&tmp_path, &json)
        .map_err(|e| VmError::Runtime(format!("failed to persist run record: {e}")))?;
    std::fs::rename(&tmp_path, &path).map_err(|e| {
        // Fallback: if rename fails (cross-device), write directly.
        let _ = std::fs::write(&path, &json);
        VmError::Runtime(format!("failed to finalize run record: {e}"))
    })?;
    Ok(path.to_string_lossy().to_string())
}

pub fn load_run_record(path: &Path) -> Result<RunRecord, VmError> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| VmError::Runtime(format!("failed to read run record: {e}")))?;
    serde_json::from_str(&content)
        .map_err(|e| VmError::Runtime(format!("failed to parse run record: {e}")))
}

pub fn replay_fixture_from_run(run: &RunRecord) -> ReplayFixture {
    ReplayFixture {
        type_name: "replay_fixture".to_string(),
        id: new_id("fixture"),
        source_run_id: run.id.clone(),
        workflow_id: run.workflow_id.clone(),
        workflow_name: run.workflow_name.clone(),
        created_at: now_rfc3339(),
        expected_status: run.status.clone(),
        stage_assertions: run
            .stages
            .iter()
            .map(|stage| ReplayStageAssertion {
                node_id: stage.node_id.clone(),
                expected_status: stage.status.clone(),
                expected_outcome: stage.outcome.clone(),
                expected_branch: stage.branch.clone(),
                required_artifact_kinds: stage
                    .artifacts
                    .iter()
                    .map(|artifact| artifact.kind.clone())
                    .collect(),
                visible_text_contains: stage
                    .visible_text
                    .as_ref()
                    .filter(|text| !text.is_empty())
                    .map(|text| text.chars().take(80).collect()),
            })
            .collect(),
    }
}

pub fn evaluate_run_against_fixture(run: &RunRecord, fixture: &ReplayFixture) -> ReplayEvalReport {
    let mut failures = Vec::new();
    if run.status != fixture.expected_status {
        failures.push(format!(
            "run status mismatch: expected {}, got {}",
            fixture.expected_status, run.status
        ));
    }
    let stages_by_id: BTreeMap<&str, &RunStageRecord> =
        run.stages.iter().map(|s| (s.node_id.as_str(), s)).collect();
    for assertion in &fixture.stage_assertions {
        let Some(stage) = stages_by_id.get(assertion.node_id.as_str()) else {
            failures.push(format!("missing stage {}", assertion.node_id));
            continue;
        };
        if stage.status != assertion.expected_status {
            failures.push(format!(
                "stage {} status mismatch: expected {}, got {}",
                assertion.node_id, assertion.expected_status, stage.status
            ));
        }
        if stage.outcome != assertion.expected_outcome {
            failures.push(format!(
                "stage {} outcome mismatch: expected {}, got {}",
                assertion.node_id, assertion.expected_outcome, stage.outcome
            ));
        }
        if stage.branch != assertion.expected_branch {
            failures.push(format!(
                "stage {} branch mismatch: expected {:?}, got {:?}",
                assertion.node_id, assertion.expected_branch, stage.branch
            ));
        }
        for required_kind in &assertion.required_artifact_kinds {
            if !stage
                .artifacts
                .iter()
                .any(|artifact| &artifact.kind == required_kind)
            {
                failures.push(format!(
                    "stage {} missing artifact kind {}",
                    assertion.node_id, required_kind
                ));
            }
        }
        if let Some(snippet) = &assertion.visible_text_contains {
            let actual = stage.visible_text.clone().unwrap_or_default();
            if !actual.contains(snippet) {
                failures.push(format!(
                    "stage {} visible text does not contain expected snippet {:?}",
                    assertion.node_id, snippet
                ));
            }
        }
    }

    ReplayEvalReport {
        pass: failures.is_empty(),
        failures,
        stage_count: run.stages.len(),
    }
}

pub fn evaluate_run_suite(
    cases: Vec<(RunRecord, ReplayFixture, Option<String>)>,
) -> ReplayEvalSuiteReport {
    let mut reports = Vec::new();
    for (run, fixture, source_path) in cases {
        let report = evaluate_run_against_fixture(&run, &fixture);
        reports.push(ReplayEvalCaseReport {
            run_id: run.id.clone(),
            workflow_id: run.workflow_id.clone(),
            label: None,
            pass: report.pass,
            failures: report.failures,
            stage_count: report.stage_count,
            source_path,
            comparison: None,
        });
    }
    let total = reports.len();
    let passed = reports.iter().filter(|report| report.pass).count();
    let failed = total.saturating_sub(passed);
    ReplayEvalSuiteReport {
        pass: failed == 0,
        total,
        passed,
        failed,
        cases: reports,
    }
}

pub fn diff_run_records(left: &RunRecord, right: &RunRecord) -> RunDiffReport {
    let mut stage_diffs = Vec::new();
    let mut all_node_ids = BTreeSet::new();
    let left_by_id: BTreeMap<&str, &RunStageRecord> = left
        .stages
        .iter()
        .map(|s| (s.node_id.as_str(), s))
        .collect();
    let right_by_id: BTreeMap<&str, &RunStageRecord> = right
        .stages
        .iter()
        .map(|s| (s.node_id.as_str(), s))
        .collect();
    all_node_ids.extend(left_by_id.keys().copied());
    all_node_ids.extend(right_by_id.keys().copied());

    for node_id in all_node_ids {
        let left_stage = left_by_id.get(node_id).copied();
        let right_stage = right_by_id.get(node_id).copied();
        match (left_stage, right_stage) {
            (Some(_), None) => stage_diffs.push(RunStageDiffRecord {
                node_id: node_id.to_string(),
                change: "removed".to_string(),
                details: vec!["stage missing from right run".to_string()],
            }),
            (None, Some(_)) => stage_diffs.push(RunStageDiffRecord {
                node_id: node_id.to_string(),
                change: "added".to_string(),
                details: vec!["stage missing from left run".to_string()],
            }),
            (Some(left_stage), Some(right_stage)) => {
                let mut details = Vec::new();
                if left_stage.status != right_stage.status {
                    details.push(format!(
                        "status: {} -> {}",
                        left_stage.status, right_stage.status
                    ));
                }
                if left_stage.outcome != right_stage.outcome {
                    details.push(format!(
                        "outcome: {} -> {}",
                        left_stage.outcome, right_stage.outcome
                    ));
                }
                if left_stage.branch != right_stage.branch {
                    details.push(format!(
                        "branch: {:?} -> {:?}",
                        left_stage.branch, right_stage.branch
                    ));
                }
                if left_stage.produced_artifact_ids.len() != right_stage.produced_artifact_ids.len()
                {
                    details.push(format!(
                        "produced_artifacts: {} -> {}",
                        left_stage.produced_artifact_ids.len(),
                        right_stage.produced_artifact_ids.len()
                    ));
                }
                if left_stage.artifacts.len() != right_stage.artifacts.len() {
                    details.push(format!(
                        "artifact_records: {} -> {}",
                        left_stage.artifacts.len(),
                        right_stage.artifacts.len()
                    ));
                }
                if !details.is_empty() {
                    stage_diffs.push(RunStageDiffRecord {
                        node_id: node_id.to_string(),
                        change: "changed".to_string(),
                        details,
                    });
                }
            }
            (None, None) => {}
        }
    }

    // Tool recording diffs
    let mut tool_diffs = Vec::new();
    let left_tools: std::collections::BTreeMap<(String, String), &ToolCallRecord> = left
        .tool_recordings
        .iter()
        .map(|r| ((r.tool_name.clone(), r.args_hash.clone()), r))
        .collect();
    let right_tools: std::collections::BTreeMap<(String, String), &ToolCallRecord> = right
        .tool_recordings
        .iter()
        .map(|r| ((r.tool_name.clone(), r.args_hash.clone()), r))
        .collect();
    let all_tool_keys: std::collections::BTreeSet<_> = left_tools
        .keys()
        .chain(right_tools.keys())
        .cloned()
        .collect();
    for key in &all_tool_keys {
        let l = left_tools.get(key);
        let r = right_tools.get(key);
        let result_changed = match (l, r) {
            (Some(a), Some(b)) => a.result != b.result,
            _ => true,
        };
        if result_changed {
            tool_diffs.push(ToolCallDiffRecord {
                tool_name: key.0.clone(),
                args_hash: key.1.clone(),
                result_changed,
                left_result: l.map(|t| t.result.clone()),
                right_result: r.map(|t| t.result.clone()),
            });
        }
    }

    let status_changed = left.status != right.status;
    let identical = !status_changed
        && stage_diffs.is_empty()
        && tool_diffs.is_empty()
        && left.transitions.len() == right.transitions.len()
        && left.artifacts.len() == right.artifacts.len()
        && left.checkpoints.len() == right.checkpoints.len();

    RunDiffReport {
        left_run_id: left.id.clone(),
        right_run_id: right.id.clone(),
        identical,
        status_changed,
        left_status: left.status.clone(),
        right_status: right.status.clone(),
        stage_diffs,
        tool_diffs,
        transition_count_delta: right.transitions.len() as isize - left.transitions.len() as isize,
        artifact_count_delta: right.artifacts.len() as isize - left.artifacts.len() as isize,
        checkpoint_count_delta: right.checkpoints.len() as isize - left.checkpoints.len() as isize,
    }
}
