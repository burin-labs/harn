//! Agent-iteration harness for Merge Captain eval sweeps (#1021).
//!
//! This layer deliberately composes the existing mock/replay driver and oracle:
//! scenarios define the deterministic backend, variants define model plus Harn
//! package/prompt-asset revision metadata, and each matrix cell writes the
//! normal transcript/receipt/summary artifacts under one shareable iteration
//! directory.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::value::VmError;

use super::{
    load_merge_captain_golden, new_id, MergeCaptainDriverBackend, MergeCaptainDriverMode,
    MergeCaptainDriverOptions, MergeCaptainRunSummary,
};

const MANIFEST_TYPE: &str = "merge_captain_iteration_manifest";
const REPORT_TYPE: &str = "merge_captain_iteration_report";
const DIFF_TYPE: &str = "merge_captain_iteration_diff";

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct MergeCaptainIterationManifest {
    #[serde(rename = "_type")]
    pub type_name: String,
    pub version: u32,
    pub id: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub base_dir: Option<String>,
    #[serde(alias = "artifact-root")]
    pub artifact_root: Option<String>,
    pub scenarios: Vec<MergeCaptainIterationScenario>,
    pub variants: Vec<MergeCaptainIterationVariant>,
    pub budget: MergeCaptainIterationBudget,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct MergeCaptainIterationScenario {
    pub id: String,
    pub description: Option<String>,
    pub backend: MergeCaptainIterationBackendSpec,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct MergeCaptainIterationBackendSpec {
    pub kind: String,
    pub path: Option<String>,
    pub scenario: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct MergeCaptainIterationVariant {
    pub id: String,
    #[serde(alias = "model-route")]
    pub model_route: Option<String>,
    #[serde(alias = "timeout-tier")]
    pub timeout_tier: Option<String>,
    #[serde(alias = "package-revision")]
    pub package_revision: Option<String>,
    #[serde(alias = "prompt-asset-revision")]
    pub prompt_asset_revision: Option<String>,
    #[serde(alias = "max-cost-usd")]
    pub max_cost_usd: Option<f64>,
    #[serde(alias = "max-model-calls")]
    pub max_model_calls: Option<u64>,
    #[serde(alias = "max-tool-calls")]
    pub max_tool_calls: Option<u64>,
    #[serde(alias = "max-latency-ms")]
    pub max_latency_ms: Option<u64>,
    #[serde(alias = "timeout-ms")]
    pub timeout_ms: Option<u64>,
    #[serde(alias = "max-sweeps")]
    pub max_sweeps: Option<u32>,
    #[serde(alias = "watch-backoff-ms")]
    pub watch_backoff_ms: Option<u64>,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct MergeCaptainIterationBudget {
    #[serde(alias = "max-cost-usd")]
    pub max_cost_usd: Option<f64>,
    #[serde(alias = "max-wallclock-ms")]
    pub max_wallclock_ms: Option<u64>,
    #[serde(alias = "max-runs")]
    pub max_runs: Option<usize>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct MergeCaptainIterationReport {
    #[serde(rename = "_type")]
    pub type_name: String,
    pub version: u32,
    pub id: String,
    pub name: Option<String>,
    pub artifact_root: String,
    pub summary_json_path: String,
    pub summary_markdown_path: String,
    pub pass: bool,
    pub total: usize,
    pub completed: usize,
    pub skipped: usize,
    pub budget_exhausted: bool,
    pub budget_exhausted_reason: Option<String>,
    pub total_cost_usd: f64,
    pub total_latency_ms: u64,
    pub runs: Vec<MergeCaptainIterationRunReport>,
    pub rankings: Vec<MergeCaptainIterationRanking>,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct MergeCaptainIterationRunReport {
    pub id: String,
    pub scenario_id: String,
    pub variant_id: String,
    pub backend: String,
    pub backend_source: Option<String>,
    pub model_route: Option<String>,
    pub timeout_tier: Option<String>,
    pub package_revision: Option<String>,
    pub prompt_asset_revision: Option<String>,
    pub pass: bool,
    pub skipped: bool,
    pub skip_reason: Option<String>,
    pub drift_score: u64,
    pub degradation_reasons: Vec<String>,
    pub transcript_path: Option<String>,
    pub receipt_path: Option<String>,
    pub summary_path: Option<String>,
    pub oracle_error_findings: usize,
    pub oracle_warn_findings: usize,
    pub cost_usd: f64,
    pub latency_ms: u64,
    pub tool_calls: u64,
    pub model_calls: u64,
    pub event_count: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct MergeCaptainIterationRanking {
    pub variant_id: String,
    pub package_revision: Option<String>,
    pub prompt_asset_revision: Option<String>,
    pub scenarios_completed: usize,
    pub scenarios_passed: usize,
    pub skipped: usize,
    pub drift_score: u64,
    pub cost_usd: f64,
    pub latency_ms: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct MergeCaptainIterationDiffReport {
    #[serde(rename = "_type")]
    pub type_name: String,
    pub version: u32,
    pub baseline_id: String,
    pub candidate_id: String,
    pub baseline_path: String,
    pub candidate_path: String,
    pub improved: usize,
    pub regressed: usize,
    pub unchanged: usize,
    pub missing: usize,
    pub entries: Vec<MergeCaptainIterationDiffEntry>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct MergeCaptainIterationDiffEntry {
    pub scenario_id: String,
    pub variant_id: String,
    pub baseline_drift_score: Option<u64>,
    pub candidate_drift_score: Option<u64>,
    pub delta: Option<i64>,
    pub status: String,
    pub baseline_pass: Option<bool>,
    pub candidate_pass: Option<bool>,
    pub baseline_prompt_asset_revision: Option<String>,
    pub candidate_prompt_asset_revision: Option<String>,
}

pub fn load_merge_captain_iteration_manifest(
    path: &Path,
) -> Result<MergeCaptainIterationManifest, VmError> {
    let content = fs::read_to_string(path).map_err(|error| {
        VmError::Runtime(format!(
            "failed to read merge-captain iteration manifest {}: {error}",
            path.display()
        ))
    })?;
    let mut manifest: MergeCaptainIterationManifest =
        if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
            serde_json::from_str(&content).map_err(|error| {
                VmError::Runtime(format!(
                    "failed to parse merge-captain iteration JSON {}: {error}",
                    path.display()
                ))
            })?
        } else {
            toml::from_str(&content).map_err(|error| {
                VmError::Runtime(format!(
                    "failed to parse merge-captain iteration TOML {}: {error}",
                    path.display()
                ))
            })?
        };
    normalize_merge_captain_iteration_manifest(&mut manifest);
    if manifest.base_dir.is_none() {
        manifest.base_dir = path.parent().map(|parent| parent.display().to_string());
    }
    Ok(manifest)
}

pub fn normalize_merge_captain_iteration_manifest(manifest: &mut MergeCaptainIterationManifest) {
    if manifest.type_name.is_empty() {
        manifest.type_name = MANIFEST_TYPE.to_string();
    }
    if manifest.version == 0 {
        manifest.version = 1;
    }
    if manifest.id.trim().is_empty() {
        manifest.id = manifest
            .name
            .clone()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| new_id("merge_captain_iteration"));
    }
    for (index, scenario) in manifest.scenarios.iter_mut().enumerate() {
        if scenario.id.trim().is_empty() {
            scenario.id = scenario
                .backend
                .scenario
                .clone()
                .or_else(|| {
                    scenario
                        .backend
                        .path
                        .as_deref()
                        .and_then(|path| Path::new(path).file_stem())
                        .and_then(|stem| stem.to_str())
                        .map(str::to_string)
                })
                .unwrap_or_else(|| format!("scenario_{}", index + 1));
        }
        if scenario.backend.kind.trim().is_empty() {
            scenario.backend.kind = "replay".to_string();
        }
    }
    if manifest.variants.is_empty() {
        manifest.variants.push(MergeCaptainIterationVariant {
            id: "default".to_string(),
            ..Default::default()
        });
    }
    for (index, variant) in manifest.variants.iter_mut().enumerate() {
        if variant.id.trim().is_empty() {
            variant.id = format!("variant_{}", index + 1);
        }
    }
}

pub fn run_merge_captain_iteration(
    manifest: &MergeCaptainIterationManifest,
) -> Result<MergeCaptainIterationReport, VmError> {
    let mut manifest = manifest.clone();
    normalize_merge_captain_iteration_manifest(&mut manifest);
    if manifest.scenarios.is_empty() {
        return Err(VmError::Runtime(format!(
            "merge-captain iteration '{}' must declare at least one scenario",
            manifest.id
        )));
    }

    let base_dir = manifest.base_dir.as_deref().map(Path::new);
    let artifact_root = resolve_artifact_root(&manifest, base_dir);
    fs::create_dir_all(&artifact_root).map_err(|error| {
        VmError::Runtime(format!(
            "failed to create merge-captain iteration artifact root {}: {error}",
            artifact_root.display()
        ))
    })?;
    write_json_file(&artifact_root.join("iteration.json"), &manifest)?;

    let total = manifest.scenarios.len() * manifest.variants.len();
    let started = Instant::now();
    let mut total_cost_usd = 0.0;
    let mut total_latency_ms: u64 = 0;
    let mut completed = 0;
    let mut budget_exhausted_reason = None;
    let mut runs = Vec::new();

    for scenario in &manifest.scenarios {
        for variant in &manifest.variants {
            if budget_exhausted_reason.is_none() {
                budget_exhausted_reason =
                    budget_exhausted(&manifest.budget, completed, total_cost_usd, started);
            }
            if let Some(reason) = &budget_exhausted_reason {
                runs.push(skipped_run_report(
                    scenario,
                    variant,
                    &artifact_root,
                    reason.clone(),
                ));
                continue;
            }

            let run = run_iteration_cell(&artifact_root, base_dir, scenario, variant)?;
            if !run.skipped {
                completed += 1;
                total_cost_usd += run.cost_usd;
                total_latency_ms = total_latency_ms.saturating_add(run.latency_ms);
            }
            runs.push(run);
        }
    }

    let rankings = rank_variants(&manifest.variants, &runs);
    let skipped = runs.iter().filter(|run| run.skipped).count();
    let best_drift = rankings.first().map(|ranking| ranking.drift_score);
    let pass = !runs.is_empty()
        && budget_exhausted_reason.is_none()
        && best_drift == Some(0)
        && rankings
            .first()
            .is_some_and(|ranking| ranking.scenarios_completed == manifest.scenarios.len());
    let summary_json_path = artifact_root.join("summary.json");
    let summary_markdown_path = artifact_root.join("summary.md");
    let mut report = MergeCaptainIterationReport {
        type_name: REPORT_TYPE.to_string(),
        version: 1,
        id: manifest.id,
        name: manifest.name,
        artifact_root: artifact_root.display().to_string(),
        summary_json_path: summary_json_path.display().to_string(),
        summary_markdown_path: summary_markdown_path.display().to_string(),
        pass,
        total,
        completed,
        skipped,
        budget_exhausted: budget_exhausted_reason.is_some(),
        budget_exhausted_reason,
        total_cost_usd,
        total_latency_ms,
        runs,
        rankings,
        metadata: manifest.metadata,
    };
    write_json_file(&summary_json_path, &report)?;
    let markdown = render_iteration_markdown(&report);
    write_text_file(&summary_markdown_path, &markdown)?;
    report.summary_json_path = summary_json_path.display().to_string();
    report.summary_markdown_path = summary_markdown_path.display().to_string();
    Ok(report)
}

fn run_iteration_cell(
    artifact_root: &Path,
    base_dir: Option<&Path>,
    scenario: &MergeCaptainIterationScenario,
    variant: &MergeCaptainIterationVariant,
) -> Result<MergeCaptainIterationRunReport, VmError> {
    let cell_dir = artifact_root
        .join("runs")
        .join(safe_path_segment(&scenario.id))
        .join(safe_path_segment(&variant.id));
    fs::create_dir_all(&cell_dir).map_err(|error| {
        VmError::Runtime(format!(
            "failed to create iteration run dir {}: {error}",
            cell_dir.display()
        ))
    })?;
    let backend = resolve_iteration_backend(artifact_root, base_dir, scenario, variant)?;
    let transcript_path = cell_dir.join("event_log.jsonl");
    let receipt_path = cell_dir.join("receipt.json");
    let summary_path = cell_dir.join("summary.json");
    let max_sweeps = variant.max_sweeps.unwrap_or(1).max(1);
    let output = super::run_merge_captain_driver(MergeCaptainDriverOptions {
        backend: backend.clone(),
        mode: if max_sweeps > 1 {
            MergeCaptainDriverMode::Watch
        } else {
            MergeCaptainDriverMode::Once
        },
        model_route: variant
            .model_route
            .clone()
            .or_else(|| Some(variant.id.clone())),
        timeout_tier: variant.timeout_tier.clone(),
        transcript_out: Some(transcript_path.clone()),
        receipt_out: Some(receipt_path.clone()),
        run_root: cell_dir.join("driver-runs"),
        max_sweeps,
        watch_backoff_ms: variant.watch_backoff_ms.unwrap_or(0),
        stream_stdout: false,
    })?;
    write_json_file(&summary_path, &output.summary)?;

    let degradation_reasons = degradation_reasons(&output.summary, variant);
    let drift_score = drift_score(&output.summary, &degradation_reasons);
    let pass = output.summary.pass && degradation_reasons.is_empty();
    let report = MergeCaptainIterationRunReport {
        id: format!("{}::{}", scenario.id, variant.id),
        scenario_id: scenario.id.clone(),
        variant_id: variant.id.clone(),
        backend: backend.kind().to_string(),
        backend_source: output.summary.backend_source.clone(),
        model_route: output.summary.model_route.clone(),
        timeout_tier: output.summary.timeout_tier.clone(),
        package_revision: variant.package_revision.clone(),
        prompt_asset_revision: variant.prompt_asset_revision.clone(),
        pass,
        skipped: false,
        skip_reason: None,
        drift_score,
        degradation_reasons,
        transcript_path: Some(relative_display(artifact_root, &transcript_path)),
        receipt_path: Some(relative_display(artifact_root, &receipt_path)),
        summary_path: Some(relative_display(artifact_root, &summary_path)),
        oracle_error_findings: output.summary.oracle_error_findings,
        oracle_warn_findings: output.summary.oracle_warn_findings,
        cost_usd: output.summary.cost_usd,
        latency_ms: output.summary.latency_ms,
        tool_calls: output.summary.tool_calls,
        model_calls: output.summary.model_calls,
        event_count: output.summary.event_count,
    };
    write_json_file(&cell_dir.join("run-report.json"), &report)?;
    Ok(report)
}

fn resolve_iteration_backend(
    artifact_root: &Path,
    base_dir: Option<&Path>,
    scenario: &MergeCaptainIterationScenario,
    variant: &MergeCaptainIterationVariant,
) -> Result<MergeCaptainDriverBackend, VmError> {
    match scenario.backend.kind.trim().to_ascii_lowercase().as_str() {
        "replay" => {
            let path = scenario.backend.path.as_deref().ok_or_else(|| {
                VmError::Runtime(format!(
                    "iteration scenario '{}' replay backend requires path",
                    scenario.id
                ))
            })?;
            let source = resolve_manifest_path(base_dir, path);
            Ok(MergeCaptainDriverBackend::Replay {
                fixture: copy_replay_fixture(artifact_root, &scenario.id, &source)?,
            })
        }
        "mock" => {
            let playground_dir = artifact_root
                .join("playgrounds")
                .join(safe_path_segment(&scenario.id))
                .join(safe_path_segment(&variant.id));
            let manifest = if let Some(name) = scenario.backend.scenario.as_deref() {
                Some(super::playground::load_builtin(name)?)
            } else if let Some(path) = scenario.backend.path.as_deref() {
                let source = resolve_manifest_path(base_dir, path);
                if super::playground::playground_marker_path(&source).exists() {
                    return Ok(MergeCaptainDriverBackend::Mock {
                        playground_dir: source,
                    });
                }
                super::playground::ScenarioManifest::load(&source).ok()
            } else {
                Some(super::playground::load_builtin(&scenario.id)?)
            };
            if let Some(manifest) = manifest {
                let _ = super::playground::cleanup_playground_at(&playground_dir)?;
                super::playground::init_playground_at(super::playground::InitOptions {
                    dir: &playground_dir,
                    manifest: &manifest,
                    allow_existing: false,
                })?;
                Ok(MergeCaptainDriverBackend::Mock { playground_dir })
            } else {
                let path = scenario.backend.path.as_deref().ok_or_else(|| {
                    VmError::Runtime(format!(
                        "iteration scenario '{}' mock backend requires path or scenario",
                        scenario.id
                    ))
                })?;
                Ok(MergeCaptainDriverBackend::Mock {
                    playground_dir: resolve_manifest_path(base_dir, path),
                })
            }
        }
        "live" => Ok(MergeCaptainDriverBackend::Live),
        other => Err(VmError::Runtime(format!(
            "unsupported merge-captain iteration backend '{}'",
            other
        ))),
    }
}

fn copy_replay_fixture(
    artifact_root: &Path,
    scenario_id: &str,
    source: &Path,
) -> Result<PathBuf, VmError> {
    let stem = source
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("event_log");
    let dest_dir = artifact_root
        .join("fixtures")
        .join(safe_path_segment(scenario_id))
        .join("transcripts");
    fs::create_dir_all(&dest_dir).map_err(|error| {
        VmError::Runtime(format!(
            "failed to create replay fixture dir {}: {error}",
            dest_dir.display()
        ))
    })?;
    let dest = dest_dir.join(format!("{stem}.jsonl"));
    fs::copy(source, &dest).map_err(|error| {
        VmError::Runtime(format!(
            "failed to copy replay fixture {} to {}: {error}",
            source.display(),
            dest.display()
        ))
    })?;
    if let Some(golden) = find_replay_golden(source)? {
        let golden_dir = artifact_root
            .join("fixtures")
            .join(safe_path_segment(scenario_id))
            .join("goldens");
        fs::create_dir_all(&golden_dir).map_err(|error| {
            VmError::Runtime(format!(
                "failed to create replay golden dir {}: {error}",
                golden_dir.display()
            ))
        })?;
        let golden_dest = golden_dir.join(format!("{stem}.json"));
        fs::copy(&golden, &golden_dest).map_err(|error| {
            VmError::Runtime(format!(
                "failed to copy replay golden {} to {}: {error}",
                golden.display(),
                golden_dest.display()
            ))
        })?;
    }
    Ok(dest)
}

fn find_replay_golden(source: &Path) -> Result<Option<PathBuf>, VmError> {
    let Some(stem) = source.file_stem().and_then(|stem| stem.to_str()) else {
        return Ok(None);
    };
    let mut candidates = Vec::new();
    if let Some(parent) = source.parent() {
        candidates.push(parent.join(format!("{stem}.golden.json")));
        if parent.file_name().and_then(|name| name.to_str()) == Some("transcripts") {
            if let Some(root) = parent.parent() {
                candidates.push(root.join("goldens").join(format!("{stem}.json")));
            }
        }
    }
    for candidate in candidates {
        if candidate.exists() {
            let _ = load_merge_captain_golden(&candidate)?;
            return Ok(Some(candidate));
        }
    }
    Ok(None)
}

fn skipped_run_report(
    scenario: &MergeCaptainIterationScenario,
    variant: &MergeCaptainIterationVariant,
    _artifact_root: &Path,
    reason: String,
) -> MergeCaptainIterationRunReport {
    MergeCaptainIterationRunReport {
        id: format!("{}::{}", scenario.id, variant.id),
        scenario_id: scenario.id.clone(),
        variant_id: variant.id.clone(),
        model_route: variant.model_route.clone(),
        timeout_tier: variant.timeout_tier.clone(),
        package_revision: variant.package_revision.clone(),
        prompt_asset_revision: variant.prompt_asset_revision.clone(),
        skipped: true,
        skip_reason: Some(reason),
        drift_score: 10_000,
        ..Default::default()
    }
}

fn budget_exhausted(
    budget: &MergeCaptainIterationBudget,
    completed: usize,
    total_cost_usd: f64,
    started: Instant,
) -> Option<String> {
    if let Some(max_runs) = budget.max_runs {
        if completed >= max_runs {
            return Some(format!("completed run cap {max_runs} reached"));
        }
    }
    if let Some(max_cost_usd) = budget.max_cost_usd {
        if total_cost_usd > max_cost_usd {
            return Some(format!(
                "cost budget ${:.6} reached (spent ${:.6})",
                max_cost_usd, total_cost_usd
            ));
        }
    }
    if let Some(max_wallclock_ms) = budget.max_wallclock_ms {
        if started.elapsed().as_millis() >= u128::from(max_wallclock_ms) {
            return Some(format!("wallclock budget {max_wallclock_ms}ms reached"));
        }
    }
    None
}

fn degradation_reasons(
    summary: &MergeCaptainRunSummary,
    variant: &MergeCaptainIterationVariant,
) -> Vec<String> {
    let mut reasons = Vec::new();
    if !summary.pass {
        reasons.push(format!(
            "oracle reported {} error finding(s) and {} warning finding(s)",
            summary.oracle_error_findings, summary.oracle_warn_findings
        ));
    }
    if let Some(max_tool_calls) = variant.max_tool_calls {
        if summary.tool_calls > max_tool_calls {
            reasons.push(format!(
                "tool calls {} exceeded variant budget {}",
                summary.tool_calls, max_tool_calls
            ));
        }
    }
    if let Some(max_model_calls) = variant.max_model_calls {
        if summary.model_calls > max_model_calls {
            reasons.push(format!(
                "model calls {} exceeded variant budget {}",
                summary.model_calls, max_model_calls
            ));
        }
    }
    if let Some(max_cost_usd) = variant.max_cost_usd {
        if summary.cost_usd > max_cost_usd {
            reasons.push(format!(
                "cost ${:.6} exceeded variant budget ${:.6}",
                summary.cost_usd, max_cost_usd
            ));
        }
    }
    if let Some(max_latency_ms) = variant.max_latency_ms.or(variant.timeout_ms) {
        if summary.latency_ms > max_latency_ms {
            reasons.push(format!(
                "latency {}ms exceeded variant timeout {}ms",
                summary.latency_ms, max_latency_ms
            ));
        }
    }
    reasons
}

fn drift_score(summary: &MergeCaptainRunSummary, degradation_reasons: &[String]) -> u64 {
    let failed_penalty = if summary.pass { 0 } else { 1_000 };
    failed_penalty
        + (summary.oracle_error_findings as u64 * 100)
        + (summary.oracle_warn_findings as u64 * 10)
        + (degradation_reasons.len() as u64 * 25)
}

fn rank_variants(
    variants: &[MergeCaptainIterationVariant],
    runs: &[MergeCaptainIterationRunReport],
) -> Vec<MergeCaptainIterationRanking> {
    let mut rankings = Vec::new();
    for variant in variants {
        let matching: Vec<_> = runs
            .iter()
            .filter(|run| run.variant_id == variant.id)
            .collect();
        rankings.push(MergeCaptainIterationRanking {
            variant_id: variant.id.clone(),
            package_revision: variant.package_revision.clone(),
            prompt_asset_revision: variant.prompt_asset_revision.clone(),
            scenarios_completed: matching.iter().filter(|run| !run.skipped).count(),
            scenarios_passed: matching.iter().filter(|run| run.pass).count(),
            skipped: matching.iter().filter(|run| run.skipped).count(),
            drift_score: matching.iter().map(|run| run.drift_score).sum(),
            cost_usd: matching.iter().map(|run| run.cost_usd).sum(),
            latency_ms: matching.iter().map(|run| run.latency_ms).sum(),
        });
    }
    rankings.sort_by(|left, right| {
        left.drift_score
            .cmp(&right.drift_score)
            .then_with(|| {
                left.cost_usd
                    .partial_cmp(&right.cost_usd)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| left.variant_id.cmp(&right.variant_id))
    });
    rankings
}

pub fn load_merge_captain_iteration_report(
    path: &Path,
) -> Result<MergeCaptainIterationReport, VmError> {
    let report_path = if path.is_dir() {
        path.join("summary.json")
    } else {
        path.to_path_buf()
    };
    let bytes = fs::read(&report_path).map_err(|error| {
        VmError::Runtime(format!(
            "failed to read merge-captain iteration report {}: {error}",
            report_path.display()
        ))
    })?;
    serde_json::from_slice(&bytes).map_err(|error| {
        VmError::Runtime(format!(
            "failed to parse merge-captain iteration report {}: {error}",
            report_path.display()
        ))
    })
}

pub fn diff_merge_captain_iterations(
    baseline_path: &Path,
    candidate_path: &Path,
) -> Result<MergeCaptainIterationDiffReport, VmError> {
    let baseline = load_merge_captain_iteration_report(baseline_path)?;
    let candidate = load_merge_captain_iteration_report(candidate_path)?;
    let mut keys = BTreeMap::new();
    for run in &baseline.runs {
        keys.insert((run.scenario_id.clone(), run.variant_id.clone()), ());
    }
    for run in &candidate.runs {
        keys.insert((run.scenario_id.clone(), run.variant_id.clone()), ());
    }

    let mut entries = Vec::new();
    let mut improved = 0;
    let mut regressed = 0;
    let mut unchanged = 0;
    let mut missing = 0;
    for ((scenario_id, variant_id), ()) in keys {
        let before = baseline
            .runs
            .iter()
            .find(|run| run.scenario_id == scenario_id && run.variant_id == variant_id);
        let after = candidate
            .runs
            .iter()
            .find(|run| run.scenario_id == scenario_id && run.variant_id == variant_id);
        let delta = before
            .zip(after)
            .map(|(before, after)| after.drift_score as i64 - before.drift_score as i64);
        let status = match delta {
            Some(value) if value < 0 => {
                improved += 1;
                "improved"
            }
            Some(value) if value > 0 => {
                regressed += 1;
                "regressed"
            }
            Some(_) => {
                unchanged += 1;
                "unchanged"
            }
            None => {
                missing += 1;
                "missing"
            }
        };
        entries.push(MergeCaptainIterationDiffEntry {
            scenario_id,
            variant_id,
            baseline_drift_score: before.map(|run| run.drift_score),
            candidate_drift_score: after.map(|run| run.drift_score),
            delta,
            status: status.to_string(),
            baseline_pass: before.map(|run| run.pass),
            candidate_pass: after.map(|run| run.pass),
            baseline_prompt_asset_revision: before
                .and_then(|run| run.prompt_asset_revision.clone()),
            candidate_prompt_asset_revision: after
                .and_then(|run| run.prompt_asset_revision.clone()),
        });
    }

    Ok(MergeCaptainIterationDiffReport {
        type_name: DIFF_TYPE.to_string(),
        version: 1,
        baseline_id: baseline.id,
        candidate_id: candidate.id,
        baseline_path: baseline_path.display().to_string(),
        candidate_path: candidate_path.display().to_string(),
        improved,
        regressed,
        unchanged,
        missing,
        entries,
    })
}

pub fn render_iteration_markdown(report: &MergeCaptainIterationReport) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# Merge Captain iteration: {}\n\n",
        report.name.as_deref().unwrap_or(&report.id)
    ));
    out.push_str(&format!(
        "- pass: {}\n- completed: {}/{}\n- skipped: {}\n- budget_exhausted: {}\n\n",
        report.pass, report.completed, report.total, report.skipped, report.budget_exhausted
    ));
    out.push_str("## Variant ranking\n\n");
    out.push_str(
        "| rank | variant | package | prompt assets | passed | drift | cost | latency ms |\n",
    );
    out.push_str("|---:|---|---|---|---:|---:|---:|---:|\n");
    for (index, ranking) in report.rankings.iter().enumerate() {
        out.push_str(&format!(
            "| {} | {} | {} | {} | {}/{} | {} | {:.6} | {} |\n",
            index + 1,
            ranking.variant_id,
            ranking.package_revision.as_deref().unwrap_or("-"),
            ranking.prompt_asset_revision.as_deref().unwrap_or("-"),
            ranking.scenarios_passed,
            ranking.scenarios_completed,
            ranking.drift_score,
            ranking.cost_usd,
            ranking.latency_ms
        ));
    }
    out.push_str("\n## Scenario runs\n\n");
    out.push_str(
        "| scenario | variant | pass | drift | errors | warnings | tools | models | artifact |\n",
    );
    out.push_str("|---|---|---:|---:|---:|---:|---:|---:|---|\n");
    for run in &report.runs {
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            run.scenario_id,
            run.variant_id,
            if run.skipped {
                "skipped".to_string()
            } else {
                run.pass.to_string()
            },
            run.drift_score,
            run.oracle_error_findings,
            run.oracle_warn_findings,
            run.tool_calls,
            run.model_calls,
            run.summary_path.as_deref().unwrap_or("-")
        ));
    }
    out
}

pub fn render_iteration_diff_markdown(report: &MergeCaptainIterationDiffReport) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# Merge Captain iteration diff: {} -> {}\n\n",
        report.baseline_id, report.candidate_id
    ));
    out.push_str(&format!(
        "- improved: {}\n- regressed: {}\n- unchanged: {}\n- missing: {}\n\n",
        report.improved, report.regressed, report.unchanged, report.missing
    ));
    out.push_str(
        "| scenario | variant | baseline | candidate | delta | status | prompt assets |\n",
    );
    out.push_str("|---|---|---:|---:|---:|---|---|\n");
    for entry in &report.entries {
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} -> {} |\n",
            entry.scenario_id,
            entry.variant_id,
            optional_u64(entry.baseline_drift_score),
            optional_u64(entry.candidate_drift_score),
            entry
                .delta
                .map(|delta| delta.to_string())
                .unwrap_or_else(|| "-".to_string()),
            entry.status,
            entry
                .baseline_prompt_asset_revision
                .as_deref()
                .unwrap_or("-"),
            entry
                .candidate_prompt_asset_revision
                .as_deref()
                .unwrap_or("-")
        ));
    }
    out
}

fn optional_u64(value: Option<u64>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "-".to_string())
}

fn resolve_artifact_root(
    manifest: &MergeCaptainIterationManifest,
    base_dir: Option<&Path>,
) -> PathBuf {
    let root = manifest
        .artifact_root
        .clone()
        .unwrap_or_else(|| format!(".harn-runs/merge-captain-iterations/{}", manifest.id));
    let resolved = resolve_manifest_path(base_dir, &root);
    if resolved.is_absolute() {
        resolved
    } else {
        let relative = resolved.strip_prefix(".").unwrap_or(&resolved);
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(relative)
    }
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

fn relative_display(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| path.display().to_string())
}

fn safe_path_segment(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "unnamed".to_string()
    } else {
        out
    }
}

fn write_json_file<T: Serialize>(path: &Path, value: &T) -> Result<(), VmError> {
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| VmError::Runtime(format!("failed to serialize JSON artifact: {error}")))?;
    bytes.push(b'\n');
    write_bytes_file(path, &bytes)
}

fn write_text_file(path: &Path, value: &str) -> Result<(), VmError> {
    write_bytes_file(path, value.as_bytes())
}

fn write_bytes_file(path: &Path, bytes: &[u8]) -> Result<(), VmError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            VmError::Runtime(format!(
                "failed to create artifact directory {}: {error}",
                parent.display()
            ))
        })?;
    }
    fs::write(path, bytes).map_err(|error| {
        VmError::Runtime(format!(
            "failed to write artifact {}: {error}",
            path.display()
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::load_transcript_jsonl;

    fn repo_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf()
    }

    #[test]
    fn iteration_runs_matrix_and_ranks_by_drift() {
        let temp = tempfile::tempdir().unwrap();
        let manifest = MergeCaptainIterationManifest {
            id: "issue-1021-smoke".to_string(),
            base_dir: Some(repo_root().display().to_string()),
            artifact_root: Some(temp.path().join("iteration").display().to_string()),
            scenarios: vec![MergeCaptainIterationScenario {
                id: "green-pr".to_string(),
                backend: MergeCaptainIterationBackendSpec {
                    kind: "replay".to_string(),
                    path: Some(
                        "examples/personas/merge_captain/transcripts/green_pr.jsonl".to_string(),
                    ),
                    ..Default::default()
                },
                ..Default::default()
            }],
            variants: vec![
                MergeCaptainIterationVariant {
                    id: "prompt-v1".to_string(),
                    prompt_asset_revision: Some("prompt/v1".to_string()),
                    max_tool_calls: Some(1),
                    ..Default::default()
                },
                MergeCaptainIterationVariant {
                    id: "prompt-v2".to_string(),
                    prompt_asset_revision: Some("prompt/v2".to_string()),
                    max_tool_calls: Some(4),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        let report = run_merge_captain_iteration(&manifest).unwrap();

        assert!(report.pass);
        assert_eq!(report.completed, 2);
        assert_eq!(report.rankings[0].variant_id, "prompt-v2");
        assert_eq!(report.rankings[0].drift_score, 0);
        assert!(Path::new(&report.summary_markdown_path).exists());
        assert!(Path::new(&report.artifact_root)
            .join("fixtures/green-pr/transcripts/green_pr.jsonl")
            .exists());
    }

    #[test]
    fn iteration_budget_cap_skips_remaining_runs() {
        let temp = tempfile::tempdir().unwrap();
        let manifest = MergeCaptainIterationManifest {
            id: "issue-1021-budget".to_string(),
            base_dir: Some(repo_root().display().to_string()),
            artifact_root: Some(temp.path().join("iteration").display().to_string()),
            scenarios: vec![MergeCaptainIterationScenario {
                id: "green-pr".to_string(),
                backend: MergeCaptainIterationBackendSpec {
                    kind: "replay".to_string(),
                    path: Some(
                        "examples/personas/merge_captain/transcripts/green_pr.jsonl".to_string(),
                    ),
                    ..Default::default()
                },
                ..Default::default()
            }],
            variants: vec![
                MergeCaptainIterationVariant {
                    id: "one".to_string(),
                    ..Default::default()
                },
                MergeCaptainIterationVariant {
                    id: "two".to_string(),
                    ..Default::default()
                },
            ],
            budget: MergeCaptainIterationBudget {
                max_runs: Some(1),
                ..Default::default()
            },
            ..Default::default()
        };

        let report = run_merge_captain_iteration(&manifest).unwrap();

        assert!(report.budget_exhausted);
        assert_eq!(report.completed, 1);
        assert_eq!(report.skipped, 1);
        assert!(report.runs[1].skipped);
    }

    #[test]
    fn diff_marks_prompt_candidate_improvement() {
        let temp = tempfile::tempdir().unwrap();
        let baseline_path = temp.path().join("baseline.json");
        let candidate_path = temp.path().join("candidate.json");
        let mut baseline = MergeCaptainIterationReport {
            type_name: REPORT_TYPE.to_string(),
            id: "baseline".to_string(),
            runs: vec![MergeCaptainIterationRunReport {
                scenario_id: "green-pr".to_string(),
                variant_id: "value-route".to_string(),
                drift_score: 25,
                prompt_asset_revision: Some("prompt/v1".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        };
        baseline.version = 1;
        let mut candidate = baseline.clone();
        candidate.id = "candidate".to_string();
        candidate.runs[0].drift_score = 0;
        candidate.runs[0].prompt_asset_revision = Some("prompt/v2".to_string());
        write_json_file(&baseline_path, &baseline).unwrap();
        write_json_file(&candidate_path, &candidate).unwrap();

        let diff = diff_merge_captain_iterations(&baseline_path, &candidate_path).unwrap();

        assert_eq!(diff.improved, 1);
        assert_eq!(diff.entries[0].delta, Some(-25));
        assert_eq!(diff.entries[0].status, "improved");
    }

    #[test]
    fn mock_scenario_manifest_materializes_playground() {
        let temp = tempfile::tempdir().unwrap();
        let manifest = MergeCaptainIterationManifest {
            id: "issue-1021-mock".to_string(),
            base_dir: Some(repo_root().display().to_string()),
            artifact_root: Some(temp.path().join("iteration").display().to_string()),
            scenarios: vec![MergeCaptainIterationScenario {
                id: "single-green".to_string(),
                backend: MergeCaptainIterationBackendSpec {
                    kind: "mock".to_string(),
                    path: Some("examples/merge_captain/scenarios/single_green.json".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            }],
            variants: vec![MergeCaptainIterationVariant {
                id: "smoke".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        };

        let report = run_merge_captain_iteration(&manifest).unwrap();

        assert_eq!(report.completed, 1);
        assert!(Path::new(&report.artifact_root)
            .join("playgrounds/single-green/smoke/playground.json")
            .exists());
        let loaded = load_transcript_jsonl(
            &Path::new(&report.artifact_root)
                .join(report.runs[0].transcript_path.as_ref().unwrap()),
        )
        .unwrap();
        assert!(!loaded.events.is_empty());
    }
}
