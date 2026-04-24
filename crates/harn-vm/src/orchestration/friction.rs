//! Friction events, context-pack manifests, and suggestion generation.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::{new_id, now_rfc3339, parse_json_payload};
use crate::llm::vm_value_to_json;
use crate::value::{VmError, VmValue};

pub const FRICTION_SCHEMA_VERSION: u32 = 1;
pub const CONTEXT_PACK_MANIFEST_VERSION: u32 = 1;

const FRICTION_KINDS: &[&str] = &[
    "repeated_query",
    "repeated_clarification",
    "approval_stall",
    "missing_context",
    "manual_handoff",
    "tool_gap",
    "failed_assumption",
    "expensive_model_used_for_deterministic_step",
    "human_hypothesis",
];

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct FrictionEvent {
    pub schema_version: u32,
    pub id: String,
    pub kind: String,
    pub source: Option<String>,
    pub actor: Option<String>,
    pub tenant_id: Option<String>,
    pub task_id: Option<String>,
    pub run_id: Option<String>,
    pub workflow_id: Option<String>,
    pub tool: Option<String>,
    pub provider: Option<String>,
    pub redacted_summary: String,
    pub estimated_cost_usd: Option<f64>,
    pub estimated_time_ms: Option<i64>,
    pub recurrence_hints: Vec<String>,
    pub trace_id: Option<String>,
    pub span_id: Option<String>,
    pub links: Vec<FrictionLink>,
    pub human_hypothesis: Option<HumanHypothesis>,
    pub metadata: BTreeMap<String, serde_json::Value>,
    pub timestamp: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct FrictionLink {
    pub label: Option<String>,
    pub url: Option<String>,
    pub trace_id: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct HumanHypothesis {
    pub note: String,
    pub confidence: Option<f64>,
    pub expires_at: Option<String>,
    pub checkback_at: Option<String>,
    pub suggested_verification_tools: Vec<String>,
    pub status: Option<String>,
    pub evidence_outcome: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ContextPackManifest {
    pub version: u32,
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub owner: String,
    pub triggers: Vec<ContextPackTrigger>,
    pub inputs: Vec<ContextPackInput>,
    pub included_queries: Vec<ContextPackQuery>,
    pub included_docs: Vec<ContextPackDoc>,
    pub included_tools: Vec<ContextPackTool>,
    pub refresh_policy: ContextPackRefreshPolicy,
    pub secrets: Vec<ContextPackSecretRef>,
    pub capabilities: Vec<String>,
    pub output_slots: Vec<ContextPackOutputSlot>,
    pub fallback_instructions: Option<String>,
    pub review: Option<ContextPackReviewPolicy>,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ContextPackTrigger {
    pub kind: String,
    pub source: Option<String>,
    pub match_hint: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ContextPackInput {
    pub name: String,
    pub description: Option<String>,
    pub required: bool,
    pub source: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ContextPackQuery {
    pub id: String,
    pub label: Option<String>,
    pub provider: Option<String>,
    pub query: String,
    pub filters: BTreeMap<String, String>,
    pub output_slot: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ContextPackDoc {
    pub id: String,
    pub title: Option<String>,
    pub url: Option<String>,
    pub path: Option<String>,
    pub freshness: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ContextPackTool {
    pub name: String,
    pub capability: Option<String>,
    pub purpose: Option<String>,
    pub deterministic: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ContextPackRefreshPolicy {
    pub mode: String,
    pub interval: Option<String>,
    pub stale_after: Option<String>,
}

impl Default for ContextPackRefreshPolicy {
    fn default() -> Self {
        Self {
            mode: "on_demand".to_string(),
            interval: None,
            stale_after: None,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ContextPackSecretRef {
    pub name: String,
    pub capability: Option<String>,
    pub required: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ContextPackOutputSlot {
    pub name: String,
    pub description: Option<String>,
    pub artifact_kind: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ContextPackReviewPolicy {
    pub owner: Option<String>,
    pub approval_required: bool,
    pub privacy_notes: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ContextPackSuggestion {
    #[serde(rename = "_type")]
    pub type_name: String,
    pub id: String,
    pub title: String,
    pub recommended_artifact: String,
    pub confidence: f64,
    pub candidate_manifest: ContextPackManifest,
    pub evidence: Vec<ContextPackSuggestionEvidence>,
    pub examples: Vec<String>,
    pub estimated_savings: ContextPackEstimatedSavings,
    pub risk_privacy_notes: Vec<String>,
    pub source_event_ids: Vec<String>,
    pub created_at: String,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ContextPackSuggestionEvidence {
    pub event_id: String,
    pub kind: String,
    pub source: Option<String>,
    pub tool: Option<String>,
    pub provider: Option<String>,
    pub redacted_summary: String,
    pub run_id: Option<String>,
    pub trace_id: Option<String>,
    pub estimated_cost_usd: Option<f64>,
    pub estimated_time_ms: Option<i64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ContextPackEstimatedSavings {
    pub occurrences: usize,
    pub estimated_time_saved_ms: i64,
    pub estimated_cost_saved_usd: f64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ContextPackSuggestionOptions {
    pub min_occurrences: usize,
    pub owner: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ContextPackSuggestionExpectation {
    pub min_suggestions: Option<usize>,
    pub recommended_artifact: Option<String>,
    pub title_contains: Option<String>,
    pub manifest_name_contains: Option<String>,
    pub required_capability: Option<String>,
    pub required_output_slot: Option<String>,
}

pub fn friction_kind_allowed(kind: &str) -> bool {
    FRICTION_KINDS.contains(&kind)
}

pub fn normalize_friction_event(value: &VmValue) -> Result<FrictionEvent, VmError> {
    let mut json = vm_value_to_json(value);
    if let Some(map) = json.as_object_mut() {
        if !map.contains_key("redacted_summary") {
            if let Some(summary) = map
                .get("summary")
                .and_then(|value| value.as_str())
                .or_else(|| map.get("message").and_then(|value| value.as_str()))
            {
                map.insert(
                    "redacted_summary".to_string(),
                    serde_json::Value::String(redact_text(summary)),
                );
            }
        }
        map.remove("summary");
        map.remove("message");
        map.remove("raw_content");
        map.remove("raw_prompt");
        if let Some(metadata) = map.get_mut("metadata") {
            redact_json_value(metadata);
        }
    }
    normalize_friction_event_json(json)
}

pub fn normalize_friction_event_json(json: serde_json::Value) -> Result<FrictionEvent, VmError> {
    let mut event: FrictionEvent = parse_json_payload(json, "friction_event")?;
    if event.schema_version == 0 {
        event.schema_version = FRICTION_SCHEMA_VERSION;
    }
    if event.id.is_empty() {
        event.id = new_id("friction");
    }
    if event.timestamp.is_empty() {
        event.timestamp = now_rfc3339();
    }
    event.kind = event.kind.trim().to_ascii_lowercase();
    if event.kind.is_empty() {
        return Err(VmError::Runtime("friction_event: missing kind".to_string()));
    }
    if !friction_kind_allowed(&event.kind) {
        return Err(VmError::Runtime(format!(
            "friction_event: unsupported kind '{}' (expected one of {})",
            event.kind,
            FRICTION_KINDS.join(", ")
        )));
    }
    event.redacted_summary = redact_text(&event.redacted_summary);
    if event.redacted_summary.trim().is_empty() {
        return Err(VmError::Runtime(
            "friction_event: missing redacted_summary".to_string(),
        ));
    }
    for value in event.metadata.values_mut() {
        redact_json_value(value);
    }
    Ok(event)
}

pub fn normalize_context_pack_manifest(value: &VmValue) -> Result<ContextPackManifest, VmError> {
    normalize_context_pack_manifest_json(vm_value_to_json(value))
}

pub fn normalize_context_pack_manifest_json(
    json: serde_json::Value,
) -> Result<ContextPackManifest, VmError> {
    let mut manifest: ContextPackManifest = parse_json_payload(json, "context_pack_manifest")?;
    normalize_context_pack_manifest_record(&mut manifest)?;
    Ok(manifest)
}

pub fn parse_context_pack_manifest_src(src: &str) -> Result<ContextPackManifest, VmError> {
    let trimmed = src.trim_start();
    let mut manifest: ContextPackManifest = if trimmed.starts_with('{') {
        serde_json::from_str(src).map_err(|e| {
            VmError::Runtime(format!("context_pack_manifest_parse: invalid JSON: {e}"))
        })?
    } else {
        toml::from_str(src).map_err(|e| {
            VmError::Runtime(format!("context_pack_manifest_parse: invalid TOML: {e}"))
        })?
    };
    normalize_context_pack_manifest_record(&mut manifest)?;
    Ok(manifest)
}

pub fn generate_context_pack_suggestions(
    events: &[FrictionEvent],
    options: &ContextPackSuggestionOptions,
) -> Vec<ContextPackSuggestion> {
    let min_occurrences = options.min_occurrences.max(2);
    let mut groups: BTreeMap<String, Vec<FrictionEvent>> = BTreeMap::new();
    for event in events {
        groups
            .entry(friction_group_key(event))
            .or_default()
            .push(event.clone());
    }

    groups
        .into_values()
        .filter(|group| group.len() >= min_occurrences)
        .map(|group| build_suggestion(group, options))
        .collect()
}

pub fn evaluate_context_pack_suggestion_expectations(
    suggestions: &[ContextPackSuggestion],
    expectations: &[ContextPackSuggestionExpectation],
) -> Vec<String> {
    let mut failures = Vec::new();
    for expectation in expectations {
        if let Some(min) = expectation.min_suggestions {
            if suggestions.len() < min {
                failures.push(format!(
                    "expected at least {min} context-pack suggestion(s), got {}",
                    suggestions.len()
                ));
                continue;
            }
        }
        if expectation_has_match(expectation, suggestions) {
            continue;
        }
        failures.push(format!(
            "no context-pack suggestion matched expectation {:?}",
            expectation
        ));
    }
    failures
}

pub fn normalize_friction_events_json(
    value: serde_json::Value,
) -> Result<Vec<FrictionEvent>, VmError> {
    let items = if let Some(events) = value.get("events").and_then(|events| events.as_array()) {
        events.clone()
    } else if let Some(array) = value.as_array() {
        array.clone()
    } else {
        return Err(VmError::Runtime(
            "friction events fixture must be an array or {events: [...]}".to_string(),
        ));
    };
    items
        .into_iter()
        .map(normalize_friction_event_json)
        .collect()
}

pub fn parse_friction_events_value(value: &VmValue) -> Result<Vec<FrictionEvent>, VmError> {
    normalize_friction_events_json(vm_value_to_json(value))
}

fn normalize_context_pack_manifest_record(
    manifest: &mut ContextPackManifest,
) -> Result<(), VmError> {
    if manifest.version == 0 {
        manifest.version = CONTEXT_PACK_MANIFEST_VERSION;
    }
    if manifest.name.trim().is_empty() {
        return Err(VmError::Runtime(
            "context_pack_manifest: missing name".to_string(),
        ));
    }
    if manifest.id.trim().is_empty() {
        manifest.id = slugify(&manifest.name);
    }
    if manifest.owner.trim().is_empty() {
        return Err(VmError::Runtime(
            "context_pack_manifest: missing owner".to_string(),
        ));
    }
    for secret in &manifest.secrets {
        if looks_like_secret_value(&secret.name)
            || looks_like_secret_value(secret.capability.as_deref().unwrap_or(""))
        {
            return Err(VmError::Runtime(
                "context_pack_manifest: secrets must be capability references, not raw secret values"
                    .to_string(),
            ));
        }
    }
    for query in &manifest.included_queries {
        if query.id.trim().is_empty() || query.query.trim().is_empty() {
            return Err(VmError::Runtime(
                "context_pack_manifest: included queries require id and query".to_string(),
            ));
        }
    }
    Ok(())
}

fn build_suggestion(
    mut group: Vec<FrictionEvent>,
    options: &ContextPackSuggestionOptions,
) -> ContextPackSuggestion {
    group.sort_by(|left, right| {
        left.timestamp
            .cmp(&right.timestamp)
            .then(left.id.cmp(&right.id))
    });
    let first = group.first().expect("filtered non-empty group");
    let title = suggestion_title(first);
    let recommended_artifact = recommended_artifact_for_kind(&first.kind).to_string();
    let evidence = group
        .iter()
        .map(|event| ContextPackSuggestionEvidence {
            event_id: event.id.clone(),
            kind: event.kind.clone(),
            source: event.source.clone(),
            tool: event.tool.clone(),
            provider: event.provider.clone(),
            redacted_summary: event.redacted_summary.clone(),
            run_id: event.run_id.clone(),
            trace_id: event.trace_id.clone(),
            estimated_cost_usd: event.estimated_cost_usd,
            estimated_time_ms: event.estimated_time_ms,
        })
        .collect::<Vec<_>>();
    let examples = group
        .iter()
        .map(|event| event.redacted_summary.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .take(3)
        .collect::<Vec<_>>();
    let candidate_manifest =
        candidate_manifest_for_group(&title, &recommended_artifact, &group, options);
    let occurrences = group.len();
    let estimated_time_saved_ms = group
        .iter()
        .skip(1)
        .filter_map(|event| event.estimated_time_ms)
        .sum();
    let estimated_cost_saved_usd = group
        .iter()
        .skip(1)
        .filter_map(|event| event.estimated_cost_usd)
        .sum();
    let source_event_ids = group.iter().map(|event| event.id.clone()).collect();
    ContextPackSuggestion {
        type_name: "context_pack_suggestion".to_string(),
        id: new_id("context_pack_suggestion"),
        title,
        recommended_artifact,
        confidence: confidence_for_occurrences(occurrences),
        candidate_manifest,
        evidence,
        examples,
        estimated_savings: ContextPackEstimatedSavings {
            occurrences,
            estimated_time_saved_ms,
            estimated_cost_saved_usd,
        },
        risk_privacy_notes: vec![
            "Evidence uses redacted summaries; raw prompts, raw content, and secret-looking metadata are not retained.".to_string(),
            "Review required before enabling this context pack for future runs.".to_string(),
        ],
        source_event_ids,
        created_at: now_rfc3339(),
        metadata: BTreeMap::new(),
    }
}

fn candidate_manifest_for_group(
    title: &str,
    recommended_artifact: &str,
    group: &[FrictionEvent],
    options: &ContextPackSuggestionOptions,
) -> ContextPackManifest {
    let first = group.first().expect("non-empty suggestion group");
    let mut included_queries = Vec::new();
    let mut included_docs = Vec::new();
    let mut included_tools = Vec::new();
    let mut capabilities = BTreeSet::new();
    let mut secrets = BTreeMap::<String, ContextPackSecretRef>::new();
    let mut output_slots = BTreeMap::<String, ContextPackOutputSlot>::new();

    for event in group {
        if let Some(query) = metadata_string(event, "query")
            .or_else(|| metadata_string(event, "deterministic_query"))
        {
            let id = format!("query_{}", included_queries.len() + 1);
            included_queries.push(ContextPackQuery {
                id: id.clone(),
                label: metadata_string(event, "query_label").or_else(|| event.tool.clone()),
                provider: event.provider.clone().or_else(|| event.tool.clone()),
                query,
                filters: metadata_string_map(event, "filters"),
                output_slot: metadata_string(event, "output_slot")
                    .or_else(|| Some("primary_context".to_string())),
            });
        }
        if let Some(doc) =
            metadata_string(event, "doc_url").or_else(|| metadata_string(event, "document_url"))
        {
            included_docs.push(ContextPackDoc {
                id: format!("doc_{}", included_docs.len() + 1),
                title: metadata_string(event, "doc_title"),
                url: Some(doc),
                path: None,
                freshness: metadata_string(event, "freshness"),
            });
        }
        if let Some(path) =
            metadata_string(event, "doc_path").or_else(|| metadata_string(event, "document_path"))
        {
            included_docs.push(ContextPackDoc {
                id: format!("doc_{}", included_docs.len() + 1),
                title: metadata_string(event, "doc_title"),
                url: None,
                path: Some(path),
                freshness: metadata_string(event, "freshness"),
            });
        }
        if let Some(tool) = event
            .tool
            .clone()
            .or_else(|| metadata_string(event, "tool"))
        {
            included_tools.push(ContextPackTool {
                name: tool.clone(),
                capability: metadata_string(event, "capability"),
                purpose: Some(event.redacted_summary.clone()),
                deterministic: matches!(event.kind.as_str(), "repeated_query" | "missing_context"),
            });
        }
        if let Some(capability) = metadata_string(event, "capability") {
            capabilities.insert(capability);
        }
        if let Some(secret_ref) = metadata_string(event, "secret_ref") {
            secrets
                .entry(secret_ref.clone())
                .or_insert(ContextPackSecretRef {
                    name: secret_ref,
                    capability: metadata_string(event, "capability"),
                    required: true,
                });
        }
        let slot =
            metadata_string(event, "output_slot").unwrap_or_else(|| "primary_context".to_string());
        output_slots
            .entry(slot.clone())
            .or_insert(ContextPackOutputSlot {
                name: slot,
                description: Some("Context gathered before the agent starts work".to_string()),
                artifact_kind: Some("context".to_string()),
            });
    }

    if included_queries.is_empty()
        && matches!(first.kind.as_str(), "repeated_query" | "missing_context")
    {
        included_queries.push(ContextPackQuery {
            id: "query_1".to_string(),
            label: first.tool.clone().or_else(|| first.provider.clone()),
            provider: first.provider.clone().or_else(|| first.tool.clone()),
            query: first.redacted_summary.clone(),
            filters: BTreeMap::new(),
            output_slot: Some("primary_context".to_string()),
        });
    }

    ContextPackManifest {
        version: CONTEXT_PACK_MANIFEST_VERSION,
        id: slugify(title),
        name: title.to_string(),
        description: Some(format!(
            "Candidate generated from repeated {} friction; review before promotion.",
            first.kind
        )),
        owner: options
            .owner
            .clone()
            .or_else(|| first.actor.clone())
            .unwrap_or_else(|| "team".to_string()),
        triggers: vec![ContextPackTrigger {
            kind: first.kind.clone(),
            source: first.source.clone(),
            match_hint: first.recurrence_hints.first().cloned(),
        }],
        inputs: vec![ContextPackInput {
            name: "incident_or_task".to_string(),
            description: Some("The current incident, ticket, run, or task identifier.".to_string()),
            required: true,
            source: first.source.clone(),
        }],
        included_queries,
        included_docs,
        included_tools,
        refresh_policy: ContextPackRefreshPolicy::default(),
        secrets: secrets.into_values().collect(),
        capabilities: capabilities.into_iter().collect(),
        output_slots: output_slots.into_values().collect(),
        fallback_instructions: Some(
            "If deterministic context is insufficient, ask a scoped clarifying question and record a new friction event.".to_string(),
        ),
        review: Some(ContextPackReviewPolicy {
            owner: options.owner.clone(),
            approval_required: true,
            privacy_notes: vec!["Confirm queries and docs do not expose raw customer secrets.".to_string()],
        }),
        metadata: BTreeMap::from([(
            "recommended_artifact".to_string(),
            serde_json::json!(recommended_artifact),
        )]),
    }
}

fn expectation_has_match(
    expectation: &ContextPackSuggestionExpectation,
    suggestions: &[ContextPackSuggestion],
) -> bool {
    if expectation.min_suggestions.is_some()
        && expectation.recommended_artifact.is_none()
        && expectation.title_contains.is_none()
        && expectation.manifest_name_contains.is_none()
        && expectation.required_capability.is_none()
        && expectation.required_output_slot.is_none()
    {
        return true;
    }
    suggestions.iter().any(|suggestion| {
        expectation
            .recommended_artifact
            .as_ref()
            .is_none_or(|expected| suggestion.recommended_artifact == *expected)
            && expectation.title_contains.as_ref().is_none_or(|needle| {
                suggestion
                    .title
                    .to_ascii_lowercase()
                    .contains(&needle.to_ascii_lowercase())
            })
            && expectation
                .manifest_name_contains
                .as_ref()
                .is_none_or(|needle| {
                    suggestion
                        .candidate_manifest
                        .name
                        .to_ascii_lowercase()
                        .contains(&needle.to_ascii_lowercase())
                })
            && expectation
                .required_capability
                .as_ref()
                .is_none_or(|capability| {
                    suggestion
                        .candidate_manifest
                        .capabilities
                        .iter()
                        .any(|candidate| candidate == capability)
                        || suggestion
                            .candidate_manifest
                            .included_tools
                            .iter()
                            .any(|tool| tool.capability.as_ref() == Some(capability))
                })
            && expectation
                .required_output_slot
                .as_ref()
                .is_none_or(|slot| {
                    suggestion
                        .candidate_manifest
                        .output_slots
                        .iter()
                        .any(|candidate| &candidate.name == slot)
                })
    })
}

fn friction_group_key(event: &FrictionEvent) -> String {
    let hint = event
        .recurrence_hints
        .first()
        .cloned()
        .unwrap_or_else(|| normalize_words(&event.redacted_summary));
    format!(
        "{}|{}|{}|{}|{}",
        event.kind,
        event.source.as_deref().unwrap_or(""),
        event.tool.as_deref().unwrap_or(""),
        event.provider.as_deref().unwrap_or(""),
        hint
    )
}

fn suggestion_title(event: &FrictionEvent) -> String {
    let source = event.source.as_deref().unwrap_or("team");
    let topic = event.recurrence_hints.first().cloned().unwrap_or_else(|| {
        normalize_words(&event.redacted_summary)
            .split_whitespace()
            .take(6)
            .collect::<Vec<_>>()
            .join(" ")
    });
    format!("{source} {topic} context pack")
}

fn recommended_artifact_for_kind(kind: &str) -> &'static str {
    match kind {
        "approval_stall" | "manual_handoff" => "workflow",
        "tool_gap" | "failed_assumption" | "expensive_model_used_for_deterministic_step" => "both",
        _ => "context_pack",
    }
}

fn confidence_for_occurrences(occurrences: usize) -> f64 {
    match occurrences {
        0 | 1 => 0.0,
        2 => 0.62,
        3 => 0.74,
        4 => 0.82,
        _ => 0.9,
    }
}

fn metadata_string(event: &FrictionEvent, key: &str) -> Option<String> {
    event
        .metadata
        .get(key)
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
}

fn metadata_string_map(event: &FrictionEvent, key: &str) -> BTreeMap<String, String> {
    event
        .metadata
        .get(key)
        .and_then(|value| value.as_object())
        .map(|map| {
            map.iter()
                .filter_map(|(key, value)| {
                    value.as_str().map(|value| (key.clone(), value.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn redact_json_value(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(text) => *text = redact_text(text),
        serde_json::Value::Array(items) => {
            for item in items {
                redact_json_value(item);
            }
        }
        serde_json::Value::Object(map) => {
            for (key, value) in map.iter_mut() {
                if is_sensitive_key(key) {
                    *value = serde_json::Value::String("[redacted]".to_string());
                } else {
                    redact_json_value(value);
                }
            }
        }
        _ => {}
    }
}

fn redact_text(text: &str) -> String {
    text.split_whitespace()
        .map(|word| {
            let lower = word.to_ascii_lowercase();
            if looks_like_secret_value(word)
                || lower.contains("token=")
                || lower.contains("password=")
                || lower.contains("api_key=")
                || lower.contains("apikey=")
            {
                "[redacted]".to_string()
            } else {
                word.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_sensitive_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    lower.contains("secret")
        || lower.contains("token")
        || lower.contains("password")
        || lower.contains("api_key")
        || lower.contains("apikey")
        || lower == "authorization"
}

fn looks_like_secret_value(value: &str) -> bool {
    let trimmed = value.trim();
    trimmed.starts_with("sk-")
        || trimmed.starts_with("ghp_")
        || trimmed.starts_with("xoxb-")
        || trimmed.starts_with("AKIA")
        || trimmed.len() > 48
            && trimmed
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

fn normalize_words(text: &str) -> String {
    text.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch.is_whitespace() {
                ch.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn slugify(text: &str) -> String {
    let slug = normalize_words(text)
        .split_whitespace()
        .take(8)
        .collect::<Vec<_>>()
        .join("_");
    if slug.is_empty() {
        new_id("context_pack")
    } else {
        slug
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn friction_event_normalizes_and_redacts_sensitive_metadata() {
        let event = normalize_friction_event_json(json!({
            "kind": "repeated_query",
            "source": "incident-triage",
            "redacted_summary": "Run Splunk query token=abc123",
            "metadata": {
                "query": "index=prod error",
                "api_key": "sk-live-secret"
            }
        }))
        .unwrap();

        assert_eq!(event.schema_version, FRICTION_SCHEMA_VERSION);
        assert!(event.id.starts_with("friction_"));
        assert!(event.redacted_summary.contains("[redacted]"));
        assert_eq!(event.metadata["api_key"], json!("[redacted]"));
    }

    #[test]
    fn context_pack_manifest_rejects_raw_secret_values() {
        let err = normalize_context_pack_manifest_json(json!({
            "name": "Incident pack",
            "owner": "sre",
            "secrets": [{"name": "sk-live-secret"}]
        }))
        .unwrap_err();

        assert!(err.to_string().contains("raw secret"));
    }

    #[test]
    fn repeated_incident_events_produce_context_pack_suggestion() {
        let events = vec![
            normalize_friction_event_json(json!({
                "kind": "repeated_query",
                "source": "incident-triage",
                "actor": "sre",
                "tool": "splunk",
                "provider": "splunk",
                "redacted_summary": "Every checkout incident needs the checkout error query",
                "estimated_time_ms": 300000,
                "estimated_cost_usd": 0.12,
                "recurrence_hints": ["checkout incident queries"],
                "metadata": {
                    "query": "index=checkout service=api error",
                    "capability": "splunk.search",
                    "secret_ref": "SPLUNK_READ_TOKEN",
                    "output_slot": "splunk_errors"
                }
            }))
            .unwrap(),
            normalize_friction_event_json(json!({
                "kind": "repeated_query",
                "source": "incident-triage",
                "actor": "sre",
                "tool": "splunk",
                "provider": "splunk",
                "redacted_summary": "Need the same checkout error search again",
                "estimated_time_ms": 240000,
                "estimated_cost_usd": 0.10,
                "recurrence_hints": ["checkout incident queries"],
                "metadata": {
                    "query": "index=checkout service=api error",
                    "capability": "splunk.search",
                    "secret_ref": "SPLUNK_READ_TOKEN",
                    "output_slot": "splunk_errors"
                }
            }))
            .unwrap(),
        ];

        let suggestions = generate_context_pack_suggestions(
            &events,
            &ContextPackSuggestionOptions {
                min_occurrences: 2,
                owner: Some("sre".to_string()),
            },
        );

        assert_eq!(suggestions.len(), 1);
        let suggestion = &suggestions[0];
        assert_eq!(suggestion.recommended_artifact, "context_pack");
        assert_eq!(suggestion.estimated_savings.occurrences, 2);
        assert_eq!(suggestion.estimated_savings.estimated_time_saved_ms, 240000);
        assert_eq!(
            suggestion.candidate_manifest.capabilities,
            vec!["splunk.search"]
        );
        assert_eq!(
            suggestion.candidate_manifest.output_slots[0].name,
            "splunk_errors"
        );
    }
}
