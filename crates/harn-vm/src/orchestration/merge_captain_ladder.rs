//! Persona eval timeout/budget ladders, starting with Merge Captain.
//!
//! The ladder is intentionally an eval artifact runner rather than host
//! orchestration logic: every route/tier combination produces the same
//! transcript, receipt, and summary contracts as `harn merge-captain run`,
//! then an aggregate report marks the first configuration that completed
//! correctly and the configurations that degraded or looped.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::value::{VmError, VmValue};

use super::{
    new_id, parse_json_value, MergeCaptainDriverBackend, MergeCaptainDriverMode,
    MergeCaptainDriverOptions, MergeCaptainRunSummary, StateTransition,
};

const MANIFEST_TYPE: &str = "persona_eval_ladder_manifest";
const REPORT_TYPE: &str = "persona_eval_ladder_report";
const DEFAULT_PERSONA: &str = "merge_captain";

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct PersonaEvalLadderManifest {
    #[serde(rename = "_type")]
    pub type_name: String,
    pub version: u32,
    pub id: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub persona: String,
    pub base_dir: Option<String>,
    #[serde(alias = "artifact-root")]
    pub artifact_root: Option<String>,
    pub severity: Option<String>,
    pub backend: PersonaEvalLadderBackendSpec,
    #[serde(alias = "model-routes")]
    pub model_routes: Vec<PersonaEvalModelRoute>,
    #[serde(alias = "timeout-tiers")]
    pub timeout_tiers: Vec<PersonaEvalTimeoutTier>,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct PersonaEvalLadderBackendSpec {
    pub kind: String,
    pub path: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct PersonaEvalModelRoute {
    pub id: String,
    pub route: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub profile: Option<String>,
    #[serde(alias = "max-cost-usd")]
    pub max_cost_usd: Option<f64>,
    #[serde(alias = "max-model-calls")]
    pub max_model_calls: Option<u64>,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct PersonaEvalTimeoutTier {
    pub id: String,
    #[serde(alias = "timeout-ms")]
    pub timeout_ms: Option<u64>,
    #[serde(alias = "max-latency-ms")]
    pub max_latency_ms: Option<u64>,
    #[serde(alias = "max-cost-usd")]
    pub max_cost_usd: Option<f64>,
    #[serde(alias = "max-tool-calls")]
    pub max_tool_calls: Option<u64>,
    #[serde(alias = "max-model-calls")]
    pub max_model_calls: Option<u64>,
    #[serde(alias = "max-sweeps")]
    pub max_sweeps: Option<u32>,
    #[serde(alias = "watch-backoff-ms")]
    pub watch_backoff_ms: Option<u64>,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonaEvalTierOutcome {
    Correct,
    Degraded,
    Loop,
}

impl PersonaEvalTierOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Correct => "correct",
            Self::Degraded => "degraded",
            Self::Loop => "loop",
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct PersonaEvalLadderReport {
    #[serde(rename = "_type")]
    pub type_name: String,
    pub version: u32,
    pub id: String,
    pub persona: String,
    pub severity: String,
    pub blocking: bool,
    pub pass: bool,
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub first_correct_tier: Option<String>,
    pub first_correct_route: Option<String>,
    pub first_correct_index: Option<usize>,
    pub artifact_root: String,
    pub tiers: Vec<PersonaEvalTierReport>,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct PersonaEvalTierReport {
    pub id: String,
    pub route_id: String,
    pub model_route: Option<String>,
    pub timeout_tier: String,
    pub timeout_ms: Option<u64>,
    pub max_cost_usd: Option<f64>,
    pub max_latency_ms: Option<u64>,
    pub pass: bool,
    pub outcome: String,
    pub degradation_reasons: Vec<String>,
    pub transcript_path: Option<String>,
    pub receipt_path: String,
    pub summary_path: String,
    pub event_count: u64,
    pub cost_usd: f64,
    pub latency_ms: u64,
    pub tool_calls: u64,
    pub model_calls: u64,
    pub oracle_error_findings: usize,
    pub oracle_warn_findings: usize,
    pub state_machine_coverage: StateMachineCoverage,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct StateMachineCoverage {
    pub observed: usize,
    pub observed_steps: Vec<String>,
    pub transitions: Vec<StateTransition>,
}

pub fn load_persona_eval_ladder_manifest(
    path: &Path,
) -> Result<PersonaEvalLadderManifest, VmError> {
    let content = fs::read_to_string(path).map_err(|error| {
        VmError::Runtime(format!(
            "failed to read persona eval ladder manifest {}: {error}",
            path.display()
        ))
    })?;
    let mut manifest: PersonaEvalLadderManifest =
        if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
            serde_json::from_str(&content).map_err(|error| {
                VmError::Runtime(format!(
                    "failed to parse persona eval ladder JSON {}: {error}",
                    path.display()
                ))
            })?
        } else {
            toml::from_str(&content).map_err(|error| {
                VmError::Runtime(format!(
                    "failed to parse persona eval ladder TOML {}: {error}",
                    path.display()
                ))
            })?
        };
    normalize_persona_eval_ladder_manifest(&mut manifest);
    if manifest.base_dir.is_none() {
        manifest.base_dir = path.parent().map(|parent| parent.display().to_string());
    }
    Ok(manifest)
}

pub fn normalize_persona_eval_ladder_manifest_value(
    value: &VmValue,
) -> Result<PersonaEvalLadderManifest, VmError> {
    let mut manifest: PersonaEvalLadderManifest = parse_json_value(value)?;
    normalize_persona_eval_ladder_manifest(&mut manifest);
    Ok(manifest)
}

pub fn normalize_persona_eval_ladder_manifest(manifest: &mut PersonaEvalLadderManifest) {
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
            .unwrap_or_else(|| new_id("persona_eval_ladder"));
    }
    if manifest.persona.trim().is_empty() {
        manifest.persona = DEFAULT_PERSONA.to_string();
    }
    if manifest.backend.kind.trim().is_empty() {
        manifest.backend.kind = "replay".to_string();
    }
    if manifest.model_routes.is_empty() {
        manifest.model_routes.push(PersonaEvalModelRoute {
            id: "default".to_string(),
            ..Default::default()
        });
    }
    for (index, route) in manifest.model_routes.iter_mut().enumerate() {
        if route.id.trim().is_empty() {
            route.id = format!("route_{}", index + 1);
        }
    }
    for (index, tier) in manifest.timeout_tiers.iter_mut().enumerate() {
        if tier.id.trim().is_empty() {
            tier.id = format!("tier_{}", index + 1);
        }
    }
}

pub fn run_persona_eval_ladder(
    manifest: &PersonaEvalLadderManifest,
) -> Result<PersonaEvalLadderReport, VmError> {
    let mut manifest = manifest.clone();
    normalize_persona_eval_ladder_manifest(&mut manifest);
    if manifest.persona != DEFAULT_PERSONA {
        return Err(VmError::Runtime(format!(
            "persona eval ladder only supports persona '{}', got '{}'",
            DEFAULT_PERSONA, manifest.persona
        )));
    }
    if manifest.timeout_tiers.is_empty() {
        return Err(VmError::Runtime(format!(
            "persona eval ladder '{}' must declare at least one timeout tier",
            manifest.id
        )));
    }

    let base_dir = manifest.base_dir.as_deref().map(Path::new);
    let backend = resolve_ladder_backend(&manifest.backend, base_dir)?;
    let artifact_root = resolve_artifact_root(&manifest, base_dir);
    fs::create_dir_all(&artifact_root).map_err(|error| {
        VmError::Runtime(format!(
            "failed to create persona eval ladder artifact root {}: {error}",
            artifact_root.display()
        ))
    })?;

    let mut tiers = Vec::new();
    for route in &manifest.model_routes {
        for tier in &manifest.timeout_tiers {
            let index = tiers.len();
            tiers.push(run_ladder_tier(
                &backend,
                &artifact_root,
                route,
                tier,
                index,
            )?);
        }
    }

    let first_correct_index = tiers.iter().position(|tier| tier.pass);
    let (first_correct_tier, first_correct_route) = first_correct_index
        .and_then(|index| tiers.get(index))
        .map(|tier| (Some(tier.timeout_tier.clone()), Some(tier.route_id.clone())))
        .unwrap_or((None, None));
    let passed = tiers.iter().filter(|tier| tier.pass).count();
    let total = tiers.len();
    let severity = normalize_ladder_severity(manifest.severity.as_deref());
    Ok(PersonaEvalLadderReport {
        type_name: REPORT_TYPE.to_string(),
        version: 1,
        id: manifest.id,
        persona: manifest.persona,
        blocking: severity == "blocking",
        severity,
        pass: first_correct_index.is_some(),
        total,
        passed,
        failed: total.saturating_sub(passed),
        first_correct_tier,
        first_correct_route,
        first_correct_index,
        artifact_root: artifact_root.display().to_string(),
        tiers,
        metadata: manifest.metadata,
    })
}

fn resolve_ladder_backend(
    spec: &PersonaEvalLadderBackendSpec,
    base_dir: Option<&Path>,
) -> Result<MergeCaptainDriverBackend, VmError> {
    match spec.kind.trim().to_ascii_lowercase().as_str() {
        "live" => Ok(MergeCaptainDriverBackend::Live),
        "mock" => {
            let path = spec.path.as_deref().ok_or_else(|| {
                VmError::Runtime("mock ladder backend requires backend.path".to_string())
            })?;
            Ok(MergeCaptainDriverBackend::Mock {
                playground_dir: resolve_manifest_path(base_dir, path),
            })
        }
        "replay" => {
            let path = spec.path.as_deref().ok_or_else(|| {
                VmError::Runtime("replay ladder backend requires backend.path".to_string())
            })?;
            Ok(MergeCaptainDriverBackend::Replay {
                fixture: resolve_manifest_path(base_dir, path),
            })
        }
        other => Err(VmError::Runtime(format!(
            "unsupported persona eval ladder backend '{other}'"
        ))),
    }
}

fn resolve_artifact_root(manifest: &PersonaEvalLadderManifest, base_dir: Option<&Path>) -> PathBuf {
    let root = manifest
        .artifact_root
        .clone()
        .unwrap_or_else(|| format!(".harn-runs/persona-eval-ladders/{}", manifest.id));
    resolve_manifest_path(base_dir, &root)
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

fn run_ladder_tier(
    backend: &MergeCaptainDriverBackend,
    artifact_root: &Path,
    route: &PersonaEvalModelRoute,
    tier: &PersonaEvalTimeoutTier,
    index: usize,
) -> Result<PersonaEvalTierReport, VmError> {
    let tier_dir = artifact_root
        .join(format!("{:02}-{}", index + 1, safe_path_segment(&route.id)))
        .join(safe_path_segment(&tier.id));
    fs::create_dir_all(&tier_dir).map_err(|error| {
        VmError::Runtime(format!(
            "failed to create persona eval ladder tier dir {}: {error}",
            tier_dir.display()
        ))
    })?;

    let transcript_path = tier_dir.join("event_log.jsonl");
    let receipt_path = tier_dir.join("receipt.json");
    let summary_path = tier_dir.join("summary.json");
    let max_sweeps = tier.max_sweeps.unwrap_or(1).max(1);
    let options = MergeCaptainDriverOptions {
        backend: backend.clone(),
        mode: if max_sweeps > 1 {
            MergeCaptainDriverMode::Watch
        } else {
            MergeCaptainDriverMode::Once
        },
        model_route: Some(route.route.clone().unwrap_or_else(|| route.id.clone())),
        timeout_tier: Some(tier.id.clone()),
        transcript_out: Some(transcript_path),
        receipt_out: Some(receipt_path),
        run_root: tier_dir.join("runs"),
        max_sweeps,
        watch_backoff_ms: tier.watch_backoff_ms.unwrap_or(0),
        stream_stdout: false,
    };

    let output = super::run_merge_captain_driver(options)?;
    write_json_file(&summary_path, &output.summary)?;

    let mut reasons = degradation_reasons(&output.summary, route, tier);
    if !output.summary.pass {
        reasons.push(format!(
            "oracle reported {} error finding(s) and {} warning finding(s)",
            output.summary.oracle_error_findings, output.summary.oracle_warn_findings
        ));
    }
    let looped = output.audit_report.findings.iter().any(|finding| {
        let message = finding.message.to_ascii_lowercase();
        message.contains("loop") || message.contains("stuck")
    });
    let pass = output.summary.pass && reasons.is_empty();
    let outcome = if looped {
        PersonaEvalTierOutcome::Loop
    } else if pass {
        PersonaEvalTierOutcome::Correct
    } else {
        PersonaEvalTierOutcome::Degraded
    };

    Ok(PersonaEvalTierReport {
        id: format!("{}::{}", route.id, tier.id),
        route_id: route.id.clone(),
        model_route: output.summary.model_route.clone(),
        timeout_tier: tier.id.clone(),
        timeout_ms: tier.timeout_ms,
        max_cost_usd: tier.max_cost_usd.or(route.max_cost_usd),
        max_latency_ms: tier.max_latency_ms.or(tier.timeout_ms),
        pass,
        outcome: outcome.as_str().to_string(),
        degradation_reasons: reasons,
        transcript_path: output
            .transcript_path
            .as_deref()
            .map(|path| path.display().to_string()),
        receipt_path: output.receipt_path.display().to_string(),
        summary_path: summary_path.display().to_string(),
        event_count: output.summary.event_count,
        cost_usd: output.summary.cost_usd,
        latency_ms: output.summary.latency_ms,
        tool_calls: output.summary.tool_calls,
        model_calls: output.summary.model_calls,
        oracle_error_findings: output.summary.oracle_error_findings,
        oracle_warn_findings: output.summary.oracle_warn_findings,
        state_machine_coverage: state_machine_coverage(&output.summary.state_transitions),
    })
}

fn degradation_reasons(
    summary: &MergeCaptainRunSummary,
    route: &PersonaEvalModelRoute,
    tier: &PersonaEvalTimeoutTier,
) -> Vec<String> {
    let mut reasons = Vec::new();
    if let Some(max_tool_calls) = tier.max_tool_calls {
        if summary.tool_calls > max_tool_calls {
            reasons.push(format!(
                "tool calls {} exceeded tier budget {}",
                summary.tool_calls, max_tool_calls
            ));
        }
    }
    if let Some(max_model_calls) = tier.max_model_calls.or(route.max_model_calls) {
        if summary.model_calls > max_model_calls {
            reasons.push(format!(
                "model calls {} exceeded budget {}",
                summary.model_calls, max_model_calls
            ));
        }
    }
    if let Some(max_cost_usd) = tier.max_cost_usd.or(route.max_cost_usd) {
        if summary.cost_usd > max_cost_usd {
            reasons.push(format!(
                "cost ${:.6} exceeded budget ${:.6}",
                summary.cost_usd, max_cost_usd
            ));
        }
    }
    if let Some(max_latency_ms) = tier.max_latency_ms.or(tier.timeout_ms) {
        if summary.latency_ms > max_latency_ms {
            reasons.push(format!(
                "latency {}ms exceeded tier timeout {}ms",
                summary.latency_ms, max_latency_ms
            ));
        }
    }
    reasons
}

fn state_machine_coverage(transitions: &[StateTransition]) -> StateMachineCoverage {
    let observed_steps: Vec<String> = transitions
        .iter()
        .map(|transition| transition.step.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    StateMachineCoverage {
        observed: observed_steps.len(),
        observed_steps,
        transitions: transitions.to_vec(),
    }
}

fn normalize_ladder_severity(value: Option<&str>) -> String {
    match value
        .unwrap_or("blocking")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "warn" | "warning" => "warning".to_string(),
        "info" | "informational" => "informational".to_string(),
        _ => "blocking".to_string(),
    }
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

    fn repo_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf()
    }

    #[test]
    fn ladder_marks_first_correct_tier_and_writes_artifacts() {
        let temp = tempfile::tempdir().unwrap();
        let manifest = PersonaEvalLadderManifest {
            id: "merge-captain-ladder-test".to_string(),
            base_dir: Some(repo_root().display().to_string()),
            artifact_root: Some(temp.path().join("ladder").display().to_string()),
            backend: PersonaEvalLadderBackendSpec {
                kind: "replay".to_string(),
                path: Some(
                    "examples/personas/merge_captain/transcripts/green_pr.jsonl".to_string(),
                ),
            },
            model_routes: vec![PersonaEvalModelRoute {
                id: "gemma-value".to_string(),
                route: Some("local/gemma-value".to_string()),
                provider: Some("llama.cpp".to_string()),
                model: Some("gemma".to_string()),
                profile: Some("value".to_string()),
                ..Default::default()
            }],
            timeout_tiers: vec![
                PersonaEvalTimeoutTier {
                    id: "too-tight".to_string(),
                    max_tool_calls: Some(1),
                    ..Default::default()
                },
                PersonaEvalTimeoutTier {
                    id: "balanced".to_string(),
                    max_tool_calls: Some(4),
                    max_model_calls: Some(1),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        let report = run_persona_eval_ladder(&manifest).unwrap();

        assert!(report.pass);
        assert_eq!(report.total, 2);
        assert_eq!(report.first_correct_tier.as_deref(), Some("balanced"));
        assert_eq!(report.first_correct_route.as_deref(), Some("gemma-value"));
        assert_eq!(report.tiers[0].outcome, "degraded");
        assert_eq!(report.tiers[1].outcome, "correct");
        assert!(Path::new(&report.tiers[0].transcript_path.as_ref().unwrap()).exists());
        assert!(Path::new(&report.tiers[1].receipt_path).exists());
        assert!(report.tiers[1].state_machine_coverage.observed > 0);
    }

    #[test]
    fn unsupported_persona_is_rejected() {
        let manifest = PersonaEvalLadderManifest {
            id: "other-persona".to_string(),
            persona: "ship_captain".to_string(),
            timeout_tiers: vec![PersonaEvalTimeoutTier {
                id: "smoke".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        };

        let error = run_persona_eval_ladder(&manifest).unwrap_err();

        assert!(format!("{error}").contains("only supports persona"));
    }
}
