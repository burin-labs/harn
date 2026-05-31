//! Crystallization bundle: types, build/write/load/validate, shadow replay, and redaction helpers.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use super::super::{now_rfc3339, ReplayTraceRun};
use super::api::load_crystallization_trace;
use super::shadow::{find_sequence_start, shadow_candidate};
use super::types::{
    CrystallizationApproval, CrystallizationArtifacts, CrystallizationReport,
    CrystallizationSideEffect, CrystallizationTrace, PromotionApprovalRecord, PromotionCriteria,
    PromotionDivergenceRecord, SavingsEstimate, SegmentKind, ShadowRunReport,
    SkillInductionGateReceipt, WorkflowCandidate, WorkflowCandidateStep, BUNDLE_EVAL_PACK_FILE,
    BUNDLE_FIXTURES_DIR, BUNDLE_MANIFEST_FILE, BUNDLE_REPORT_FILE, BUNDLE_SCHEMA,
    BUNDLE_SCHEMA_VERSION, BUNDLE_SKILL_DIR, BUNDLE_SKILL_FILE, BUNDLE_SKILL_GATE_FILE,
    BUNDLE_WORKFLOW_FILE, DEFAULT_ROLLOUT_POLICY, SKILL_GATE_RECEIPT_SCHEMA,
};
use crate::skills::{parse_frontmatter, split_frontmatter};
use crate::value::VmError;

// ===== Crystallization bundle =====
//
// A bundle is a directory layout that Harn writes and Harn Cloud (or any
// other importer) reads without bespoke glue. The contract is:
//
//   bundle/
//     candidate.json        # versioned manifest documented below
//     workflow.harn         # generated/reviewable workflow code
//     report.json           # full mining/shadow/eval report
//     harn.eval.toml        # generated eval pack when available (optional)
//     fixtures/             # redacted replay fixtures referenced by the
//                           # report (optional, only when --bundle is used
//                           # with `harn crystallize` and traces were on disk)
//
// `candidate.json` is the authoritative manifest. It must include the
// `schema` and `schema_version` markers. Cloud importers MUST reject any
// bundle whose `schema` is not exactly `harn.crystallization.candidate.bundle`
// or whose `schema_version` is greater than the highest version they
// understand. Only the documented additive fields may be added without
// bumping `schema_version`.

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct BundleGenerator {
    pub tool: String,
    pub version: String,
}

impl Default for BundleGenerator {
    fn default() -> Self {
        Self {
            tool: "harn".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct BundleWorkflowRef {
    /// Relative path inside the bundle directory.
    pub path: String,
    /// Short identifier used in `pipeline NAME(...)`.
    pub name: String,
    /// Logical package name promotion uses to register the workflow.
    pub package_name: String,
    /// Initial workflow version proposed for promotion.
    pub package_version: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct BundleSourceTrace {
    pub trace_id: String,
    pub source_hash: String,
    /// Optional human-visible URL (PR, issue, run record path) for the
    /// trace. `None` when the trace was loaded from an in-memory store.
    pub source_url: Option<String>,
    /// Optional cloud-side receipt id when the trace was already promoted
    /// into a tenant receipt. Cloud importers use this to wire candidate
    /// evidence to existing receipts without round-tripping the raw payload.
    pub source_receipt_id: Option<String>,
    /// Relative path of the redacted fixture inside the bundle, if one
    /// was emitted.
    pub fixture_path: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct BundleStep {
    pub index: usize,
    pub kind: String,
    pub name: String,
    pub segment: SegmentKind,
    pub parameter_refs: Vec<String>,
    pub side_effects: Vec<CrystallizationSideEffect>,
    pub capabilities: Vec<String>,
    pub required_secrets: Vec<String>,
    pub approval: Option<CrystallizationApproval>,
    pub review_notes: Vec<String>,
}

impl BundleStep {
    fn from_candidate_step(step: &WorkflowCandidateStep) -> Self {
        Self {
            index: step.index,
            kind: step.kind.clone(),
            name: step.name.clone(),
            segment: step.segment.clone(),
            parameter_refs: step.parameter_refs.clone(),
            side_effects: step.side_effects.clone(),
            capabilities: step.capabilities.clone(),
            required_secrets: step.required_secrets.clone(),
            approval: step.approval.clone(),
            review_notes: step.review_notes.clone(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct BundleEvalPackRef {
    /// Relative path of the eval pack inside the bundle directory.
    pub path: String,
    /// Optional external link the eval pack also lives at (e.g. a hosted
    /// `eval-pack://` URI when the bundle was promoted into a tenant).
    pub link: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct BundleSkillRef {
    /// Relative path to the generated `SKILL.md` inside the bundle.
    pub path: String,
    /// Relative path to the replay/held-out gate receipt for the skill.
    pub gate_receipt_path: String,
    pub name: String,
    pub skill_candidate_id: String,
    pub workflow_candidate_id: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct BundleFixtureRef {
    pub path: String,
    pub trace_id: String,
    pub source_hash: String,
    pub redacted: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct BundlePromotion {
    pub owner: Option<String>,
    pub approver: Option<String>,
    pub author: Option<String>,
    /// Logical rollout strategy. Defaults to `shadow_then_canary`. Hosted
    /// surfaces may extend this enum but must keep existing values stable.
    pub rollout_policy: String,
    pub rollback_target: Option<String>,
    pub created_at: String,
    pub workflow_version: String,
    pub package_name: String,
    pub sample_count: usize,
    pub confidence: f64,
    pub shadow_success_count: usize,
    pub shadow_failure_count: usize,
    pub divergence_history: Vec<PromotionDivergenceRecord>,
    pub approval_history: Vec<PromotionApprovalRecord>,
    pub criteria: PromotionCriteria,
    pub estimated_time_token_savings: SavingsEstimate,
}

impl Default for BundlePromotion {
    fn default() -> Self {
        Self {
            owner: None,
            approver: None,
            author: None,
            rollout_policy: DEFAULT_ROLLOUT_POLICY.to_string(),
            rollback_target: None,
            created_at: String::new(),
            workflow_version: String::new(),
            package_name: String::new(),
            sample_count: 0,
            confidence: 0.0,
            shadow_success_count: 0,
            shadow_failure_count: 0,
            divergence_history: Vec::new(),
            approval_history: Vec::new(),
            criteria: PromotionCriteria::default(),
            estimated_time_token_savings: SavingsEstimate::default(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct BundleRedactionSummary {
    pub applied: bool,
    pub rules: Vec<String>,
    pub summary: String,
    /// Number of fixture files copied into the bundle (0 when no fixture
    /// directory was emitted).
    pub fixture_count: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct CrystallizationBundleManifest {
    pub schema: String,
    pub schema_version: u32,
    pub generated_at: String,
    pub generator: BundleGenerator,
    pub kind: BundleKind,
    pub candidate_id: String,
    pub external_key: String,
    pub title: String,
    pub team: Option<String>,
    pub repo: Option<String>,
    pub risk_level: String,
    pub workflow: BundleWorkflowRef,
    pub source_trace_hashes: Vec<String>,
    pub source_traces: Vec<BundleSourceTrace>,
    pub deterministic_steps: Vec<BundleStep>,
    pub fuzzy_steps: Vec<BundleStep>,
    pub side_effects: Vec<CrystallizationSideEffect>,
    pub capabilities: Vec<String>,
    pub required_secrets: Vec<String>,
    pub savings: SavingsEstimate,
    pub shadow: ShadowRunReport,
    pub eval_pack: Option<BundleEvalPackRef>,
    pub skill: Option<BundleSkillRef>,
    pub fixtures: Vec<BundleFixtureRef>,
    pub promotion: BundlePromotion,
    pub redaction: BundleRedactionSummary,
    pub confidence: f64,
    pub rejection_reasons: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BundleKind {
    /// A normal candidate that passed shadow comparison and is ready for
    /// review and promotion.
    #[default]
    Candidate,
    /// A "plan-only" candidate: every step has a side-effect-free, in-process
    /// outcome (e.g. classify and write a receipt). Cloud importers can
    /// promote these without explicit external-side-effect approval.
    PlanOnly,
    /// No safe candidate was selected. The bundle still records what was
    /// attempted, the rejection reasons, and any rejected candidates so
    /// reviewers can debug or feed it back into mining.
    Rejected,
}

#[derive(Clone, Debug, Default)]
pub struct BundleOptions {
    /// Stable identifier downstream cloud importers use to dedupe bundles
    /// across runs (defaults to a sanitized workflow name).
    pub external_key: Option<String>,
    pub title: Option<String>,
    pub team: Option<String>,
    pub repo: Option<String>,
    pub risk_level: Option<String>,
    pub rollout_policy: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct CrystallizationBundle {
    pub manifest: CrystallizationBundleManifest,
    pub report: CrystallizationReport,
    pub harn_code: String,
    pub eval_pack_toml: String,
    pub skill_markdown: String,
    pub skill_gate_receipt_json: String,
    pub fixtures: Vec<CrystallizationTrace>,
}

/// Errors surfaced when validating a bundle on disk.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct BundleValidation {
    pub bundle_dir: String,
    pub schema: String,
    pub schema_version: u32,
    pub kind: BundleKind,
    pub candidate_id: String,
    pub manifest_ok: bool,
    pub workflow_ok: bool,
    pub report_ok: bool,
    pub eval_pack_ok: bool,
    pub skill_ok: bool,
    pub fixtures_ok: bool,
    pub redaction_ok: bool,
    pub problems: Vec<String>,
}

impl BundleValidation {
    pub fn is_ok(&self) -> bool {
        self.problems.is_empty()
    }
}

/// Build an in-memory bundle from already-mined artifacts. The traces
/// passed here are the same normalized traces used to mine the candidate;
/// they will be redacted before being attached as fixtures.
pub fn build_crystallization_bundle(
    artifacts: CrystallizationArtifacts,
    traces: &[CrystallizationTrace],
    options: BundleOptions,
) -> Result<CrystallizationBundle, VmError> {
    let CrystallizationArtifacts {
        report,
        harn_code,
        eval_pack_toml,
    } = artifacts;

    let (selected, kind) = match report
        .selected_candidate_id
        .as_deref()
        .and_then(|id| report.candidates.iter().find(|c| c.id == id))
    {
        Some(candidate) => {
            let kind = if candidate_is_plan_only(candidate) {
                BundleKind::PlanOnly
            } else {
                BundleKind::Candidate
            };
            (Some(candidate), kind)
        }
        None => (None, BundleKind::Rejected),
    };

    let workflow_name = selected
        .map(|candidate| candidate.name.clone())
        .unwrap_or_else(|| "crystallized_workflow".to_string());
    let package_name = selected
        .map(|candidate| candidate.promotion.package_name.clone())
        .unwrap_or_else(|| workflow_name.replace('_', "-"));
    let workflow_version = selected
        .map(|candidate| candidate.promotion.version.clone())
        .unwrap_or_else(|| "0.0.0".to_string());

    let manifest_workflow = BundleWorkflowRef {
        path: BUNDLE_WORKFLOW_FILE.to_string(),
        name: workflow_name.clone(),
        package_name: package_name.clone(),
        package_version: workflow_version.clone(),
    };

    let external_key = options
        .external_key
        .clone()
        .filter(|key| !key.trim().is_empty())
        .unwrap_or_else(|| sanitize_external_key(&workflow_name));
    let title = options
        .title
        .clone()
        .filter(|title| !title.trim().is_empty())
        .unwrap_or_else(|| infer_bundle_title(selected, &workflow_name));
    let risk_level = options
        .risk_level
        .clone()
        .filter(|risk| !risk.trim().is_empty())
        .unwrap_or_else(|| infer_risk_level(selected));
    let rollout_policy = options
        .rollout_policy
        .clone()
        .filter(|policy| !policy.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_ROLLOUT_POLICY.to_string());

    let (deterministic_steps, fuzzy_steps) = match selected {
        Some(candidate) => candidate
            .steps
            .iter()
            .map(BundleStep::from_candidate_step)
            .partition::<Vec<_>, _>(|step| step.segment == SegmentKind::Deterministic),
        None => (Vec::new(), Vec::new()),
    };

    let source_trace_hashes = selected
        .map(|candidate| candidate.promotion.source_trace_hashes.clone())
        .unwrap_or_default();

    let mut source_traces = Vec::new();
    let mut fixture_refs = Vec::new();
    let mut fixture_payloads = Vec::new();
    if let Some(candidate) = selected {
        let mut fixture_trace_ids = BTreeSet::new();
        for example in &candidate.examples {
            fixture_trace_ids.insert(example.trace_id.clone());
        }
        for trace in traces {
            if find_sequence_start(trace, &candidate.sequence_signature).is_some() {
                fixture_trace_ids.insert(trace.id.clone());
            }
        }
        for trace_id in fixture_trace_ids {
            let trace = traces.iter().find(|trace| trace.id == trace_id);
            let source_hash = trace
                .and_then(|trace| trace.source_hash.clone())
                .or_else(|| {
                    candidate
                        .examples
                        .iter()
                        .find(|example| example.trace_id == trace_id)
                        .map(|example| example.source_hash.clone())
                })
                .unwrap_or_default();
            let fixture_relative = trace.map(|trace| {
                format!(
                    "{BUNDLE_FIXTURES_DIR}/{}.json",
                    sanitize_fixture_name(&trace.id)
                )
            });
            source_traces.push(BundleSourceTrace {
                trace_id: trace_id.clone(),
                source_hash: source_hash.clone(),
                source_url: trace.and_then(|trace| trace.source.clone()),
                source_receipt_id: trace
                    .and_then(|trace| trace.metadata.get("source_receipt_id"))
                    .and_then(|value| value.as_str().map(str::to_string)),
                fixture_path: fixture_relative.clone(),
            });
            if let (Some(trace), Some(fixture_path)) = (trace, fixture_relative.clone()) {
                let mut redacted = trace.clone();
                redact_trace_for_bundle(&mut redacted);
                fixture_refs.push(BundleFixtureRef {
                    path: fixture_path,
                    trace_id: trace.id.clone(),
                    source_hash,
                    redacted: true,
                });
                fixture_payloads.push(redacted);
            }
        }
    }

    // Owner defaults to author so cloud importers always have a populated
    // ownership pointer, but stays separate from `author` so reviewers can
    // assign a different owner in the manifest before promotion.
    let author = selected.and_then(|candidate| candidate.promotion.author.clone());
    let promotion = BundlePromotion {
        owner: author.clone(),
        approver: selected.and_then(|candidate| candidate.promotion.approver.clone()),
        author,
        rollout_policy,
        rollback_target: selected.and_then(|candidate| candidate.promotion.rollback_target.clone()),
        created_at: now_rfc3339(),
        workflow_version,
        package_name,
        sample_count: selected
            .map(|candidate| candidate.promotion.sample_count)
            .unwrap_or_default(),
        confidence: selected
            .map(|candidate| candidate.promotion.confidence)
            .unwrap_or_default(),
        shadow_success_count: selected
            .map(|candidate| candidate.promotion.shadow_success_count)
            .unwrap_or_default(),
        shadow_failure_count: selected
            .map(|candidate| candidate.promotion.shadow_failure_count)
            .unwrap_or_default(),
        divergence_history: selected
            .map(|candidate| candidate.promotion.divergence_history.clone())
            .unwrap_or_default(),
        approval_history: selected
            .map(|candidate| candidate.promotion.approval_history.clone())
            .unwrap_or_default(),
        criteria: selected
            .map(|candidate| candidate.promotion.criteria.clone())
            .unwrap_or_default(),
        estimated_time_token_savings: selected
            .map(|candidate| candidate.promotion.estimated_time_token_savings.clone())
            .unwrap_or_default(),
    };

    let redaction = BundleRedactionSummary {
        applied: !fixture_payloads.is_empty(),
        rules: vec![
            "sensitive_keys".to_string(),
            "secret_value_heuristic".to_string(),
        ],
        summary: if fixture_payloads.is_empty() {
            "no fixtures emitted".to_string()
        } else {
            "fixture payloads scrubbed of secret-like values and sensitive keys before write"
                .to_string()
        },
        fixture_count: fixture_payloads.len(),
    };

    let eval_pack = if eval_pack_toml.trim().is_empty() {
        None
    } else {
        Some(BundleEvalPackRef {
            path: BUNDLE_EVAL_PACK_FILE.to_string(),
            link: selected
                .and_then(|candidate| candidate.promotion.eval_pack_link.clone())
                .filter(|link| !link.trim().is_empty()),
        })
    };
    let selected_skill = selected.and_then(|candidate| {
        report
            .skill_candidates
            .iter()
            .find(|skill| skill.workflow_candidate_id == candidate.id)
    });
    let skill = selected_skill.map(|skill| BundleSkillRef {
        path: format!("{BUNDLE_SKILL_DIR}/{BUNDLE_SKILL_FILE}"),
        gate_receipt_path: format!("{BUNDLE_SKILL_DIR}/{BUNDLE_SKILL_GATE_FILE}"),
        name: skill.name.clone(),
        skill_candidate_id: skill.id.clone(),
        workflow_candidate_id: skill.workflow_candidate_id.clone(),
    });
    let skill_markdown = selected_skill
        .map(|skill| skill.skill_markdown.clone())
        .unwrap_or_default();
    let skill_gate_receipt_json = selected_skill
        .and_then(|skill| serde_json::to_string_pretty(&skill.replay_gate.receipt).ok())
        .unwrap_or_default();

    let manifest = CrystallizationBundleManifest {
        schema: BUNDLE_SCHEMA.to_string(),
        schema_version: BUNDLE_SCHEMA_VERSION,
        generated_at: now_rfc3339(),
        generator: BundleGenerator::default(),
        kind,
        candidate_id: selected
            .map(|candidate| candidate.id.clone())
            .unwrap_or_default(),
        external_key,
        title,
        team: options.team,
        repo: options.repo,
        risk_level,
        workflow: manifest_workflow,
        source_trace_hashes,
        source_traces,
        deterministic_steps,
        fuzzy_steps,
        side_effects: selected
            .map(|candidate| candidate.side_effects.clone())
            .unwrap_or_default(),
        capabilities: selected
            .map(|candidate| candidate.capabilities.clone())
            .unwrap_or_default(),
        required_secrets: selected
            .map(|candidate| candidate.required_secrets.clone())
            .unwrap_or_default(),
        savings: selected
            .map(|candidate| candidate.savings.clone())
            .unwrap_or_default(),
        shadow: selected
            .map(|candidate| candidate.shadow.clone())
            .unwrap_or_default(),
        eval_pack,
        skill,
        fixtures: fixture_refs,
        promotion,
        redaction,
        confidence: selected
            .map(|candidate| candidate.confidence)
            .unwrap_or(0.0),
        rejection_reasons: report
            .rejected_candidates
            .iter()
            .flat_map(|candidate| candidate.rejection_reasons.iter().cloned())
            .collect(),
        warnings: report.warnings.clone(),
    };

    Ok(CrystallizationBundle {
        manifest,
        report,
        harn_code,
        eval_pack_toml,
        skill_markdown,
        skill_gate_receipt_json,
        fixtures: fixture_payloads,
    })
}

/// Write a bundle to a directory. Creates the directory if it does not
/// already exist. Returns the manifest with `generated_at` and any
/// runtime-resolved metadata filled in.
pub fn write_crystallization_bundle(
    bundle: &CrystallizationBundle,
    bundle_dir: &Path,
) -> Result<CrystallizationBundleManifest, VmError> {
    std::fs::create_dir_all(bundle_dir).map_err(|error| {
        VmError::Runtime(format!(
            "failed to create bundle dir {}: {error}",
            bundle_dir.display()
        ))
    })?;
    write_bytes(
        &bundle_dir.join(BUNDLE_WORKFLOW_FILE),
        bundle.harn_code.as_bytes(),
    )?;
    let report_json = serde_json::to_vec_pretty(&bundle.report)
        .map_err(|error| VmError::Runtime(format!("failed to encode report JSON: {error}")))?;
    write_bytes(&bundle_dir.join(BUNDLE_REPORT_FILE), &report_json)?;

    if !bundle.eval_pack_toml.trim().is_empty() {
        write_bytes(
            &bundle_dir.join(BUNDLE_EVAL_PACK_FILE),
            bundle.eval_pack_toml.as_bytes(),
        )?;
    }

    if !bundle.skill_markdown.trim().is_empty() {
        let skill_dir = bundle_dir.join(BUNDLE_SKILL_DIR);
        std::fs::create_dir_all(&skill_dir).map_err(|error| {
            VmError::Runtime(format!(
                "failed to create skill dir {}: {error}",
                skill_dir.display()
            ))
        })?;
        write_bytes(
            &skill_dir.join(BUNDLE_SKILL_FILE),
            bundle.skill_markdown.as_bytes(),
        )?;
        if !bundle.skill_gate_receipt_json.trim().is_empty() {
            write_bytes(
                &skill_dir.join(BUNDLE_SKILL_GATE_FILE),
                bundle.skill_gate_receipt_json.as_bytes(),
            )?;
        }
    }

    if !bundle.fixtures.is_empty() {
        let fixtures_dir = bundle_dir.join(BUNDLE_FIXTURES_DIR);
        std::fs::create_dir_all(&fixtures_dir).map_err(|error| {
            VmError::Runtime(format!(
                "failed to create fixtures dir {}: {error}",
                fixtures_dir.display()
            ))
        })?;
        for trace in &bundle.fixtures {
            let path = fixtures_dir.join(format!("{}.json", sanitize_fixture_name(&trace.id)));
            let payload = serde_json::to_vec_pretty(trace).map_err(|error| {
                VmError::Runtime(format!("failed to encode fixture {}: {error}", trace.id))
            })?;
            write_bytes(&path, &payload)?;
        }
    }

    let manifest_json = serde_json::to_vec_pretty(&bundle.manifest)
        .map_err(|error| VmError::Runtime(format!("failed to encode manifest JSON: {error}")))?;
    write_bytes(&bundle_dir.join(BUNDLE_MANIFEST_FILE), &manifest_json)?;
    Ok(bundle.manifest.clone())
}

/// Read a bundle manifest from disk. Verifies the schema marker but does
/// not cross-check workflow/report/eval-pack sibling files; for a richer
/// check use [`validate_crystallization_bundle`].
pub fn load_crystallization_bundle_manifest(
    bundle_dir: &Path,
) -> Result<CrystallizationBundleManifest, VmError> {
    let manifest_path = bundle_dir.join(BUNDLE_MANIFEST_FILE);
    let bytes = std::fs::read(&manifest_path).map_err(|error| {
        VmError::Runtime(format!(
            "failed to read bundle manifest {}: {error}",
            manifest_path.display()
        ))
    })?;
    let manifest: CrystallizationBundleManifest =
        serde_json::from_slice(&bytes).map_err(|error| {
            VmError::Runtime(format!(
                "failed to decode bundle manifest {}: {error}",
                manifest_path.display()
            ))
        })?;
    if manifest.schema != BUNDLE_SCHEMA {
        return Err(VmError::Runtime(format!(
            "bundle {} has unrecognized schema {:?} (expected {})",
            bundle_dir.display(),
            manifest.schema,
            BUNDLE_SCHEMA
        )));
    }
    if manifest.schema_version > BUNDLE_SCHEMA_VERSION {
        return Err(VmError::Runtime(format!(
            "bundle {} schema_version {} is newer than supported {}",
            bundle_dir.display(),
            manifest.schema_version,
            BUNDLE_SCHEMA_VERSION
        )));
    }
    Ok(manifest)
}

fn resolve_bundle_manifest_path(
    bundle_dir: &Path,
    relative_path: &str,
    label: &str,
) -> Result<PathBuf, String> {
    let path = Path::new(relative_path);
    if relative_path.trim().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::Prefix(_) | Component::RootDir
            )
        })
        || has_windows_rooted_or_drive_relative_prefix(relative_path)
    {
        return Err(format!(
            "manifest {label} path {relative_path:?} must stay inside the bundle"
        ));
    }
    Ok(bundle_dir.join(path))
}

fn has_windows_rooted_or_drive_relative_prefix(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    let bytes = normalized.as_bytes();
    normalized.starts_with('/')
        || (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
}

/// Read every fixture trace referenced by the bundle manifest. Returns
/// the manifest plus loaded traces, in the order they appear in the
/// manifest. Fixtures with `path: None` are skipped.
pub fn load_crystallization_bundle(
    bundle_dir: &Path,
) -> Result<(CrystallizationBundleManifest, Vec<CrystallizationTrace>), VmError> {
    let manifest = load_crystallization_bundle_manifest(bundle_dir)?;
    let mut traces = Vec::new();
    for fixture in &manifest.fixtures {
        let path = resolve_bundle_manifest_path(bundle_dir, &fixture.path, "fixture")
            .map_err(VmError::Runtime)?;
        traces.push(load_crystallization_trace(&path)?);
    }
    Ok((manifest, traces))
}

/// Validate a bundle directory layout and contents. Cheap enough to call
/// from a CLI smoke command; performs no live side effects.
pub fn validate_crystallization_bundle(bundle_dir: &Path) -> Result<BundleValidation, VmError> {
    let mut validation = BundleValidation {
        bundle_dir: bundle_dir.display().to_string(),
        ..BundleValidation::default()
    };
    let manifest = match load_crystallization_bundle_manifest(bundle_dir) {
        Ok(manifest) => manifest,
        Err(error) => {
            validation.problems.push(error.to_string());
            return Ok(validation);
        }
    };
    validation.manifest_ok = true;
    validation.schema = manifest.schema.clone();
    validation.schema_version = manifest.schema_version;
    validation.kind = manifest.kind.clone();
    validation.candidate_id = manifest.candidate_id.clone();

    match resolve_bundle_manifest_path(bundle_dir, &manifest.workflow.path, "workflow") {
        Ok(workflow_path) if workflow_path.exists() => {
            validation.workflow_ok = true;
        }
        Ok(workflow_path) => {
            validation
                .problems
                .push(format!("missing workflow file {}", workflow_path.display()));
        }
        Err(problem) => validation.problems.push(problem),
    }

    let report_path = bundle_dir.join(BUNDLE_REPORT_FILE);
    match std::fs::read(&report_path) {
        Ok(bytes) => match serde_json::from_slice::<CrystallizationReport>(&bytes) {
            Ok(report) => {
                validation.report_ok = true;
                if matches!(manifest.kind, BundleKind::Candidate | BundleKind::PlanOnly)
                    && manifest.candidate_id.is_empty()
                {
                    validation
                        .problems
                        .push("manifest is non-rejected but has empty candidate_id".to_string());
                }
                if matches!(manifest.kind, BundleKind::Candidate | BundleKind::PlanOnly)
                    && report.selected_candidate_id.as_deref() != Some(&manifest.candidate_id)
                {
                    validation.problems.push(format!(
                        "report selected_candidate_id {:?} does not match manifest candidate_id {}",
                        report.selected_candidate_id, manifest.candidate_id
                    ));
                }
            }
            Err(error) => {
                validation
                    .problems
                    .push(format!("invalid report.json: {error}"));
            }
        },
        Err(error) => {
            validation.problems.push(format!(
                "missing report file {}: {error}",
                report_path.display()
            ));
        }
    }

    if let Some(eval_pack) = &manifest.eval_pack {
        match resolve_bundle_manifest_path(bundle_dir, &eval_pack.path, "eval_pack") {
            Ok(path) if path.exists() => {
                validation.eval_pack_ok = true;
            }
            Ok(path) => {
                validation.problems.push(format!(
                    "manifest references eval pack {} but file is missing",
                    path.display()
                ));
            }
            Err(problem) => validation.problems.push(problem),
        }
    } else {
        validation.eval_pack_ok = true;
    }

    if let Some(skill) = &manifest.skill {
        let mut skill_problem = false;
        match resolve_bundle_manifest_path(bundle_dir, &skill.path, "skill") {
            Ok(path) if path.exists() => match std::fs::read_to_string(&path) {
                Ok(source) => {
                    let (frontmatter, _) = split_frontmatter(&source);
                    match parse_frontmatter(frontmatter) {
                        Ok(parsed) => {
                            if parsed.manifest.name.trim().is_empty() {
                                validation
                                    .problems
                                    .push("skill SKILL.md is missing frontmatter name".to_string());
                                skill_problem = true;
                            } else if parsed.manifest.name != skill.name {
                                validation.problems.push(format!(
                                    "skill SKILL.md name {} does not match manifest skill name {}",
                                    parsed.manifest.name, skill.name
                                ));
                                skill_problem = true;
                            }
                            if parsed.manifest.short.trim().is_empty() {
                                validation.problems.push(
                                    "skill SKILL.md is missing required short card".to_string(),
                                );
                                skill_problem = true;
                            }
                        }
                        Err(error) => {
                            validation
                                .problems
                                .push(format!("invalid skill SKILL.md frontmatter: {error}"));
                            skill_problem = true;
                        }
                    }
                }
                Err(error) => {
                    validation.problems.push(format!(
                        "failed to read skill file {}: {error}",
                        path.display()
                    ));
                    skill_problem = true;
                }
            },
            Ok(path) => {
                validation.problems.push(format!(
                    "manifest references skill {} but file is missing",
                    path.display()
                ));
                skill_problem = true;
            }
            Err(problem) => {
                validation.problems.push(problem);
                skill_problem = true;
            }
        }
        match resolve_bundle_manifest_path(bundle_dir, &skill.gate_receipt_path, "skill gate") {
            Ok(path) if path.exists() => match std::fs::read_to_string(&path) {
                Ok(source) => match serde_json::from_str::<SkillInductionGateReceipt>(&source) {
                    Ok(receipt) => {
                        if receipt.type_name != SKILL_GATE_RECEIPT_SCHEMA {
                            validation.problems.push(format!(
                                "skill gate receipt has unexpected type {}",
                                receipt.type_name
                            ));
                            skill_problem = true;
                        }
                        if receipt.skill_candidate_id != skill.skill_candidate_id {
                            validation.problems.push(format!(
                                "skill gate receipt candidate id {} does not match manifest {}",
                                receipt.skill_candidate_id, skill.skill_candidate_id
                            ));
                            skill_problem = true;
                        }
                        if receipt.workflow_candidate_id != skill.workflow_candidate_id {
                            validation.problems.push(format!(
                                "skill gate receipt workflow id {} does not match manifest {}",
                                receipt.workflow_candidate_id, skill.workflow_candidate_id
                            ));
                            skill_problem = true;
                        }
                        if !receipt.accepted {
                            validation
                                .problems
                                .push("skill gate receipt is not accepted".to_string());
                            skill_problem = true;
                        }
                    }
                    Err(error) => {
                        validation
                            .problems
                            .push(format!("invalid skill gate receipt JSON: {error}"));
                        skill_problem = true;
                    }
                },
                Err(error) => {
                    validation.problems.push(format!(
                        "failed to read skill gate receipt {}: {error}",
                        path.display()
                    ));
                    skill_problem = true;
                }
            },
            Ok(path) => {
                validation.problems.push(format!(
                    "manifest references skill gate receipt {} but file is missing",
                    path.display()
                ));
                skill_problem = true;
            }
            Err(problem) => {
                validation.problems.push(problem);
                skill_problem = true;
            }
        }
        validation.skill_ok = !skill_problem;
    } else {
        validation.skill_ok = true;
    }

    let mut fixtures_problem = false;
    for fixture in &manifest.fixtures {
        let path = match resolve_bundle_manifest_path(bundle_dir, &fixture.path, "fixture") {
            Ok(path) => path,
            Err(problem) => {
                validation.problems.push(problem);
                fixtures_problem = true;
                continue;
            }
        };
        if !path.exists() {
            validation
                .problems
                .push(format!("missing fixture {}", path.display()));
            fixtures_problem = true;
            continue;
        }
        if !fixture.redacted {
            validation.problems.push(format!(
                "fixture {} is not marked redacted; bundle must not ship raw private payloads",
                fixture.path
            ));
            fixtures_problem = true;
        }
    }
    validation.fixtures_ok = !fixtures_problem;

    if !manifest.redaction.applied && !manifest.fixtures.is_empty() {
        validation
            .problems
            .push("redaction.applied is false but bundle includes fixtures".to_string());
    } else {
        validation.redaction_ok = true;
    }
    if !manifest
        .required_secrets
        .iter()
        .all(|secret| secret_id_looks_logical(secret))
    {
        validation.problems.push(
            "required_secrets contains a non-logical id (looks like a raw secret)".to_string(),
        );
    }

    Ok(validation)
}

/// Replay shadow comparison from a bundle: re-runs the deterministic
/// shadow check in-process against the bundle's redacted fixtures, with
/// no live side effects. Returns the manifest and the freshly computed
/// `ShadowRunReport`. The returned report is suitable for cloud import or
/// for asserting determinism in CI.
pub fn shadow_replay_bundle(
    bundle_dir: &Path,
) -> Result<(CrystallizationBundleManifest, ShadowRunReport), VmError> {
    let (manifest, traces) = load_crystallization_bundle(bundle_dir)?;
    let report_path = bundle_dir.join(BUNDLE_REPORT_FILE);
    let bytes = std::fs::read(&report_path).map_err(|error| {
        VmError::Runtime(format!(
            "failed to read bundle report {}: {error}",
            report_path.display()
        ))
    })?;
    let report: CrystallizationReport = serde_json::from_slice(&bytes).map_err(|error| {
        VmError::Runtime(format!(
            "failed to decode bundle report {}: {error}",
            report_path.display()
        ))
    })?;
    let candidate = report
        .selected_candidate_id
        .as_deref()
        .and_then(|id| report.candidates.iter().find(|c| c.id == id))
        .ok_or_else(|| {
            VmError::Runtime(format!(
                "bundle {} has no selected candidate to replay",
                bundle_dir.display()
            ))
        })?;
    let shadow = shadow_candidate(candidate, &traces);
    Ok((manifest, shadow))
}

fn write_bytes(path: &Path, bytes: &[u8]) -> Result<(), VmError> {
    crate::atomic_io::atomic_write(path, bytes)
        .map_err(|error| VmError::Runtime(format!("failed to write {}: {error}", path.display())))
}

fn sanitize_fixture_name(raw: &str) -> String {
    let cleaned = raw
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    if cleaned.trim_matches('_').is_empty() {
        "trace".to_string()
    } else {
        cleaned.trim_matches('_').to_string()
    }
}

fn sanitize_external_key(raw: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in raw.chars() {
        let lowered = ch.to_ascii_lowercase();
        if lowered.is_ascii_alphanumeric() {
            out.push(lowered);
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "crystallized-workflow".to_string()
    } else {
        trimmed
    }
}

fn infer_bundle_title(candidate: Option<&WorkflowCandidate>, fallback_name: &str) -> String {
    if let Some(candidate) = candidate {
        format!(
            "{} ({} step{})",
            candidate.name,
            candidate.steps.len(),
            if candidate.steps.len() == 1 { "" } else { "s" }
        )
    } else {
        format!("rejected: {fallback_name}")
    }
}

fn infer_risk_level(candidate: Option<&WorkflowCandidate>) -> String {
    let Some(candidate) = candidate else {
        return "high".to_string();
    };
    let touches_external = candidate.side_effects.iter().any(side_effect_is_external);
    let needs_secret = !candidate.required_secrets.is_empty();
    if touches_external && needs_secret {
        "high".to_string()
    } else if touches_external || needs_secret {
        "medium".to_string()
    } else {
        "low".to_string()
    }
}

fn side_effect_is_external(effect: &CrystallizationSideEffect) -> bool {
    let kind = effect.kind.to_ascii_lowercase();
    if kind.is_empty() {
        return false;
    }
    // Plan-only side effects stay inside Harn's own data plane: they
    // write receipts, append to the in-process event log, or stash plans.
    // None of those touch tenant-external systems.
    let internal = kind.contains("receipt")
        || kind.contains("event_log")
        || kind.contains("memo")
        || kind.contains("plan");
    if internal {
        return false;
    }
    kind.contains("post")
        || kind.contains("write")
        || kind.contains("publish")
        || kind.contains("delete")
        || kind.contains("send")
}

fn candidate_is_plan_only(candidate: &WorkflowCandidate) -> bool {
    if candidate.steps.is_empty() {
        return false;
    }
    candidate.side_effects.iter().all(|effect| {
        let kind = effect.kind.to_ascii_lowercase();
        // Plan-only side effects stay inside Harn's own data plane: receipt
        // writes, in-memory event-log appends, file-only mutations, etc.
        kind.is_empty()
            || kind.contains("receipt")
            || kind.contains("event_log")
            || kind.contains("memo")
            || kind.contains("plan")
            || (kind.contains("file") && !kind.contains("publish"))
    })
}

pub(super) fn redact_trace_for_bundle(trace: &mut CrystallizationTrace) {
    for action in &mut trace.actions {
        redact_bundle_value(&mut action.inputs);
        if let Some(output) = action.output.as_mut() {
            redact_bundle_value(output);
        }
        if let Some(observed) = action.observed_output.as_mut() {
            redact_bundle_value(observed);
        }
        for value in action.parameters.values_mut() {
            redact_bundle_value(value);
        }
        for (_, value) in action.metadata.iter_mut() {
            redact_bundle_value(value);
        }
    }
    for (_, value) in trace.metadata.iter_mut() {
        redact_bundle_value(value);
    }
    if let Some(run) = trace.replay_run.as_mut() {
        redact_replay_run_for_bundle(run);
    }
}

fn redact_replay_run_for_bundle(run: &mut ReplayTraceRun) {
    for value in run
        .event_log_entries
        .iter_mut()
        .chain(run.trigger_firings.iter_mut())
        .chain(run.llm_interactions.iter_mut())
        .chain(run.protocol_interactions.iter_mut())
        .chain(run.approval_interactions.iter_mut())
        .chain(run.effect_receipts.iter_mut())
        .chain(run.agent_transcript_deltas.iter_mut())
        .chain(run.final_artifacts.iter_mut())
        .chain(run.policy_decisions.iter_mut())
    {
        redact_bundle_value(value);
    }
}

fn redact_bundle_value(value: &mut JsonValue) {
    match value {
        JsonValue::String(text) if looks_like_secret_value(text) => {
            *text = "[redacted]".to_string();
        }
        JsonValue::Array(items) => {
            for item in items {
                redact_bundle_value(item);
            }
        }
        JsonValue::Object(map) => {
            for (key, child) in map.iter_mut() {
                if is_sensitive_bundle_key(key) {
                    *child = JsonValue::String("[redacted]".to_string());
                } else {
                    redact_bundle_value(child);
                }
            }
        }
        _ => {}
    }
}

fn is_sensitive_bundle_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    lower.contains("secret")
        || lower.contains("token")
        || lower.contains("password")
        || lower.contains("api_key")
        || lower.contains("apikey")
        || lower == "authorization"
        || lower == "cookie"
        || lower == "set-cookie"
}

fn looks_like_secret_value(value: &str) -> bool {
    let trimmed = value.trim();
    trimmed.starts_with("sk-")
        || trimmed.starts_with("ghp_")
        || trimmed.starts_with("ghs_")
        || trimmed.starts_with("xoxb-")
        || trimmed.starts_with("xoxp-")
        || trimmed.starts_with("AKIA")
        || (trimmed.len() > 48
            && trimmed
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-'))
}

fn secret_id_looks_logical(value: &str) -> bool {
    !looks_like_secret_value(value) && !value.trim().is_empty()
}
