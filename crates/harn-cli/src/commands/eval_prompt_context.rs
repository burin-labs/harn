use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use harn_vm::orchestration::{
    assemble_context, render_assembled_chunks, ArtifactRecord, AssembleDedup, AssembleOptions,
    AssembleStrategy, AssembledContext,
};
use harn_vm::stdlib::template::{
    render_template_to_string_with_branch_trace, BranchDecision, LlmRenderContext,
    LlmRenderContextGuard,
};
use harn_vm::value::VmValue;
use serde_json::{json, Value as JsonValue};

use super::eval_prompt::FleetEntry;

#[derive(Debug, Clone, serde::Deserialize, Default)]
#[serde(default)]
struct PromptContextFixture {
    #[serde(rename = "_type")]
    type_name: Option<String>,
    version: Option<u32>,
    id: Option<String>,
    name: Option<String>,
    cases: Vec<PromptContextCase>,
}

#[derive(Debug, Clone, serde::Deserialize, Default)]
#[serde(default)]
struct PromptContextCase {
    id: Option<String>,
    name: Option<String>,
    description: Option<String>,
    bindings: Option<JsonValue>,
    artifacts: Vec<ArtifactRecord>,
    assembler: ContextAssemblerSpec,
    expect: ContextExpectation,
}

#[derive(Debug, Clone, serde::Deserialize, Default)]
#[serde(default)]
struct ContextAssemblerSpec {
    #[serde(alias = "budget-tokens")]
    budget_tokens: Option<usize>,
    dedup: Option<String>,
    strategy: Option<String>,
    query: Option<String>,
    #[serde(alias = "microcompact-threshold")]
    microcompact_threshold: Option<usize>,
    #[serde(alias = "semantic-overlap")]
    semantic_overlap: Option<f64>,
}

#[derive(Debug, Clone, serde::Deserialize, Default)]
#[serde(default)]
struct ContextExpectation {
    #[serde(alias = "selected-artifact-ids")]
    selected_artifact_ids: Vec<String>,
    #[serde(alias = "rejected-artifact-ids")]
    rejected_artifact_ids: Vec<String>,
    #[serde(alias = "stale-artifact-ids")]
    stale_artifact_ids: Vec<String>,
    #[serde(alias = "max-total-tokens")]
    max_total_tokens: Option<usize>,
    #[serde(alias = "required-section-names")]
    required_section_names: Vec<String>,
    #[serde(alias = "section-envelopes-by-family")]
    section_envelopes_by_family: BTreeMap<String, BTreeMap<String, String>>,
    #[serde(alias = "section-envelopes-by-selector")]
    section_envelopes_by_selector: BTreeMap<String, BTreeMap<String, String>>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct PromptContextEvalReport {
    pub(crate) pass: bool,
    pub(crate) total: usize,
    pub(crate) passed: usize,
    pub(crate) failed: usize,
    pub(crate) fixtures: Vec<PromptContextFixtureReport>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct PromptContextFixtureReport {
    pub(crate) path: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) name: Option<String>,
    pub(crate) pass: bool,
    pub(crate) total: usize,
    pub(crate) passed: usize,
    pub(crate) failed: usize,
    pub(crate) cases: Vec<PromptContextCaseReport>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct PromptContextCaseReport {
    pub(crate) id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) description: Option<String>,
    pub(crate) pass: bool,
    pub(crate) score: ContextScoreBreakdown,
    pub(crate) selected_artifact_ids: Vec<String>,
    pub(crate) dropped_artifact_ids: Vec<String>,
    pub(crate) stale_artifact_ids: Vec<String>,
    pub(crate) budget: ContextBudgetReport,
    pub(crate) variants: Vec<ContextVariantReport>,
    pub(crate) failures: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct ContextScoreBreakdown {
    pub(crate) overall: f64,
    pub(crate) selected_artifacts: f64,
    pub(crate) rejected_artifacts: f64,
    pub(crate) stale_rejection: f64,
    pub(crate) token_budget: f64,
    pub(crate) rendered_sections: f64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct ContextBudgetReport {
    pub(crate) total_tokens: usize,
    pub(crate) budget_tokens: usize,
    pub(crate) max_total_tokens: usize,
    pub(crate) pass: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct ContextVariantReport {
    pub(crate) selector: String,
    pub(crate) provider: String,
    pub(crate) model: String,
    pub(crate) family: String,
    pub(crate) rendered_bytes: usize,
    pub(crate) pass: bool,
    pub(crate) sections: Vec<ContextSectionShape>,
    pub(crate) failures: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct ContextSectionShape {
    pub(crate) name: String,
    pub(crate) envelope: String,
    pub(crate) line: usize,
    pub(crate) col: usize,
}

pub(crate) fn evaluate_context_fixtures(
    paths: &[PathBuf],
    fleet: &[FleetEntry],
    template_source: &str,
    template_path: &Path,
    base_bindings: Option<&VmValue>,
) -> Result<PromptContextEvalReport, String> {
    let mut fixtures = Vec::new();
    for path in paths {
        let fixture = load_context_fixture(path)?;
        let mut cases = Vec::new();
        for (index, case) in fixture.cases.iter().enumerate() {
            cases.push(evaluate_context_case(
                case,
                index,
                fleet,
                template_source,
                template_path,
                base_bindings,
            )?);
        }
        let total = cases.len();
        let passed = cases.iter().filter(|case| case.pass).count();
        let failed = total.saturating_sub(passed);
        fixtures.push(PromptContextFixtureReport {
            path: path.clone(),
            id: fixture.id,
            name: fixture.name,
            pass: failed == 0,
            total,
            passed,
            failed,
            cases,
        });
    }

    let total = fixtures.iter().map(|fixture| fixture.total).sum();
    let passed = fixtures.iter().map(|fixture| fixture.passed).sum();
    let failed = fixtures.iter().map(|fixture| fixture.failed).sum();
    Ok(PromptContextEvalReport {
        pass: failed == 0,
        total,
        passed,
        failed,
        fixtures,
    })
}

fn load_context_fixture(path: &Path) -> Result<PromptContextFixture, String> {
    let raw = fs::read_to_string(path)
        .map_err(|error| format!("failed to read context fixture {}: {error}", path.display()))?;
    let fixture: PromptContextFixture = serde_json::from_str(&raw).map_err(|error| {
        format!(
            "failed to parse context fixture {}: {error}",
            path.display()
        )
    })?;
    if let Some(kind) = fixture.type_name.as_deref() {
        if kind != "prompt_context_eval_fixture" {
            return Err(format!(
                "context fixture {} has unsupported _type `{kind}`",
                path.display(),
            ));
        }
    }
    if let Some(version) = fixture.version {
        if version != 1 {
            return Err(format!(
                "context fixture {} has unsupported version {version}",
                path.display(),
            ));
        }
    }
    if fixture.cases.is_empty() {
        return Err(format!(
            "context fixture {} must declare at least one case",
            path.display()
        ));
    }
    for (index, case) in fixture.cases.iter().enumerate() {
        if !case.has_context_assertion() {
            let case_label = case
                .id
                .as_deref()
                .filter(|id| !id.trim().is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| format!("case_{}", index + 1));
            return Err(format!(
                "context fixture {} case `{case_label}` must declare at least one expectation",
                path.display(),
            ));
        }
    }
    Ok(fixture)
}

impl PromptContextCase {
    fn has_context_assertion(&self) -> bool {
        !self.expect.selected_artifact_ids.is_empty()
            || !self.expect.rejected_artifact_ids.is_empty()
            || !self.expect.stale_artifact_ids.is_empty()
            || self.expect.max_total_tokens.is_some()
            || !self.expect.required_section_names.is_empty()
            || !self.expect.section_envelopes_by_family.is_empty()
            || !self.expect.section_envelopes_by_selector.is_empty()
    }
}

fn evaluate_context_case(
    case: &PromptContextCase,
    index: usize,
    fleet: &[FleetEntry],
    template_source: &str,
    template_path: &Path,
    base_bindings: Option<&VmValue>,
) -> Result<PromptContextCaseReport, String> {
    let id = case
        .id
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| format!("case_{}", index + 1));
    let artifacts: Vec<ArtifactRecord> = case
        .artifacts
        .clone()
        .into_iter()
        .map(ArtifactRecord::normalize)
        .collect();
    let options = context_assemble_options(&case.assembler)?;
    let assembled = assemble_context(&artifacts, &options, None);
    let selected_artifact_ids = selected_artifact_ids(&assembled);
    let dropped_artifact_ids = dropped_artifact_ids(&assembled);
    let stale_artifact_ids = expected_stale_artifact_ids(case, &artifacts);

    let mut failures = Vec::new();
    if artifacts.is_empty() {
        failures.push("case has no candidate artifacts".to_string());
    }

    let selected_score = score_expected_present(
        "selected artifact",
        &case.expect.selected_artifact_ids,
        &selected_artifact_ids,
        &mut failures,
    );
    let rejected_score = score_expected_absent(
        "rejected artifact",
        &case.expect.rejected_artifact_ids,
        &selected_artifact_ids,
        &mut failures,
    );
    let stale_score = score_expected_absent(
        "stale artifact",
        &stale_artifact_ids,
        &selected_artifact_ids,
        &mut failures,
    );

    let max_total_tokens = case
        .expect
        .max_total_tokens
        .unwrap_or(options.budget_tokens);
    let budget_pass = assembled.total_tokens <= options.budget_tokens
        && assembled.total_tokens <= max_total_tokens;
    if !budget_pass {
        failures.push(format!(
            "assembled context used {} tokens; expected <= {} and budget <= {}",
            assembled.total_tokens, max_total_tokens, options.budget_tokens,
        ));
    }
    let budget = ContextBudgetReport {
        total_tokens: assembled.total_tokens,
        budget_tokens: options.budget_tokens,
        max_total_tokens,
        pass: budget_pass,
    };

    let bindings = context_case_bindings(
        base_bindings,
        case.bindings.as_ref(),
        &artifacts,
        &assembled,
        &selected_artifact_ids,
        &dropped_artifact_ids,
    )?;
    let variants = render_context_variants(
        fleet,
        template_source,
        template_path,
        &bindings,
        &case.expect,
    );
    for variant in &variants {
        failures.extend(
            variant
                .failures
                .iter()
                .map(|failure| format!("{}: {failure}", variant.selector)),
        );
    }
    let rendered_sections_score = if variants.is_empty() {
        failures.push("fleet is empty for context fixture case".to_string());
        0.0
    } else {
        variants.iter().filter(|variant| variant.pass).count() as f64 / variants.len() as f64
    };

    let token_budget_score = if budget.pass { 1.0 } else { 0.0 };
    let overall = average_scores(&[
        selected_score,
        rejected_score,
        stale_score,
        token_budget_score,
        rendered_sections_score,
    ]);
    let score = ContextScoreBreakdown {
        overall,
        selected_artifacts: selected_score,
        rejected_artifacts: rejected_score,
        stale_rejection: stale_score,
        token_budget: token_budget_score,
        rendered_sections: rendered_sections_score,
    };

    Ok(PromptContextCaseReport {
        id,
        name: case.name.clone(),
        description: case.description.clone(),
        pass: failures.is_empty(),
        score,
        selected_artifact_ids,
        dropped_artifact_ids,
        stale_artifact_ids,
        budget,
        variants,
        failures,
    })
}

fn context_assemble_options(spec: &ContextAssemblerSpec) -> Result<AssembleOptions, String> {
    let mut options = AssembleOptions::default();
    if let Some(value) = spec.budget_tokens {
        options.budget_tokens = value;
    }
    if let Some(value) = spec.microcompact_threshold {
        options.microcompact_threshold = value;
    }
    if let Some(value) = spec.semantic_overlap {
        if !(0.0..=1.0).contains(&value) {
            return Err("context fixture semantic_overlap must be in [0.0, 1.0]".to_string());
        }
        options.semantic_overlap = value;
    }
    if let Some(value) = spec.dedup.as_deref() {
        options.dedup = AssembleDedup::parse(value)?;
    }
    if let Some(value) = spec.strategy.as_deref() {
        options.strategy = AssembleStrategy::parse(value)?;
    }
    if let Some(query) = spec.query.as_ref().filter(|query| !query.trim().is_empty()) {
        options.query = Some(query.clone());
    }
    Ok(options)
}

fn selected_artifact_ids(assembled: &AssembledContext) -> Vec<String> {
    unique_preserve_order(
        assembled
            .chunks
            .iter()
            .map(|chunk| chunk.artifact_id.clone()),
    )
}

fn dropped_artifact_ids(assembled: &AssembledContext) -> Vec<String> {
    unique_preserve_order(
        assembled
            .dropped
            .iter()
            .map(|entry| entry.artifact_id.clone()),
    )
}

fn expected_stale_artifact_ids(
    case: &PromptContextCase,
    artifacts: &[ArtifactRecord],
) -> Vec<String> {
    unique_preserve_order(
        case.expect.stale_artifact_ids.iter().cloned().chain(
            artifacts
                .iter()
                .filter(|artifact| artifact.freshness.as_deref() == Some("stale"))
                .map(|artifact| artifact.id.clone()),
        ),
    )
}

fn unique_preserve_order(values: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for value in values {
        if seen.insert(value.clone()) {
            out.push(value);
        }
    }
    out
}

fn score_expected_present(
    label: &str,
    expected: &[String],
    actual: &[String],
    failures: &mut Vec<String>,
) -> f64 {
    if expected.is_empty() {
        return 1.0;
    }
    let actual: BTreeSet<&str> = actual.iter().map(String::as_str).collect();
    let mut matched = 0usize;
    for id in expected {
        if actual.contains(id.as_str()) {
            matched += 1;
        } else {
            failures.push(format!("missing expected {label} `{id}`"));
        }
    }
    matched as f64 / expected.len() as f64
}

fn score_expected_absent(
    label: &str,
    expected_absent: &[String],
    actual: &[String],
    failures: &mut Vec<String>,
) -> f64 {
    if expected_absent.is_empty() {
        return 1.0;
    }
    let actual: BTreeSet<&str> = actual.iter().map(String::as_str).collect();
    let mut absent = 0usize;
    for id in expected_absent {
        if actual.contains(id.as_str()) {
            failures.push(format!("selected forbidden {label} `{id}`"));
        } else {
            absent += 1;
        }
    }
    absent as f64 / expected_absent.len() as f64
}

fn average_scores(scores: &[f64]) -> f64 {
    if scores.is_empty() {
        return 1.0;
    }
    let raw = scores.iter().sum::<f64>() / scores.len() as f64;
    (raw * 1000.0).round() / 1000.0
}

fn context_case_bindings(
    base_bindings: Option<&VmValue>,
    case_bindings: Option<&JsonValue>,
    artifacts: &[ArtifactRecord],
    assembled: &AssembledContext,
    selected_artifact_ids: &[String],
    dropped_artifact_ids: &[String],
) -> Result<harn_vm::value::DictMap, String> {
    let mut bindings = match base_bindings {
        Some(VmValue::Dict(dict)) => dict.as_ref().clone(),
        Some(other) => {
            return Err(format!(
                "context fixture base bindings must be a dict, got {}",
                other.type_name()
            ));
        }
        None => harn_vm::value::DictMap::new(),
    };
    if let Some(case_bindings) = case_bindings {
        let object = case_bindings
            .as_object()
            .ok_or_else(|| "context fixture case bindings must be a JSON object".to_string())?;
        for (key, value) in object {
            bindings.insert(key.clone(), harn_vm::json_to_vm_value(value));
        }
    }

    let context_text = render_assembled_chunks(assembled);
    bindings.insert(
        "candidate_artifacts".to_string(),
        harn_vm::json_to_vm_value(
            &serde_json::to_value(artifacts)
                .map_err(|error| format!("failed to serialize candidate artifacts: {error}"))?,
        ),
    );
    bindings.insert(
        "assembled_context".to_string(),
        harn_vm::json_to_vm_value(&assembled_context_to_json(assembled)),
    );
    bindings.insert(
        "context".to_string(),
        harn_vm::json_to_vm_value(&JsonValue::String(context_text)),
    );
    bindings.insert(
        "selected_artifact_ids".to_string(),
        harn_vm::json_to_vm_value(&json!(selected_artifact_ids)),
    );
    bindings.insert(
        "dropped_artifact_ids".to_string(),
        harn_vm::json_to_vm_value(&json!(dropped_artifact_ids)),
    );
    Ok(bindings)
}

fn assembled_context_to_json(assembled: &AssembledContext) -> JsonValue {
    json!({
        "chunks": assembled.chunks.iter().map(|chunk| {
            json!({
                "id": &chunk.id,
                "artifact_id": &chunk.artifact_id,
                "artifact_kind": &chunk.artifact_kind,
                "title": &chunk.title,
                "source": &chunk.source,
                "text": &chunk.text,
                "estimated_tokens": chunk.estimated_tokens,
                "chunk_index": chunk.chunk_index,
                "chunk_count": chunk.chunk_count,
                "score": chunk.score,
            })
        }).collect::<Vec<_>>(),
        "included": assembled.included.iter().map(|summary| {
            json!({
                "artifact_id": &summary.artifact_id,
                "artifact_kind": &summary.artifact_kind,
                "chunks_included": summary.chunks_included,
                "chunks_total": summary.chunks_total,
                "tokens_included": summary.tokens_included,
            })
        }).collect::<Vec<_>>(),
        "dropped": assembled.dropped.iter().map(|entry| {
            json!({
                "artifact_id": &entry.artifact_id,
                "chunk_id": &entry.chunk_id,
                "reason": entry.reason,
                "detail": &entry.detail,
            })
        }).collect::<Vec<_>>(),
        "reasons": assembled.reasons.iter().map(|reason| {
            json!({
                "chunk_id": &reason.chunk_id,
                "artifact_id": &reason.artifact_id,
                "strategy": reason.strategy,
                "score": reason.score,
                "included": reason.included,
                "reason": reason.reason,
            })
        }).collect::<Vec<_>>(),
        "total_tokens": assembled.total_tokens,
        "budget_tokens": assembled.budget_tokens,
        "strategy": assembled.strategy.as_str(),
        "dedup": assembled.dedup.as_str(),
    })
}

fn render_context_variants(
    fleet: &[FleetEntry],
    template_source: &str,
    template_path: &Path,
    bindings: &harn_vm::value::DictMap,
    expect: &ContextExpectation,
) -> Vec<ContextVariantReport> {
    let base = template_path.parent();
    fleet
        .iter()
        .map(|entry| {
            let ctx = LlmRenderContext::resolve(&entry.provider, &entry.model);
            let family = ctx.family.clone();
            let result = {
                let _guard = LlmRenderContextGuard::enter(ctx);
                render_template_to_string_with_branch_trace(
                    template_source,
                    Some(bindings),
                    base,
                    Some(template_path),
                )
            };
            match result {
                Ok((rendered, trace)) => {
                    let sections = section_shapes(&trace);
                    let failures = evaluate_section_shape(entry, &family, &sections, expect);
                    ContextVariantReport {
                        selector: entry.selector.clone(),
                        provider: entry.provider.clone(),
                        model: entry.model.clone(),
                        family,
                        rendered_bytes: rendered.len(),
                        pass: failures.is_empty(),
                        sections,
                        failures,
                    }
                }
                Err(error) => ContextVariantReport {
                    selector: entry.selector.clone(),
                    provider: entry.provider.clone(),
                    model: entry.model.clone(),
                    family,
                    rendered_bytes: 0,
                    pass: false,
                    sections: Vec::new(),
                    failures: vec![format!("template render failed: {error}")],
                },
            }
        })
        .collect()
}

fn section_shapes(trace: &[BranchDecision]) -> Vec<ContextSectionShape> {
    trace
        .iter()
        .filter_map(|decision| {
            if decision.kind.as_str() != "section" {
                return None;
            }
            Some(ContextSectionShape {
                name: decision.branch_label.clone().unwrap_or_default(),
                envelope: decision.branch_id.clone(),
                line: decision.line,
                col: decision.col,
            })
        })
        .collect()
}

fn evaluate_section_shape(
    entry: &FleetEntry,
    family: &str,
    sections: &[ContextSectionShape],
    expect: &ContextExpectation,
) -> Vec<String> {
    let mut failures = Vec::new();
    let sections_by_name: BTreeMap<&str, &ContextSectionShape> = sections
        .iter()
        .map(|section| (section.name.as_str(), section))
        .collect();

    for required in &expect.required_section_names {
        if !sections_by_name.contains_key(required.as_str()) {
            failures.push(format!("missing logical section `{required}`"));
        }
    }

    let expected_envelopes = expect
        .section_envelopes_by_selector
        .get(&entry.selector)
        .or_else(|| expect.section_envelopes_by_family.get(family));
    if let Some(expected_envelopes) = expected_envelopes {
        for (section_name, expected_envelope) in expected_envelopes {
            match sections_by_name.get(section_name.as_str()) {
                Some(section) if &section.envelope == expected_envelope => {}
                Some(section) => failures.push(format!(
                    "section `{section_name}` envelope `{}` != expected `{expected_envelope}`",
                    section.envelope,
                )),
                None => failures.push(format!(
                    "missing logical section `{section_name}` for envelope check"
                )),
            }
        }
    }
    failures
}
