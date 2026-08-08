//! Contamination-safe held-out gates for skill and guidance candidates.
//!
//! The gate is deliberately data-driven: local or hosted runners can produce
//! with/without observations however they execute models, then this module
//! applies the reusable admission policy, contamination filter, context-cost
//! accounting, grader checksum checks, and receipt shaping.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use super::estimate_chunk_tokens;
use crate::value::VmError;

pub const SKILL_GATE_SCHEMA_VERSION: u32 = 1;
pub const SKILL_GATE_MANIFEST_TYPE: &str = "harn.skill_gate.manifest.v1";
pub const SKILL_GATE_REPORT_TYPE: &str = "harn.skill_gate.report.v1";
pub const SKILL_GATE_RECEIPT_TYPE: &str = "harn.skill_gate.receipt.v1";

const EPSILON: f64 = 0.000_000_1;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SkillGateManifest {
    #[serde(rename = "_type")]
    pub type_name: String,
    pub version: u32,
    pub id: String,
    pub name: Option<String>,
    pub description: Option<String>,
    #[serde(default, alias = "base-dir")]
    pub base_dir: Option<String>,
    #[serde(default, alias = "target-model")]
    pub target_model: SkillGateTargetModel,
    pub policy: SkillGatePolicy,
    pub grader: SkillGateGrader,
    pub tasks: Vec<SkillGateTask>,
    pub variants: Vec<SkillGateVariant>,
    pub metadata: BTreeMap<String, JsonValue>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SkillGateTargetModel {
    pub id: String,
    pub provider: Option<String>,
    #[serde(default, alias = "knowledge-cutoff")]
    pub knowledge_cutoff: Option<String>,
    #[serde(default, alias = "context-budget-tokens")]
    pub context_budget_tokens: Option<usize>,
    pub metadata: BTreeMap<String, JsonValue>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SkillGatePolicy {
    #[serde(default, alias = "min-included-tasks")]
    pub min_included_tasks: Option<usize>,
    #[serde(default, alias = "min-score-lift")]
    pub min_score_lift: Option<f64>,
    #[serde(default, alias = "min-gap-recovery")]
    pub min_gap_recovery: Option<f64>,
    #[serde(default, alias = "min-cluster-gap-recovery")]
    pub min_cluster_gap_recovery: Option<f64>,
    #[serde(default, alias = "require-cluster-lift")]
    pub require_cluster_lift: bool,
    #[serde(default, alias = "max-regression-rate")]
    pub max_regression_rate: Option<f64>,
    #[serde(default, alias = "min-win-rate")]
    pub min_win_rate: Option<f64>,
    #[serde(default, alias = "max-context-delta-tokens")]
    pub max_context_delta_tokens: Option<i64>,
    #[serde(default, alias = "pass-score-threshold")]
    pub pass_score_threshold: Option<f64>,
    #[serde(default, alias = "require-no-tamper")]
    pub require_no_tamper: Option<bool>,
    pub metadata: BTreeMap<String, JsonValue>,
}

impl SkillGatePolicy {
    fn min_included_tasks(&self) -> usize {
        self.min_included_tasks.unwrap_or(1)
    }

    fn min_score_lift(&self) -> f64 {
        self.min_score_lift.unwrap_or(0.0)
    }

    fn min_gap_recovery(&self) -> f64 {
        self.min_gap_recovery.unwrap_or(0.0)
    }

    fn min_cluster_gap_recovery(&self) -> f64 {
        self.min_cluster_gap_recovery
            .unwrap_or_else(|| self.min_gap_recovery())
    }

    fn max_regression_rate(&self) -> f64 {
        self.max_regression_rate.unwrap_or(0.0)
    }

    fn pass_score_threshold(&self) -> f64 {
        self.pass_score_threshold.unwrap_or(0.5)
    }

    fn require_no_tamper(&self) -> bool {
        self.require_no_tamper.unwrap_or(true)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SkillGateGrader {
    pub id: String,
    #[serde(default, alias = "immutable-paths", alias = "protected-paths")]
    pub immutable_paths: Vec<SkillGateProtectedPath>,
    pub metadata: BTreeMap<String, JsonValue>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SkillGateProtectedPath {
    pub path: String,
    pub sha256: String,
    pub label: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SkillGateTask {
    pub id: String,
    pub name: Option<String>,
    pub cluster: String,
    pub source: Option<String>,
    pub heldout: SkillGateHeldout,
    #[serde(default, alias = "baseline-score")]
    pub baseline_score: f64,
    #[serde(default, alias = "frontier-score")]
    pub frontier_score: f64,
    #[serde(default, alias = "baseline-passed")]
    pub baseline_passed: Option<bool>,
    pub metadata: BTreeMap<String, JsonValue>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SkillGateHeldout {
    pub kind: String,
    #[serde(default, alias = "created-at")]
    pub created_at: Option<String>,
    pub private: bool,
    pub suite: Option<String>,
    pub metadata: BTreeMap<String, JsonValue>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SkillGateVariant {
    pub id: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub baseline: SkillGateArtifact,
    pub candidate: SkillGateArtifact,
    #[serde(default, alias = "case-results")]
    pub case_results: Vec<SkillGateCaseResult>,
    pub metadata: BTreeMap<String, JsonValue>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SkillGateArtifact {
    pub kind: String,
    pub paths: Vec<String>,
    #[serde(default, alias = "context-tokens")]
    pub context_tokens: Option<usize>,
    pub metadata: BTreeMap<String, JsonValue>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SkillGateCaseResult {
    #[serde(default, alias = "task-id")]
    pub task_id: String,
    pub score: Option<f64>,
    pub passed: Option<bool>,
    pub notes: Option<String>,
    pub metadata: BTreeMap<String, JsonValue>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SkillGateReport {
    #[serde(rename = "_type")]
    pub type_name: String,
    pub schema_version: u32,
    pub manifest_id: String,
    pub manifest_name: Option<String>,
    pub target_model: SkillGateTargetModel,
    pub pass: bool,
    pub selected_variant_id: Option<String>,
    pub included_task_count: usize,
    pub excluded_task_count: usize,
    pub task_safety: Vec<SkillGateTaskSafetyReport>,
    pub tamper: SkillGateTamperReport,
    pub variants: Vec<SkillGateVariantReport>,
    pub pareto_frontier: Vec<String>,
    pub receipt: SkillGateReceipt,
    pub metadata: BTreeMap<String, JsonValue>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SkillGateTaskSafetyReport {
    pub task_id: String,
    pub cluster: String,
    pub included: bool,
    pub heldout_kind: String,
    pub created_at: Option<String>,
    pub private: bool,
    pub exclusion_reason: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SkillGateTamperReport {
    pub pass: bool,
    pub checks: Vec<SkillGateTamperCheck>,
    pub failures: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SkillGateTamperCheck {
    pub path: String,
    pub label: Option<String>,
    pub expected_sha256: String,
    pub actual_sha256: Option<String>,
    pub status: String,
    pub failure: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SkillGateVariantReport {
    pub id: String,
    pub name: Option<String>,
    pub accepted: bool,
    pub decision: String,
    pub failures: Vec<String>,
    pub warnings: Vec<String>,
    pub metrics: SkillGateVariantMetrics,
    pub context: SkillGateContextReport,
    pub clusters: Vec<SkillGateClusterReport>,
    pub cases: Vec<SkillGateCaseReport>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SkillGateVariantMetrics {
    pub included_task_count: usize,
    pub scored_task_count: usize,
    pub gap_task_count: usize,
    pub mean_baseline_score: f64,
    pub mean_candidate_score: f64,
    pub mean_frontier_score: f64,
    pub mean_score_lift: f64,
    pub mean_gap_recovery: f64,
    pub candidate_win_count: usize,
    pub candidate_tie_count: usize,
    pub candidate_loss_count: usize,
    pub win_rate: f64,
    pub regression_count: usize,
    pub regression_denominator: usize,
    pub regression_rate: f64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SkillGateContextReport {
    pub baseline_tokens: usize,
    pub candidate_tokens: usize,
    pub delta_tokens: i64,
    pub max_delta_tokens: Option<i64>,
    pub target_context_budget_tokens: Option<usize>,
    pub within_delta_budget: bool,
    pub within_target_budget: bool,
    pub artifact_hashes: Vec<SkillGateArtifactHash>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SkillGateArtifactHash {
    pub role: String,
    pub path: String,
    pub sha256: String,
    pub tokens: usize,
    pub bytes: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SkillGateClusterReport {
    pub cluster: String,
    pub task_count: usize,
    pub gap_task_count: usize,
    pub mean_baseline_score: f64,
    pub mean_candidate_score: f64,
    pub mean_frontier_score: f64,
    pub mean_score_lift: f64,
    pub mean_gap_recovery: f64,
    pub pass: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SkillGateCaseReport {
    pub task_id: String,
    pub cluster: String,
    pub included: bool,
    pub exclusion_reason: Option<String>,
    pub baseline_score: f64,
    pub candidate_score: Option<f64>,
    pub frontier_score: f64,
    pub score_lift: Option<f64>,
    pub gap_recovery: Option<f64>,
    pub baseline_passed: bool,
    pub candidate_passed: Option<bool>,
    pub regression: bool,
    pub failures: Vec<String>,
    pub notes: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SkillGateReceipt {
    #[serde(rename = "_type")]
    pub type_name: String,
    pub schema_version: u32,
    pub manifest_id: String,
    pub target_model_id: String,
    pub accepted: bool,
    pub selected_variant_id: Option<String>,
    pub decision: String,
    pub metrics: Option<SkillGateVariantMetrics>,
    pub context: Option<SkillGateContextReport>,
    pub tamper: SkillGateTamperReport,
    pub pareto_frontier: Vec<String>,
    pub excluded_task_ids: Vec<String>,
    pub variant_receipts: Vec<SkillGateVariantReceipt>,
    pub metadata: BTreeMap<String, JsonValue>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SkillGateVariantReceipt {
    pub variant_id: String,
    pub accepted: bool,
    pub decision: String,
    pub metrics: SkillGateVariantMetrics,
    pub context_delta_tokens: i64,
    pub failures: Vec<String>,
}

pub fn load_skill_gate_manifest(path: &Path) -> Result<SkillGateManifest, VmError> {
    let content = fs::read_to_string(path).map_err(|error| {
        VmError::Runtime(format!("failed to read skill gate manifest: {error}"))
    })?;
    let mut manifest: SkillGateManifest =
        if path.extension().and_then(|ext| ext.to_str()) == Some("toml") {
            toml::from_str(&content).map_err(|error| {
                VmError::Runtime(format!("failed to parse skill gate TOML: {error}"))
            })?
        } else {
            serde_json::from_str(&content).map_err(|error| {
                VmError::Runtime(format!("failed to parse skill gate JSON: {error}"))
            })?
        };
    if manifest.base_dir.is_none() {
        manifest.base_dir = path.parent().map(|parent| parent.display().to_string());
    }
    normalize_skill_gate_manifest(&mut manifest)?;
    Ok(manifest)
}

pub fn evaluate_skill_gate_manifest(
    manifest: &SkillGateManifest,
) -> Result<SkillGateReport, VmError> {
    let mut manifest = manifest.clone();
    normalize_skill_gate_manifest(&mut manifest)?;
    let base_dir = manifest.base_dir.as_deref().map(Path::new);
    let task_safety = manifest
        .tasks
        .iter()
        .map(|task| task_safety_report(task, &manifest.target_model))
        .collect::<Vec<_>>();
    let included_task_count = task_safety.iter().filter(|task| task.included).count();
    let excluded_task_count = task_safety.len().saturating_sub(included_task_count);
    let safety_by_id = task_safety
        .iter()
        .map(|task| (task.task_id.as_str(), task))
        .collect::<BTreeMap<_, _>>();
    let tamper = verify_immutable_grader(&manifest.grader, base_dir);
    let variants = manifest
        .variants
        .iter()
        .map(|variant| evaluate_variant(variant, &manifest, &safety_by_id, &tamper, base_dir))
        .collect::<Vec<_>>();
    let pareto_frontier = pareto_frontier(&variants);
    let selected_variant_id = select_variant(&variants, &pareto_frontier);
    let pass = selected_variant_id.is_some();
    let receipt = build_receipt(
        &manifest,
        pass,
        selected_variant_id.clone(),
        &task_safety,
        tamper.clone(),
        &variants,
        pareto_frontier.clone(),
    );
    Ok(SkillGateReport {
        type_name: SKILL_GATE_REPORT_TYPE.to_string(),
        schema_version: SKILL_GATE_SCHEMA_VERSION,
        manifest_id: manifest.id,
        manifest_name: manifest.name,
        target_model: manifest.target_model,
        pass,
        selected_variant_id,
        included_task_count,
        excluded_task_count,
        task_safety,
        tamper,
        variants,
        pareto_frontier,
        receipt,
        metadata: manifest.metadata,
    })
}

fn normalize_skill_gate_manifest(manifest: &mut SkillGateManifest) -> Result<(), VmError> {
    if manifest.type_name.is_empty() {
        manifest.type_name = SKILL_GATE_MANIFEST_TYPE.to_string();
    }
    if manifest.type_name != SKILL_GATE_MANIFEST_TYPE {
        return Err(VmError::Runtime(format!(
            "skill gate manifest _type must be {SKILL_GATE_MANIFEST_TYPE}"
        )));
    }
    if manifest.version == 0 {
        manifest.version = SKILL_GATE_SCHEMA_VERSION;
    }
    if manifest.version != SKILL_GATE_SCHEMA_VERSION {
        return Err(VmError::Runtime(format!(
            "skill gate manifest version must be {SKILL_GATE_SCHEMA_VERSION}"
        )));
    }
    if manifest.id.trim().is_empty() {
        manifest.id = "skill-gate".to_string();
    }
    if manifest.target_model.id.trim().is_empty() {
        return Err(VmError::Runtime(
            "skill gate manifest target_model.id is required".to_string(),
        ));
    }
    if manifest.tasks.is_empty() {
        return Err(VmError::Runtime(
            "skill gate manifest must declare at least one task".to_string(),
        ));
    }
    if manifest.variants.is_empty() {
        return Err(VmError::Runtime(
            "skill gate manifest must declare at least one variant".to_string(),
        ));
    }
    let mut task_ids = BTreeSet::new();
    for (index, task) in manifest.tasks.iter_mut().enumerate() {
        if task.id.trim().is_empty() {
            task.id = format!("task_{}", index + 1);
        }
        if !task_ids.insert(task.id.clone()) {
            return Err(VmError::Runtime(format!(
                "skill gate manifest has duplicate task id '{}'",
                task.id
            )));
        }
        if task.cluster.trim().is_empty() {
            task.cluster = "default".to_string();
        }
        validate_score("baseline_score", &task.id, task.baseline_score)?;
        validate_score("frontier_score", &task.id, task.frontier_score)?;
    }
    let mut variant_ids = BTreeSet::new();
    for (index, variant) in manifest.variants.iter_mut().enumerate() {
        if variant.id.trim().is_empty() {
            variant.id = format!("variant_{}", index + 1);
        }
        if !variant_ids.insert(variant.id.clone()) {
            return Err(VmError::Runtime(format!(
                "skill gate manifest has duplicate variant id '{}'",
                variant.id
            )));
        }
        let mut result_ids = BTreeSet::new();
        for result in &variant.case_results {
            if result.task_id.trim().is_empty() {
                return Err(VmError::Runtime(format!(
                    "skill gate variant '{}' has a case result with no task_id",
                    variant.id
                )));
            }
            if !task_ids.contains(&result.task_id) {
                return Err(VmError::Runtime(format!(
                    "skill gate variant '{}' references unknown task '{}'",
                    variant.id, result.task_id
                )));
            }
            if !result_ids.insert(result.task_id.clone()) {
                return Err(VmError::Runtime(format!(
                    "skill gate variant '{}' has duplicate result for task '{}'",
                    variant.id, result.task_id
                )));
            }
            if let Some(score) = result.score {
                validate_score("candidate score", &result.task_id, score)?;
            }
        }
    }
    Ok(())
}

fn validate_score(label: &str, task_id: &str, score: f64) -> Result<(), VmError> {
    if !(0.0..=1.0).contains(&score) {
        return Err(VmError::Runtime(format!(
            "skill gate task '{task_id}' {label} must be between 0 and 1"
        )));
    }
    Ok(())
}

fn task_safety_report(
    task: &SkillGateTask,
    target_model: &SkillGateTargetModel,
) -> SkillGateTaskSafetyReport {
    let kind = normalize_kind(&task.heldout.kind);
    let (included, exclusion_reason) = if task.heldout.private || kind == "private" {
        (true, None)
    } else if matches!(kind.as_str(), "public_static" | "static" | "pre_cutoff") {
        (
            false,
            Some("static public or declared pre-cutoff task is contamination-prone".to_string()),
        )
    } else if matches!(
        kind.as_str(),
        "post_cutoff" | "rolling" | "livecodebench" | "swe_mera" | "swe_rebench"
    ) {
        match (
            task.heldout.created_at.as_deref(),
            target_model.knowledge_cutoff.as_deref(),
        ) {
            (Some(created_at), Some(cutoff)) if date_after(created_at, cutoff).unwrap_or(false) => {
                (true, None)
            }
            (Some(_), Some(cutoff)) => (
                false,
                Some(format!(
                    "task does not post-date target model cutoff {cutoff}"
                )),
            ),
            (Some(_), None) => (
                false,
                Some(
                    "target model knowledge_cutoff is required for non-private held-out tasks"
                        .to_string(),
                ),
            ),
            (None, _) => (
                false,
                Some("non-private held-out task must declare created_at".to_string()),
            ),
        }
    } else {
        (
            false,
            Some(format!(
                "held-out kind '{}' is not recognized as contamination-safe",
                task.heldout.kind
            )),
        )
    };

    SkillGateTaskSafetyReport {
        task_id: task.id.clone(),
        cluster: task.cluster.clone(),
        included,
        heldout_kind: task.heldout.kind.clone(),
        created_at: task.heldout.created_at.clone(),
        private: task.heldout.private,
        exclusion_reason,
    }
}

fn normalize_kind(kind: &str) -> String {
    kind.trim().to_ascii_lowercase().replace(['-', ' '], "_")
}

fn date_after(created_at: &str, cutoff: &str) -> Option<bool> {
    Some(parse_date_prefix(created_at)? > parse_date_prefix(cutoff)?)
}

fn parse_date_prefix(value: &str) -> Option<(u32, u32, u32)> {
    let trimmed = value.trim();
    let prefix = trimmed.get(..10)?;
    let bytes = prefix.as_bytes();
    if bytes.get(4) != Some(&b'-') || bytes.get(7) != Some(&b'-') {
        return None;
    }
    for index in [0, 1, 2, 3, 5, 6, 8, 9] {
        if !bytes[index].is_ascii_digit() {
            return None;
        }
    }
    let year = prefix[0..4].parse::<u32>().ok()?;
    let month = prefix[5..7].parse::<u32>().ok()?;
    let day = prefix[8..10].parse::<u32>().ok()?;
    let max_day = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => return None,
    };
    if day == 0 || day > max_day {
        None
    } else {
        Some((year, month, day))
    }
}

fn is_leap_year(year: u32) -> bool {
    (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400)
}

fn verify_immutable_grader(
    grader: &SkillGateGrader,
    base_dir: Option<&Path>,
) -> SkillGateTamperReport {
    let mut checks = Vec::new();
    let mut failures = Vec::new();
    for protected in &grader.immutable_paths {
        let resolved = resolve_manifest_path(base_dir, &protected.path);
        let mut check = SkillGateTamperCheck {
            path: protected.path.clone(),
            label: protected.label.clone(),
            expected_sha256: protected.sha256.clone(),
            ..Default::default()
        };
        match sha256_path(&resolved) {
            Ok(hash) => {
                check.actual_sha256 = Some(hash.sha256.clone());
                if hash.sha256.eq_ignore_ascii_case(protected.sha256.trim()) {
                    check.status = "pass".to_string();
                } else {
                    check.status = "fail".to_string();
                    check.failure = Some(format!(
                        "checksum mismatch for immutable grader path {}",
                        protected.path
                    ));
                }
            }
            Err(error) => {
                check.status = "fail".to_string();
                check.failure = Some(error);
            }
        }
        if let Some(failure) = &check.failure {
            failures.push(failure.clone());
        }
        checks.push(check);
    }
    SkillGateTamperReport {
        pass: failures.is_empty(),
        checks,
        failures,
    }
}

fn evaluate_variant(
    variant: &SkillGateVariant,
    manifest: &SkillGateManifest,
    safety_by_id: &BTreeMap<&str, &SkillGateTaskSafetyReport>,
    tamper: &SkillGateTamperReport,
    base_dir: Option<&Path>,
) -> SkillGateVariantReport {
    let mut failures = Vec::new();
    let mut warnings = Vec::new();
    let mut context_valid = true;
    let context = match measure_context(variant, manifest, base_dir) {
        Ok(context) => context,
        Err(error) => {
            context_valid = false;
            failures.push(error);
            SkillGateContextReport::default()
        }
    };
    let results_by_task = variant
        .case_results
        .iter()
        .map(|result| (result.task_id.as_str(), result))
        .collect::<BTreeMap<_, _>>();
    let cases = manifest
        .tasks
        .iter()
        .map(|task| {
            evaluate_case(
                task,
                results_by_task.get(task.id.as_str()).copied(),
                safety_by_id.get(task.id.as_str()).copied(),
                manifest.policy.pass_score_threshold(),
            )
        })
        .collect::<Vec<_>>();
    for case in &cases {
        failures.extend(case.failures.iter().cloned());
    }
    let metrics = aggregate_variant_metrics(&cases);
    let clusters = aggregate_cluster_reports(&cases, manifest.policy.min_cluster_gap_recovery());
    if !tamper.pass && manifest.policy.require_no_tamper() {
        failures.push("immutable grader check failed".to_string());
    }
    if metrics.included_task_count < manifest.policy.min_included_tasks() {
        failures.push(format!(
            "included held-out task count {} is below required {}",
            metrics.included_task_count,
            manifest.policy.min_included_tasks()
        ));
    }
    if metrics.scored_task_count == 0 {
        failures.push("no contamination-safe scored tasks were available".to_string());
    }
    if metrics.mean_score_lift + EPSILON < manifest.policy.min_score_lift() {
        failures.push(format!(
            "mean score lift {:.4} is below required {:.4}",
            metrics.mean_score_lift,
            manifest.policy.min_score_lift()
        ));
    }
    if metrics.mean_gap_recovery + EPSILON < manifest.policy.min_gap_recovery() {
        failures.push(format!(
            "mean gap recovery {:.4} is below required {:.4}",
            metrics.mean_gap_recovery,
            manifest.policy.min_gap_recovery()
        ));
    }
    if metrics.regression_rate > manifest.policy.max_regression_rate() + EPSILON {
        failures.push(format!(
            "regression rate {:.4} exceeds allowed {:.4}",
            metrics.regression_rate,
            manifest.policy.max_regression_rate()
        ));
    }
    if let Some(min_win_rate) = manifest.policy.min_win_rate {
        if metrics.win_rate + EPSILON < min_win_rate {
            failures.push(format!(
                "candidate win rate {:.4} is below required {:.4}",
                metrics.win_rate, min_win_rate
            ));
        }
    }
    if context_valid && !context.within_delta_budget {
        failures.push(format!(
            "context delta {} tokens exceeds allowed {}",
            context.delta_tokens,
            context.max_delta_tokens.unwrap_or_default()
        ));
    }
    if context_valid && !context.within_target_budget {
        failures.push(format!(
            "candidate context {} tokens exceeds target budget {}",
            context.candidate_tokens,
            context.target_context_budget_tokens.unwrap_or_default()
        ));
    }
    if manifest.policy.require_cluster_lift {
        for cluster in &clusters {
            if !cluster.pass {
                failures.push(format!(
                    "cluster '{}' gap recovery {:.4} is below required {:.4}",
                    cluster.cluster,
                    cluster.mean_gap_recovery,
                    manifest.policy.min_cluster_gap_recovery()
                ));
            }
        }
    }
    if manifest.grader.immutable_paths.is_empty() {
        let warning = "no immutable grader paths were declared".to_string();
        if manifest.policy.require_no_tamper() {
            failures.push(warning.clone());
        }
        warnings.push(warning);
    }
    let accepted = failures.is_empty();
    SkillGateVariantReport {
        id: variant.id.clone(),
        name: variant.name.clone(),
        accepted,
        decision: if accepted {
            "accepted".to_string()
        } else {
            "rejected".to_string()
        },
        failures,
        warnings,
        metrics,
        context,
        clusters,
        cases,
    }
}

fn evaluate_case(
    task: &SkillGateTask,
    result: Option<&SkillGateCaseResult>,
    safety: Option<&SkillGateTaskSafetyReport>,
    pass_score_threshold: f64,
) -> SkillGateCaseReport {
    let included = safety.is_none_or(|safety| safety.included);
    let exclusion_reason = safety.and_then(|safety| safety.exclusion_reason.clone());
    let baseline_passed = task
        .baseline_passed
        .unwrap_or(task.baseline_score >= pass_score_threshold);
    let mut report = SkillGateCaseReport {
        task_id: task.id.clone(),
        cluster: task.cluster.clone(),
        included,
        exclusion_reason,
        baseline_score: task.baseline_score,
        candidate_score: result.and_then(|result| result.score),
        frontier_score: task.frontier_score,
        baseline_passed,
        candidate_passed: result.map(|result| {
            result
                .passed
                .unwrap_or_else(|| result.score.unwrap_or(0.0) >= pass_score_threshold)
        }),
        notes: result.and_then(|result| result.notes.clone()),
        ..Default::default()
    };
    if !included {
        return report;
    }
    let Some(candidate_score) = report.candidate_score else {
        report
            .failures
            .push(format!("variant is missing result for task '{}'", task.id));
        return report;
    };
    let score_lift = candidate_score - task.baseline_score;
    report.score_lift = Some(score_lift);
    if task.frontier_score > task.baseline_score + EPSILON {
        report.gap_recovery = Some(score_lift / (task.frontier_score - task.baseline_score));
    }
    let candidate_passed = report.candidate_passed.unwrap_or(false);
    report.regression = baseline_passed && !candidate_passed;
    if report.regression {
        report.failures.push(format!(
            "task '{}' regressed from passing to failing",
            task.id
        ));
    }
    report
}

fn aggregate_variant_metrics(cases: &[SkillGateCaseReport]) -> SkillGateVariantMetrics {
    let included_task_count = cases.iter().filter(|case| case.included).count();
    let scored = cases
        .iter()
        .filter(|case| case.included && case.candidate_score.is_some())
        .collect::<Vec<_>>();
    let scored_task_count = scored.len();
    let gap_cases = scored
        .iter()
        .filter(|case| case.gap_recovery.is_some())
        .collect::<Vec<_>>();
    let gap_task_count = gap_cases.len();
    let mut metrics = SkillGateVariantMetrics {
        included_task_count,
        scored_task_count,
        gap_task_count,
        ..Default::default()
    };
    if scored_task_count > 0 {
        metrics.mean_baseline_score =
            scored.iter().map(|case| case.baseline_score).sum::<f64>() / scored_task_count as f64;
        metrics.mean_candidate_score = scored
            .iter()
            .map(|case| case.candidate_score.unwrap_or_default())
            .sum::<f64>()
            / scored_task_count as f64;
        metrics.mean_frontier_score =
            scored.iter().map(|case| case.frontier_score).sum::<f64>() / scored_task_count as f64;
        metrics.mean_score_lift = scored
            .iter()
            .map(|case| case.score_lift.unwrap_or_default())
            .sum::<f64>()
            / scored_task_count as f64;
        metrics.candidate_win_count = scored
            .iter()
            .filter(|case| case.score_lift.unwrap_or_default() > EPSILON)
            .count();
        metrics.candidate_loss_count = scored
            .iter()
            .filter(|case| case.score_lift.unwrap_or_default() < -EPSILON)
            .count();
        metrics.candidate_tie_count = scored_task_count
            .saturating_sub(metrics.candidate_win_count + metrics.candidate_loss_count);
        metrics.win_rate = metrics.candidate_win_count as f64 / scored_task_count as f64;
    }
    if gap_task_count > 0 {
        metrics.mean_gap_recovery = gap_cases
            .iter()
            .map(|case| case.gap_recovery.unwrap_or_default())
            .sum::<f64>()
            / gap_task_count as f64;
    }
    metrics.regression_denominator = cases
        .iter()
        .filter(|case| case.included && case.baseline_passed)
        .count();
    metrics.regression_count = cases
        .iter()
        .filter(|case| case.included && case.regression)
        .count();
    if metrics.regression_denominator > 0 {
        metrics.regression_rate =
            metrics.regression_count as f64 / metrics.regression_denominator as f64;
    }
    metrics
}

fn aggregate_cluster_reports(
    cases: &[SkillGateCaseReport],
    min_cluster_gap_recovery: f64,
) -> Vec<SkillGateClusterReport> {
    let mut grouped: BTreeMap<String, Vec<&SkillGateCaseReport>> = BTreeMap::new();
    for case in cases
        .iter()
        .filter(|case| case.included && case.candidate_score.is_some())
    {
        grouped.entry(case.cluster.clone()).or_default().push(case);
    }
    grouped
        .into_iter()
        .map(|(cluster, cases)| {
            let task_count = cases.len();
            let gap_cases = cases
                .iter()
                .filter(|case| case.gap_recovery.is_some())
                .copied()
                .collect::<Vec<_>>();
            let gap_task_count = gap_cases.len();
            let mean_baseline_score =
                cases.iter().map(|case| case.baseline_score).sum::<f64>() / task_count as f64;
            let mean_candidate_score = cases
                .iter()
                .map(|case| case.candidate_score.unwrap_or_default())
                .sum::<f64>()
                / task_count as f64;
            let mean_frontier_score =
                cases.iter().map(|case| case.frontier_score).sum::<f64>() / task_count as f64;
            let mean_score_lift = cases
                .iter()
                .map(|case| case.score_lift.unwrap_or_default())
                .sum::<f64>()
                / task_count as f64;
            let mean_gap_recovery = if gap_task_count == 0 {
                0.0
            } else {
                gap_cases
                    .iter()
                    .map(|case| case.gap_recovery.unwrap_or_default())
                    .sum::<f64>()
                    / gap_task_count as f64
            };
            SkillGateClusterReport {
                cluster,
                task_count,
                gap_task_count,
                mean_baseline_score,
                mean_candidate_score,
                mean_frontier_score,
                mean_score_lift,
                mean_gap_recovery,
                pass: gap_task_count == 0
                    || mean_gap_recovery + EPSILON >= min_cluster_gap_recovery,
            }
        })
        .collect()
}

fn measure_context(
    variant: &SkillGateVariant,
    manifest: &SkillGateManifest,
    base_dir: Option<&Path>,
) -> Result<SkillGateContextReport, String> {
    let baseline = measure_artifact("baseline", &variant.baseline, base_dir)?;
    let candidate = measure_artifact("candidate", &variant.candidate, base_dir)?;
    let baseline_tokens = baseline.context_tokens;
    let candidate_tokens = candidate.context_tokens;
    let delta_tokens = candidate_tokens as i64 - baseline_tokens as i64;
    let max_delta_tokens = manifest.policy.max_context_delta_tokens;
    let target_context_budget_tokens = manifest.target_model.context_budget_tokens;
    let within_delta_budget = max_delta_tokens.is_none_or(|max| delta_tokens <= max);
    let within_target_budget =
        target_context_budget_tokens.is_none_or(|max| candidate_tokens <= max);
    let mut artifact_hashes = baseline.hashes;
    artifact_hashes.extend(candidate.hashes);
    Ok(SkillGateContextReport {
        baseline_tokens,
        candidate_tokens,
        delta_tokens,
        max_delta_tokens,
        target_context_budget_tokens,
        within_delta_budget,
        within_target_budget,
        artifact_hashes,
    })
}

#[derive(Debug, Default)]
struct ArtifactMeasurement {
    context_tokens: usize,
    hashes: Vec<SkillGateArtifactHash>,
}

fn measure_artifact(
    role: &str,
    artifact: &SkillGateArtifact,
    base_dir: Option<&Path>,
) -> Result<ArtifactMeasurement, String> {
    let mut measurement = ArtifactMeasurement::default();
    for path in &artifact.paths {
        let resolved = resolve_manifest_path(base_dir, path);
        let hash = sha256_path(&resolved)?;
        measurement.context_tokens += hash.tokens;
        measurement.hashes.push(SkillGateArtifactHash {
            role: role.to_string(),
            path: path.clone(),
            sha256: hash.sha256,
            tokens: hash.tokens,
            bytes: hash.bytes,
        });
    }
    if let Some(tokens) = artifact.context_tokens {
        measurement.context_tokens = tokens;
    }
    Ok(measurement)
}

#[derive(Debug)]
struct PathHash {
    sha256: String,
    tokens: usize,
    bytes: usize,
}

fn sha256_path(path: &Path) -> Result<PathHash, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("failed to stat {}: {error}", path.display()))?;
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "refusing to hash symlink protected path {}",
            path.display()
        ));
    }
    if metadata.is_file() {
        return sha256_file(path);
    }
    if metadata.is_dir() {
        return sha256_dir(path);
    }
    Err(format!(
        "protected path {} is neither a file nor a directory",
        path.display()
    ))
}

fn sha256_file(path: &Path) -> Result<PathHash, String> {
    let bytes =
        fs::read(path).map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    let sha256 = hex_digest(&bytes);
    let tokens = estimate_chunk_tokens(&String::from_utf8_lossy(&bytes));
    Ok(PathHash {
        sha256,
        tokens,
        bytes: bytes.len(),
    })
}

fn sha256_dir(path: &Path) -> Result<PathHash, String> {
    let mut files = Vec::new();
    for entry in WalkDir::new(path).follow_links(false) {
        let entry = entry.map_err(|error| format!("failed to walk {}: {error}", path.display()))?;
        if entry.file_type().is_symlink() {
            return Err(format!(
                "refusing to hash symlink inside protected directory {}",
                entry.path().display()
            ));
        }
        if entry.file_type().is_file() {
            files.push(entry.path().to_path_buf());
        }
    }
    files.sort();
    let mut hasher = Sha256::new();
    let mut tokens = 0;
    let mut bytes_total = 0;
    for file in files {
        let rel = file
            .strip_prefix(path)
            .map_err(|error| format!("failed to relativize {}: {error}", file.display()))?;
        let rel = rel.to_string_lossy().replace('\\', "/");
        let bytes = fs::read(&file)
            .map_err(|error| format!("failed to read {}: {error}", file.display()))?;
        hasher.update(rel.as_bytes());
        hasher.update([0]);
        hasher.update(&bytes);
        hasher.update([0xff]);
        tokens += estimate_chunk_tokens(&String::from_utf8_lossy(&bytes));
        bytes_total += bytes.len();
    }
    Ok(PathHash {
        sha256: bytes_to_hex(hasher.finalize().as_ref()),
        tokens,
        bytes: bytes_total,
    })
}

fn hex_digest(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    bytes_to_hex(hasher.finalize().as_ref())
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
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

fn pareto_frontier(variants: &[SkillGateVariantReport]) -> Vec<String> {
    variants
        .iter()
        .filter(|variant| variant.metrics.scored_task_count > 0)
        .filter(|variant| {
            !variants
                .iter()
                .any(|other| other.id != variant.id && dominates(other, variant))
        })
        .map(|variant| variant.id.clone())
        .collect()
}

fn dominates(left: &SkillGateVariantReport, right: &SkillGateVariantReport) -> bool {
    if left.metrics.scored_task_count == 0 {
        return false;
    }
    let at_least_as_good = left.metrics.mean_gap_recovery + EPSILON
        >= right.metrics.mean_gap_recovery
        && left.metrics.mean_score_lift + EPSILON >= right.metrics.mean_score_lift
        && left.metrics.regression_rate <= right.metrics.regression_rate + EPSILON
        && left.context.delta_tokens <= right.context.delta_tokens;
    let strictly_better = left.metrics.mean_gap_recovery
        > right.metrics.mean_gap_recovery + EPSILON
        || left.metrics.mean_score_lift > right.metrics.mean_score_lift + EPSILON
        || left.metrics.regression_rate + EPSILON < right.metrics.regression_rate
        || left.context.delta_tokens < right.context.delta_tokens;
    at_least_as_good && strictly_better
}

fn select_variant(
    variants: &[SkillGateVariantReport],
    pareto_frontier: &[String],
) -> Option<String> {
    let frontier = pareto_frontier.iter().collect::<BTreeSet<_>>();
    let mut accepted = variants
        .iter()
        .filter(|variant| variant.accepted && frontier.contains(&variant.id))
        .collect::<Vec<_>>();
    if accepted.is_empty() {
        accepted = variants.iter().filter(|variant| variant.accepted).collect();
    }
    accepted.sort_by(|left, right| {
        right
            .metrics
            .mean_gap_recovery
            .partial_cmp(&left.metrics.mean_gap_recovery)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                right
                    .metrics
                    .mean_score_lift
                    .partial_cmp(&left.metrics.mean_score_lift)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| left.context.delta_tokens.cmp(&right.context.delta_tokens))
            .then_with(|| left.id.cmp(&right.id))
    });
    accepted.first().map(|variant| variant.id.clone())
}

fn build_receipt(
    manifest: &SkillGateManifest,
    accepted: bool,
    selected_variant_id: Option<String>,
    task_safety: &[SkillGateTaskSafetyReport],
    tamper: SkillGateTamperReport,
    variants: &[SkillGateVariantReport],
    pareto_frontier: Vec<String>,
) -> SkillGateReceipt {
    let selected = selected_variant_id
        .as_ref()
        .and_then(|id| variants.iter().find(|variant| &variant.id == id));
    SkillGateReceipt {
        type_name: SKILL_GATE_RECEIPT_TYPE.to_string(),
        schema_version: SKILL_GATE_SCHEMA_VERSION,
        manifest_id: manifest.id.clone(),
        target_model_id: manifest.target_model.id.clone(),
        accepted,
        selected_variant_id,
        decision: if accepted {
            "accepted".to_string()
        } else {
            "rejected".to_string()
        },
        metrics: selected.map(|variant| variant.metrics.clone()),
        context: selected.map(|variant| variant.context.clone()),
        tamper,
        pareto_frontier,
        excluded_task_ids: task_safety
            .iter()
            .filter(|task| !task.included)
            .map(|task| task.task_id.clone())
            .collect(),
        variant_receipts: variants
            .iter()
            .map(|variant| SkillGateVariantReceipt {
                variant_id: variant.id.clone(),
                accepted: variant.accepted,
                decision: variant.decision.clone(),
                metrics: variant.metrics.clone(),
                context_delta_tokens: variant.context.delta_tokens,
                failures: variant.failures.clone(),
            })
            .collect(),
        metadata: manifest.metadata.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, content: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn fixture_manifest(root: &Path, grader_hash: String) -> SkillGateManifest {
        SkillGateManifest {
            type_name: SKILL_GATE_MANIFEST_TYPE.to_string(),
            version: SKILL_GATE_SCHEMA_VERSION,
            id: "skill-gate-test".to_string(),
            base_dir: Some(root.display().to_string()),
            target_model: SkillGateTargetModel {
                id: "mock-cheap".to_string(),
                knowledge_cutoff: Some("2026-05-01".to_string()),
                context_budget_tokens: Some(220),
                ..Default::default()
            },
            policy: SkillGatePolicy {
                min_included_tasks: Some(2),
                min_score_lift: Some(0.10),
                min_gap_recovery: Some(0.25),
                max_regression_rate: Some(0.0),
                max_context_delta_tokens: Some(120),
                min_win_rate: Some(0.5),
                ..Default::default()
            },
            grader: SkillGateGrader {
                id: "immutable".to_string(),
                immutable_paths: vec![SkillGateProtectedPath {
                    path: "grader/check.txt".to_string(),
                    sha256: grader_hash,
                    label: Some("grader".to_string()),
                }],
                ..Default::default()
            },
            tasks: vec![
                SkillGateTask {
                    id: "post-cutoff-failure".to_string(),
                    cluster: "api-drift".to_string(),
                    heldout: SkillGateHeldout {
                        kind: "post_cutoff".to_string(),
                        created_at: Some("2026-05-20".to_string()),
                        ..Default::default()
                    },
                    baseline_score: 0.20,
                    frontier_score: 1.0,
                    baseline_passed: Some(false),
                    ..Default::default()
                },
                SkillGateTask {
                    id: "private-regression-check".to_string(),
                    cluster: "regression".to_string(),
                    heldout: SkillGateHeldout {
                        kind: "private".to_string(),
                        private: true,
                        ..Default::default()
                    },
                    baseline_score: 0.90,
                    frontier_score: 1.0,
                    baseline_passed: Some(true),
                    ..Default::default()
                },
                SkillGateTask {
                    id: "old-public-benchmark".to_string(),
                    cluster: "contaminated".to_string(),
                    heldout: SkillGateHeldout {
                        kind: "public_static".to_string(),
                        created_at: Some("2024-01-01".to_string()),
                        ..Default::default()
                    },
                    baseline_score: 0.0,
                    frontier_score: 1.0,
                    baseline_passed: Some(false),
                    ..Default::default()
                },
            ],
            variants: vec![
                SkillGateVariant {
                    id: "known-good".to_string(),
                    candidate: SkillGateArtifact {
                        kind: "skill".to_string(),
                        paths: vec!["skills/good/SKILL.md".to_string()],
                        ..Default::default()
                    },
                    case_results: vec![
                        SkillGateCaseResult {
                            task_id: "post-cutoff-failure".to_string(),
                            score: Some(0.80),
                            passed: Some(true),
                            ..Default::default()
                        },
                        SkillGateCaseResult {
                            task_id: "private-regression-check".to_string(),
                            score: Some(0.92),
                            passed: Some(true),
                            ..Default::default()
                        },
                        SkillGateCaseResult {
                            task_id: "old-public-benchmark".to_string(),
                            score: Some(1.0),
                            passed: Some(true),
                            ..Default::default()
                        },
                    ],
                    ..Default::default()
                },
                SkillGateVariant {
                    id: "bloated".to_string(),
                    candidate: SkillGateArtifact {
                        kind: "skill".to_string(),
                        paths: vec!["skills/bloat/SKILL.md".to_string()],
                        ..Default::default()
                    },
                    case_results: vec![
                        SkillGateCaseResult {
                            task_id: "post-cutoff-failure".to_string(),
                            score: Some(0.85),
                            passed: Some(true),
                            ..Default::default()
                        },
                        SkillGateCaseResult {
                            task_id: "private-regression-check".to_string(),
                            score: Some(0.91),
                            passed: Some(true),
                            ..Default::default()
                        },
                    ],
                    ..Default::default()
                },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn gate_accepts_compact_lift_rejects_bloat_and_excludes_contamination() {
        let temp = tempfile::tempdir().unwrap();
        write(
            temp.path().join("grader/check.txt").as_path(),
            "stable grader\n",
        );
        write(
            temp.path().join("skills/good/SKILL.md").as_path(),
            "Use the post-cutoff API name and keep the answer scoped.\n",
        );
        write(
            temp.path().join("skills/bloat/SKILL.md").as_path(),
            &"repeat this irrelevant guidance for token bloat.\n".repeat(80),
        );
        let grader_hash = sha256_file(&temp.path().join("grader/check.txt"))
            .unwrap()
            .sha256;
        let report = evaluate_skill_gate_manifest(&fixture_manifest(temp.path(), grader_hash))
            .expect("gate evaluates");

        assert!(report.pass);
        assert_eq!(report.selected_variant_id.as_deref(), Some("known-good"));
        assert_eq!(report.included_task_count, 2);
        assert_eq!(report.excluded_task_count, 1);
        assert_eq!(
            report.receipt.excluded_task_ids,
            vec!["old-public-benchmark"]
        );
        let good = report
            .variants
            .iter()
            .find(|variant| variant.id == "known-good")
            .unwrap();
        assert!(good.accepted);
        assert!(good.metrics.mean_gap_recovery > 0.25);
        assert_eq!(good.metrics.regression_rate, 0.0);
        let bloat = report
            .variants
            .iter()
            .find(|variant| variant.id == "bloated")
            .unwrap();
        assert!(!bloat.accepted);
        assert!(bloat
            .failures
            .iter()
            .any(|failure| failure.contains("context delta")));
    }

    #[test]
    fn gate_fails_when_immutable_grader_checksum_changes() {
        let temp = tempfile::tempdir().unwrap();
        write(
            temp.path().join("grader/check.txt").as_path(),
            "stable grader\n",
        );
        write(
            temp.path().join("skills/good/SKILL.md").as_path(),
            "Use the post-cutoff API name and keep the answer scoped.\n",
        );
        write(
            temp.path().join("skills/bloat/SKILL.md").as_path(),
            &"repeat this irrelevant guidance for token bloat.\n".repeat(80),
        );
        let mut manifest = fixture_manifest(temp.path(), "not-the-real-hash".to_string());
        manifest.variants.truncate(1);
        let report = evaluate_skill_gate_manifest(&manifest).expect("gate evaluates");

        assert!(!report.pass);
        assert!(!report.tamper.pass);
        assert!(report
            .variants
            .first()
            .unwrap()
            .failures
            .iter()
            .any(|failure| failure.contains("immutable grader")));
    }
}
