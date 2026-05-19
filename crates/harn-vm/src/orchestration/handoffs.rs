use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::{
    current_mutation_session, effect_record_summary, effect_subset_violations, new_id, now_rfc3339,
    ArtifactRecord, CapabilityPolicy, EffectRecord, RunRecord,
};

const HANDOFF_TYPE: &str = "handoff_artifact";
const HANDOFF_ARTIFACT_KIND: &str = "handoff";
const RUN_RECEIPT_LINK_KIND: &str = "run_receipt";
const DEFAULT_HANDOFF_KIND: &str = "handoff";

thread_local! {
    static HANDOFF_ROUTES: RefCell<Vec<HandoffRouteConfig>> = const { RefCell::new(Vec::new()) };
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct HandoffTargetRecord {
    pub kind: String,
    pub id: Option<String>,
    pub label: Option<String>,
    pub uri: Option<String>,
}

impl HandoffTargetRecord {
    pub fn normalize(mut self) -> Self {
        self.kind = normalize_target_kind(&self.kind);
        if self
            .id
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            self.id = None;
        }
        if self
            .label
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            self.label = None;
        }
        if self
            .uri
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            self.uri = None;
        }
        self
    }

    pub fn display_name(&self) -> String {
        self.label
            .clone()
            .or_else(|| self.id.clone())
            .unwrap_or_else(|| "unknown".to_string())
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct HandoffRouteTargetConfig {
    pub id: Option<String>,
    pub target: String,
    pub when: Option<String>,
    pub transport: Option<String>,
    pub allow_cleartext: Option<bool>,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

impl HandoffRouteTargetConfig {
    pub fn normalize(mut self) -> Self {
        if self
            .id
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            self.id = None;
        }
        self.target = self.target.trim().to_string();
        self.when = self
            .when
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        self.transport = self
            .transport
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        self
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct HandoffRouteConfig {
    pub id: Option<String>,
    pub kind: String,
    pub from: String,
    #[serde(alias = "routes")]
    pub route: Vec<HandoffRouteTargetConfig>,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

impl HandoffRouteConfig {
    pub fn normalize(mut self) -> Self {
        if self
            .id
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            self.id = None;
        }
        self.kind = normalize_handoff_kind(&self.kind);
        self.from = self.from.trim().to_string();
        self.route = self
            .route
            .into_iter()
            .map(HandoffRouteTargetConfig::normalize)
            .collect();
        self
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct HandoffRouteDecisionRecord {
    pub route_id: Option<String>,
    pub route_index: Option<u64>,
    pub target_index: Option<u64>,
    pub handoff_id: Option<String>,
    pub handoff_kind: String,
    pub source_persona: String,
    pub target: String,
    pub target_persona_or_human: HandoffTargetRecord,
    pub matched_when: String,
    pub selected_at: String,
    pub dispatch_kind: String,
    pub dispatch_status: Option<String>,
    pub dispatch_receipt: Option<serde_json::Value>,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

impl HandoffRouteDecisionRecord {
    pub fn normalize(mut self) -> Self {
        self.handoff_id = self
            .handoff_id
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        self.handoff_kind = normalize_handoff_kind(&self.handoff_kind);
        self.source_persona = self.source_persona.trim().to_string();
        self.target = self.target.trim().to_string();
        self.target_persona_or_human = self.target_persona_or_human.normalize();
        self.matched_when = self.matched_when.trim().to_string();
        if self.matched_when.is_empty() {
            self.matched_when = "always".to_string();
        }
        self.selected_at = self.selected_at.trim().to_string();
        if self.selected_at.is_empty() {
            self.selected_at = now_rfc3339();
        }
        self.dispatch_kind = normalize_target_kind(&self.dispatch_kind);
        self.dispatch_status = self
            .dispatch_status
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        self
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct HandoffEvidenceRefRecord {
    pub artifact_id: Option<String>,
    pub kind: Option<String>,
    pub label: Option<String>,
    pub path: Option<String>,
    pub uri: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct HandoffBudgetRemainingRecord {
    pub tokens: Option<i64>,
    pub tool_calls: Option<i64>,
    pub dollars: Option<f64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct HandoffDeadlineCheckbackRecord {
    pub deadline: Option<String>,
    pub checkback_at: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct HandoffReceiptLinkRecord {
    pub kind: String,
    pub label: Option<String>,
    pub run_id: Option<String>,
    pub artifact_id: Option<String>,
    pub path: Option<String>,
    pub href: Option<String>,
}

impl HandoffReceiptLinkRecord {
    pub fn normalize(mut self) -> Self {
        if self.kind.trim().is_empty() {
            self.kind = RUN_RECEIPT_LINK_KIND.to_string();
        }
        if self
            .label
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            self.label = None;
        }
        if self
            .run_id
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            self.run_id = None;
        }
        if self
            .artifact_id
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            self.artifact_id = None;
        }
        if self
            .path
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            self.path = None;
        }
        if self
            .href
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            self.href = None;
        }
        self
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct HandoffArtifact {
    #[serde(rename = "_type")]
    pub type_name: String,
    pub kind: String,
    pub id: String,
    pub parent_run_id: Option<String>,
    pub source_persona: String,
    pub target_persona_or_human: HandoffTargetRecord,
    pub task: String,
    pub reason: String,
    pub evidence_refs: Vec<HandoffEvidenceRefRecord>,
    pub files_or_entities_touched: Vec<String>,
    pub open_questions: Vec<String>,
    pub blocked_on: Vec<String>,
    pub requested_capabilities: Vec<String>,
    pub allowed_side_effects: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_override: Option<CapabilityPolicy>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reminder_propagation: Vec<crate::llm::helpers::SystemReminder>,
    /// Typed effect set computed at child-spawn time from the spawn
    /// config's capability declarations + transitive `harn graph --json`
    /// analysis. Empty when the handoff predates effect tracking or the
    /// producer has no analyzable entrypoint. Enforcement of the
    /// parent-⊆-child relation lives in E5.4 (`HARN-CAP-301`); the
    /// receipt-chain inclusion proof lives in E5.5
    /// (`opentrustgraph/v0.1`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<EffectRecord>,
    pub budget_remaining: Option<HandoffBudgetRemainingRecord>,
    pub deadline_checkback: Option<HandoffDeadlineCheckbackRecord>,
    pub confidence: Option<f64>,
    pub receipt_links: Vec<HandoffReceiptLinkRecord>,
    pub route_decision: Option<HandoffRouteDecisionRecord>,
    pub created_at: String,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

impl HandoffArtifact {
    pub fn normalize(mut self) -> Self {
        if self.type_name.is_empty() {
            self.type_name = HANDOFF_TYPE.to_string();
        }
        self.kind = normalize_handoff_kind(&self.kind);
        if self.id.is_empty() {
            self.id = new_id("handoff");
        }
        if self.created_at.is_empty() {
            self.created_at = now_rfc3339();
        }
        if self.parent_run_id.is_none() {
            self.parent_run_id = current_mutation_session().and_then(|session| session.run_id);
        }
        self.source_persona = self.source_persona.trim().to_string();
        self.task = self.task.trim().to_string();
        self.reason = self.reason.trim().to_string();
        self.target_persona_or_human = self.target_persona_or_human.normalize();
        self.files_or_entities_touched = normalize_string_list(self.files_or_entities_touched);
        self.open_questions = normalize_string_list(self.open_questions);
        self.blocked_on = normalize_string_list(self.blocked_on);
        self.requested_capabilities = normalize_string_list(self.requested_capabilities);
        self.allowed_side_effects = normalize_string_list(self.allowed_side_effects);
        self.receipt_links = self
            .receipt_links
            .into_iter()
            .map(HandoffReceiptLinkRecord::normalize)
            .collect();
        self.route_decision = self
            .route_decision
            .map(HandoffRouteDecisionRecord::normalize);
        self.confidence = self.confidence.map(|value| value.clamp(0.0, 1.0));
        self
    }
}

pub fn install_handoff_routes(routes: Vec<HandoffRouteConfig>) {
    HANDOFF_ROUTES.with(|installed| {
        *installed.borrow_mut() = routes
            .into_iter()
            .map(HandoffRouteConfig::normalize)
            .collect();
    });
}

pub fn snapshot_handoff_routes() -> Vec<HandoffRouteConfig> {
    HANDOFF_ROUTES.with(|installed| installed.borrow().clone())
}

fn normalize_string_list(values: Vec<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    values
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty() && seen.insert(value.clone()))
        .collect()
}

fn normalize_target_kind(kind: &str) -> String {
    match kind.trim() {
        "human" => "human".to_string(),
        "persona" => "persona".to_string(),
        "a2a" | "external_a2a" => "a2a".to_string(),
        "worker" | "queue" => "worker".to_string(),
        _ => "persona".to_string(),
    }
}

fn normalize_handoff_kind(kind: &str) -> String {
    let kind = kind.trim();
    if kind.is_empty() {
        DEFAULT_HANDOFF_KIND.to_string()
    } else {
        kind.to_string()
    }
}

pub fn normalize_handoff_artifact_json(
    value: serde_json::Value,
) -> Result<HandoffArtifact, String> {
    let handoff: HandoffArtifact =
        serde_json::from_value(value).map_err(|error| format!("handoff parse error: {error}"))?;
    let handoff = handoff.normalize();
    if handoff.source_persona.is_empty() {
        return Err("handoff source_persona is required".to_string());
    }
    if handoff.target_persona_or_human.display_name() == "unknown" {
        return Err("handoff target_persona_or_human is required".to_string());
    }
    if handoff.task.is_empty() {
        return Err("handoff task is required".to_string());
    }
    if handoff.reason.is_empty() {
        return Err("handoff reason is required".to_string());
    }
    if let Some(decision) = handoff.route_decision.as_ref() {
        if decision.target_persona_or_human.display_name() == "unknown" {
            return Err("handoff route_decision target is required".to_string());
        }
    }
    Ok(handoff)
}

pub fn handoff_from_json_value(value: &serde_json::Value) -> Option<HandoffArtifact> {
    let object = value.as_object()?;
    if object.get("_type").and_then(|value| value.as_str()) == Some(HANDOFF_TYPE)
        || (object.contains_key("source_persona")
            && object.contains_key("target_persona_or_human")
            && object.contains_key("task"))
    {
        return normalize_handoff_artifact_json(value.clone()).ok();
    }
    if object.get("_type").and_then(|value| value.as_str()) == Some("artifact")
        || object.get("kind").and_then(|value| value.as_str()) == Some(HANDOFF_ARTIFACT_KIND)
    {
        return object
            .get("data")
            .and_then(handoff_from_json_value)
            .or_else(|| normalize_handoff_artifact_json(value.clone()).ok());
    }
    if object.get("_type").and_then(|value| value.as_str()) == Some("agent_state_handoff") {
        return object
            .get("handoff")
            .and_then(handoff_from_json_value)
            .or_else(|| object.get("summary").and_then(handoff_from_json_value));
    }
    None
}

pub fn extract_handoff_from_artifact(artifact: &ArtifactRecord) -> Option<HandoffArtifact> {
    if artifact.kind != HANDOFF_ARTIFACT_KIND {
        return None;
    }
    artifact.data.as_ref().and_then(handoff_from_json_value)
}

pub fn extract_handoffs_from_json_value(value: &serde_json::Value) -> Vec<HandoffArtifact> {
    fn collect(value: &serde_json::Value, out: &mut Vec<HandoffArtifact>) {
        if let Some(handoff) = handoff_from_json_value(value) {
            out.push(handoff);
        }
        let Some(object) = value.as_object() else {
            return;
        };
        for key in ["handoffs", "artifacts"] {
            if let Some(items) = object.get(key).and_then(|value| value.as_array()) {
                for item in items {
                    collect(item, out);
                }
            }
        }
        for key in ["run", "result"] {
            if let Some(nested) = object.get(key) {
                collect(nested, out);
            }
        }
    }

    let mut handoffs = Vec::new();
    collect(value, &mut handoffs);
    dedup_handoffs(handoffs)
}

fn dedup_handoffs(handoffs: Vec<HandoffArtifact>) -> Vec<HandoffArtifact> {
    let mut by_id = BTreeMap::new();
    for handoff in handoffs {
        by_id
            .entry(handoff.id.clone())
            .and_modify(|existing: &mut HandoffArtifact| {
                *existing = merge_handoffs(existing.clone(), handoff.clone())
            })
            .or_insert(handoff);
    }
    by_id.into_values().collect()
}

fn merge_receipt_links(
    left: Vec<HandoffReceiptLinkRecord>,
    right: Vec<HandoffReceiptLinkRecord>,
) -> Vec<HandoffReceiptLinkRecord> {
    let mut seen = BTreeSet::new();
    left.into_iter()
        .chain(right)
        .map(HandoffReceiptLinkRecord::normalize)
        .filter(|link| {
            seen.insert((
                link.kind.clone(),
                link.run_id.clone(),
                link.artifact_id.clone(),
                link.path.clone(),
                link.href.clone(),
            ))
        })
        .collect()
}

fn merge_handoffs(mut left: HandoffArtifact, right: HandoffArtifact) -> HandoffArtifact {
    if left.parent_run_id.is_none() {
        left.parent_run_id = right.parent_run_id;
    }
    if left.source_persona.is_empty() {
        left.source_persona = right.source_persona;
    }
    if left.target_persona_or_human.display_name() == "unknown" {
        left.target_persona_or_human = right.target_persona_or_human;
    }
    if left.task.is_empty() {
        left.task = right.task;
    }
    if left.reason.is_empty() {
        left.reason = right.reason;
    }
    if left.evidence_refs.is_empty() {
        left.evidence_refs = right.evidence_refs;
    }
    if left.files_or_entities_touched.is_empty() {
        left.files_or_entities_touched = right.files_or_entities_touched;
    }
    if left.open_questions.is_empty() {
        left.open_questions = right.open_questions;
    }
    if left.blocked_on.is_empty() {
        left.blocked_on = right.blocked_on;
    }
    if left.requested_capabilities.is_empty() {
        left.requested_capabilities = right.requested_capabilities;
    }
    if left.allowed_side_effects.is_empty() {
        left.allowed_side_effects = right.allowed_side_effects;
    }
    if left.policy_override.is_none() {
        left.policy_override = right.policy_override;
    }
    if left.reminder_propagation.is_empty() {
        left.reminder_propagation = right.reminder_propagation;
    }
    if left.effects.is_empty() {
        left.effects = right.effects;
    }
    if left.budget_remaining.is_none() {
        left.budget_remaining = right.budget_remaining;
    }
    if left.deadline_checkback.is_none() {
        left.deadline_checkback = right.deadline_checkback;
    }
    if left.confidence.is_none() {
        left.confidence = right.confidence;
    }
    if left.route_decision.is_none() {
        left.route_decision = right.route_decision;
    }
    left.receipt_links = merge_receipt_links(left.receipt_links, right.receipt_links);
    for (key, value) in right.metadata {
        left.metadata.entry(key).or_insert(value);
    }
    left
}

pub fn handoff_context_text(handoff: &HandoffArtifact) -> String {
    let mut lines = vec![
        format!("<kind>{}</kind>", handoff.kind),
        format!(
            "<source_persona>{}</source_persona>",
            handoff.source_persona
        ),
        format!(
            "<target kind=\"{}\">{}</target>",
            handoff.target_persona_or_human.kind,
            handoff.target_persona_or_human.display_name()
        ),
        format!("<task>{}</task>", handoff.task),
        format!("<reason>{}</reason>", handoff.reason),
    ];
    append_list_section(
        &mut lines,
        "files_or_entities_touched",
        &handoff.files_or_entities_touched,
    );
    append_list_section(&mut lines, "open_questions", &handoff.open_questions);
    append_list_section(&mut lines, "blocked_on", &handoff.blocked_on);
    append_list_section(
        &mut lines,
        "requested_capabilities",
        &handoff.requested_capabilities,
    );
    append_list_section(
        &mut lines,
        "allowed_side_effects",
        &handoff.allowed_side_effects,
    );
    if !handoff.evidence_refs.is_empty() {
        lines.push("<evidence_refs>".to_string());
        for evidence in &handoff.evidence_refs {
            let mut parts = Vec::new();
            if let Some(label) = evidence.label.as_ref() {
                parts.push(label.clone());
            }
            if let Some(artifact_id) = evidence.artifact_id.as_ref() {
                parts.push(format!("artifact_id={artifact_id}"));
            }
            if let Some(path) = evidence.path.as_ref() {
                parts.push(format!("path={path}"));
            }
            if let Some(uri) = evidence.uri.as_ref() {
                parts.push(format!("uri={uri}"));
            }
            if let Some(kind) = evidence.kind.as_ref() {
                parts.push(format!("kind={kind}"));
            }
            lines.push(format!("- {}", parts.join(" | ")));
        }
        lines.push("</evidence_refs>".to_string());
    }
    if let Some(budget) = handoff.budget_remaining.as_ref() {
        lines.push(format!(
            "<budget_remaining tokens=\"{}\" tool_calls=\"{}\" dollars=\"{}\" />",
            budget
                .tokens
                .map(|value| value.to_string())
                .unwrap_or_default(),
            budget
                .tool_calls
                .map(|value| value.to_string())
                .unwrap_or_default(),
            budget
                .dollars
                .map(|value| format!("{value:.4}"))
                .unwrap_or_default(),
        ));
    }
    if let Some(deadline) = handoff.deadline_checkback.as_ref() {
        lines.push(format!(
            "<deadline_checkback deadline=\"{}\" checkback_at=\"{}\" />",
            deadline.deadline.clone().unwrap_or_default(),
            deadline.checkback_at.clone().unwrap_or_default(),
        ));
    }
    if let Some(confidence) = handoff.confidence {
        lines.push(format!("<confidence>{confidence:.2}</confidence>"));
    }
    if let Some(decision) = handoff.route_decision.as_ref() {
        lines.push(format!(
            "<route_decision target=\"{}\" when=\"{}\" dispatch=\"{}\" selected_at=\"{}\" />",
            decision.target, decision.matched_when, decision.dispatch_kind, decision.selected_at
        ));
    }
    format!("<handoff>\n{}\n</handoff>", lines.join("\n"))
}

fn append_list_section(lines: &mut Vec<String>, label: &str, items: &[String]) {
    if items.is_empty() {
        return;
    }
    lines.push(format!("<{label}>"));
    for item in items {
        lines.push(format!("- {item}"));
    }
    lines.push(format!("</{label}>"));
}

fn handoff_target_label(handoff: &HandoffArtifact) -> String {
    handoff.target_persona_or_human.display_name()
}

fn handoff_metadata(handoff: &HandoffArtifact) -> BTreeMap<String, serde_json::Value> {
    BTreeMap::from([
        ("handoff_id".to_string(), serde_json::json!(handoff.id)),
        ("handoff_kind".to_string(), serde_json::json!(handoff.kind)),
        (
            "target_kind".to_string(),
            serde_json::json!(handoff.target_persona_or_human.kind),
        ),
        (
            "target_label".to_string(),
            serde_json::json!(handoff_target_label(handoff)),
        ),
    ])
}

pub fn handoff_artifact_record(
    handoff: &HandoffArtifact,
    existing: Option<&ArtifactRecord>,
) -> ArtifactRecord {
    let mut metadata = existing
        .map(|artifact| artifact.metadata.clone())
        .unwrap_or_default();
    metadata.extend(handoff_metadata(handoff));
    ArtifactRecord {
        type_name: "artifact".to_string(),
        id: existing
            .map(|artifact| artifact.id.clone())
            .unwrap_or_else(|| format!("artifact_{}", handoff.id)),
        kind: HANDOFF_ARTIFACT_KIND.to_string(),
        title: existing
            .and_then(|artifact| artifact.title.clone())
            .or_else(|| Some(format!("Handoff to {}", handoff_target_label(handoff)))),
        text: Some(handoff_context_text(handoff)),
        data: Some(serde_json::to_value(handoff).unwrap_or(serde_json::Value::Null)),
        source: existing
            .and_then(|artifact| artifact.source.clone())
            .or_else(|| Some(handoff.source_persona.clone())),
        created_at: existing
            .map(|artifact| artifact.created_at.clone())
            .unwrap_or_else(now_rfc3339),
        freshness: existing
            .and_then(|artifact| artifact.freshness.clone())
            .or_else(|| Some("fresh".to_string())),
        priority: existing.and_then(|artifact| artifact.priority).or(Some(85)),
        lineage: existing
            .map(|artifact| artifact.lineage.clone())
            .unwrap_or_default(),
        relevance: handoff.confidence.or(Some(1.0)),
        estimated_tokens: None,
        stage: existing.and_then(|artifact| artifact.stage.clone()),
        metadata,
    }
    .normalize()
}

fn receipt_link_for_run(run: &RunRecord) -> HandoffReceiptLinkRecord {
    HandoffReceiptLinkRecord {
        kind: RUN_RECEIPT_LINK_KIND.to_string(),
        label: run
            .workflow_name
            .clone()
            .or_else(|| Some(run.workflow_id.clone())),
        run_id: Some(run.id.clone()),
        artifact_id: None,
        path: run.persisted_path.clone(),
        href: None,
    }
    .normalize()
}

fn sync_handoff_receipt_links(handoff: &mut HandoffArtifact, run: &RunRecord) {
    if handoff.parent_run_id.is_none() {
        handoff.parent_run_id = Some(run.id.clone());
    }
    handoff.receipt_links = merge_receipt_links(
        std::mem::take(&mut handoff.receipt_links),
        vec![receipt_link_for_run(run)],
    );
}

fn artifact_handoff_id(artifact: &ArtifactRecord) -> Option<String> {
    if artifact.kind != HANDOFF_ARTIFACT_KIND {
        return None;
    }
    artifact
        .metadata
        .get("handoff_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            artifact
                .data
                .as_ref()
                .and_then(|value| value.get("id"))
                .and_then(|value| value.as_str())
                .map(str::to_string)
        })
}

pub fn sync_run_handoffs(run: &mut RunRecord) {
    let mut by_id = BTreeMap::new();
    for handoff in std::mem::take(&mut run.handoffs) {
        by_id.insert(handoff.id.clone(), handoff.normalize());
    }
    for artifact in &run.artifacts {
        if let Some(handoff) = extract_handoff_from_artifact(artifact) {
            by_id
                .entry(handoff.id.clone())
                .and_modify(|existing| {
                    *existing = merge_handoffs(existing.clone(), handoff.clone())
                })
                .or_insert(handoff);
        }
    }

    let mut artifact_index_by_handoff_id = BTreeMap::new();
    for (index, artifact) in run.artifacts.iter().enumerate() {
        if let Some(handoff_id) = artifact_handoff_id(artifact) {
            artifact_index_by_handoff_id.insert(handoff_id, index);
        }
    }

    let mut handoffs = by_id.into_values().collect::<Vec<_>>();
    handoffs.sort_by(|left, right| left.created_at.cmp(&right.created_at));
    for handoff in &mut handoffs {
        sync_handoff_receipt_links(handoff, run);
        if let Some(index) = artifact_index_by_handoff_id.get(&handoff.id).copied() {
            let existing = run.artifacts[index].clone();
            run.artifacts[index] = handoff_artifact_record(handoff, Some(&existing));
        } else {
            run.artifacts.push(handoff_artifact_record(handoff, None));
        }
    }
    run.handoffs = handoffs;
}

/// Compute the effect set for a spawn-time handoff and attach it to the
/// envelope. Mirrors what `agent_spawn` / `sub_agent_run` do when a child
/// entrypoint module is statically known: the effect set is derived from
/// the child's source via the same capability analysis backing
/// `harn graph --json` and clamped to the spawn-config ceiling.
///
/// Pre-existing `effects` are preserved when the producer has already
/// populated them; otherwise the computed set is installed. Empty source
/// is a no-op so callers can route through this helper unconditionally.
pub fn attach_spawn_handoff_effects(
    handoff: &mut HandoffArtifact,
    entrypoint_source: &str,
    ceiling: Option<&CapabilityPolicy>,
) {
    if !handoff.effects.is_empty() {
        return;
    }
    if entrypoint_source.trim().is_empty() {
        return;
    }
    handoff.effects = crate::orchestration::compute_handoff_effects(entrypoint_source, ceiling);
}

/// Typed deny payload returned when a child handoff exceeds the parent's
/// declared effect set. The dispatcher (E5.4) emits this as the body of
/// an `EffectInheritanceViolation` event and refuses the spawn. The same
/// `repair_id` is suggested by `harn check`'s static `HARN-CAP-301` path,
/// so a user can dispatch one `harn fix --apply` strategy regardless of
/// which path surfaced the failure.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct EffectInheritanceViolation {
    /// Stable name for the deny payload — matches the variant carried on
    /// the deny event so downstream consumers can route by string.
    #[serde(rename = "_type")]
    pub type_name: String,
    /// Handoff that requested the over-granted effects.
    pub handoff_id: String,
    /// Source persona that produced the child handoff.
    pub source_persona: String,
    /// Display label for the target child the handoff would have spawned.
    pub target_label: String,
    /// Effects the child requested that the parent does not cover.
    pub violations: Vec<EffectRecord>,
    /// Stable diagnostic code shared with the static analyzer.
    pub diagnostic_code: String,
    /// Stable repair id suggested for `harn fix --apply`.
    pub repair_id: String,
    /// Repair safety class (matches `RepairSafety::SurfaceChanging`).
    pub repair_safety: String,
    /// Human-readable summary used by transcripts and the friction log.
    pub message: String,
}

const EFFECT_INHERITANCE_VIOLATION_TYPE: &str = "effect_inheritance_violation";
const EFFECT_INHERITANCE_DIAGNOSTIC_CODE: &str = "HARN-CAP-301";
const EFFECT_INHERITANCE_REPAIR_ID: &str = "policy/narrow-child-effects";
const EFFECT_INHERITANCE_REPAIR_SAFETY: &str = "surface-changing";

impl EffectInheritanceViolation {
    /// Render the violation as a one-line dispatcher-deny message. Kept
    /// stable so log scrapers + the `harn check` CLI can pattern-match.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Build a violation payload from a (handoff, violations) pair.
    pub fn for_handoff(handoff: &HandoffArtifact, violations: Vec<EffectRecord>) -> Self {
        let summaries: Vec<String> = violations.iter().map(effect_record_summary).collect();
        let target_label = handoff.target_persona_or_human.display_name();
        let message = format!(
            "EffectInheritanceViolation: child handoff '{handoff_id}' to '{target}' \
             requests effects outside the parent's declared set: {effects} \
             [{code}, repair={repair} ({safety})]",
            handoff_id = handoff.id,
            target = target_label,
            effects = summaries.join(", "),
            code = EFFECT_INHERITANCE_DIAGNOSTIC_CODE,
            repair = EFFECT_INHERITANCE_REPAIR_ID,
            safety = EFFECT_INHERITANCE_REPAIR_SAFETY,
        );
        EffectInheritanceViolation {
            type_name: EFFECT_INHERITANCE_VIOLATION_TYPE.to_string(),
            handoff_id: handoff.id.clone(),
            source_persona: handoff.source_persona.clone(),
            target_label,
            violations,
            diagnostic_code: EFFECT_INHERITANCE_DIAGNOSTIC_CODE.to_string(),
            repair_id: EFFECT_INHERITANCE_REPAIR_ID.to_string(),
            repair_safety: EFFECT_INHERITANCE_REPAIR_SAFETY.to_string(),
            message,
        }
    }
}

/// Runtime guard that mirrors the static `HARN-CAP-301` check. When a
/// spawn-time handoff carries `effects` that are not covered by the
/// parent's declared `effects`, return a typed violation that the
/// dispatcher surfaces as an `EffectInheritanceViolation` deny event and
/// refuses to spawn the child. When `parent_effects` is `None` no
/// enforcement is performed — the parent has no statically derivable
/// effect surface so over-granting is impossible to prove at this layer
/// and the next stage of the pipeline (receipts in E5.5) takes over.
pub fn enforce_spawn_handoff_effects(
    handoff: &HandoffArtifact,
    parent_effects: Option<&[EffectRecord]>,
) -> Result<(), EffectInheritanceViolation> {
    let violations = effect_subset_violations(parent_effects, &handoff.effects);
    if violations.is_empty() {
        return Ok(());
    }
    Err(EffectInheritanceViolation::for_handoff(handoff, violations))
}

/// Convenience wrapper: emit a structured deny log alongside the typed
/// payload so transcripts and observability sinks pick it up without
/// every caller re-emitting the same `log_warn_meta` call.
pub fn report_effect_inheritance_violation(violation: &EffectInheritanceViolation) {
    let mut metadata = BTreeMap::new();
    metadata.insert(
        "handoff_id".to_string(),
        serde_json::Value::String(violation.handoff_id.clone()),
    );
    metadata.insert(
        "source_persona".to_string(),
        serde_json::Value::String(violation.source_persona.clone()),
    );
    metadata.insert(
        "target_label".to_string(),
        serde_json::Value::String(violation.target_label.clone()),
    );
    metadata.insert(
        "diagnostic_code".to_string(),
        serde_json::Value::String(violation.diagnostic_code.clone()),
    );
    metadata.insert(
        "repair_id".to_string(),
        serde_json::Value::String(violation.repair_id.clone()),
    );
    metadata.insert(
        "repair_safety".to_string(),
        serde_json::Value::String(violation.repair_safety.clone()),
    );
    metadata.insert(
        "violations".to_string(),
        serde_json::to_value(&violation.violations).unwrap_or(serde_json::Value::Null),
    );
    crate::events::log_warn_meta("policy.effect_inheritance", violation.message(), metadata);
}

#[cfg(test)]
mod spawn_effect_tests {
    use super::{enforce_spawn_handoff_effects, EffectInheritanceViolation, *};
    use crate::orchestration::{
        attach_spawn_handoff_effects, CapabilityPolicy, EffectKind, EffectRecord, EffectScope,
        HandoffTargetRecord,
    };

    fn spawn_handoff(source_persona: &str) -> HandoffArtifact {
        HandoffArtifact {
            source_persona: source_persona.to_string(),
            target_persona_or_human: HandoffTargetRecord {
                kind: "persona".to_string(),
                label: Some("research-worker".to_string()),
                ..Default::default()
            },
            task: "summarize the page".to_string(),
            reason: "needs network reach".to_string(),
            ..Default::default()
        }
        .normalize()
    }

    #[test]
    fn spawn_with_harness_net_child_attaches_net_effect() {
        let source = r#"fn main(harness: Harness) { harness.net.get("https://example.test/api") }"#;
        let mut handoff = spawn_handoff("planner");
        attach_spawn_handoff_effects(&mut handoff, source, None);
        assert!(
            handoff
                .effects
                .iter()
                .any(|effect| matches!(effect.kind, EffectKind::Net)),
            "expected Net effect on spawn handoff, got {:?}",
            handoff.effects
        );
    }

    #[test]
    fn spawn_ceiling_clamps_to_allowed_capabilities() {
        let source = r#"fn main(harness: Harness) {
            harness.net.get("https://example.test")
            harness.fs.read_file("/tmp/input")
        }"#;
        let mut ceiling = CapabilityPolicy::default();
        ceiling
            .capabilities
            .insert("workspace".to_string(), vec!["read_text".to_string()]);
        let mut handoff = spawn_handoff("planner");
        attach_spawn_handoff_effects(&mut handoff, source, Some(&ceiling));

        assert!(
            handoff
                .effects
                .iter()
                .all(|effect| !matches!(effect.kind, EffectKind::Net)),
            "ceiling should have dropped Net effect, got {:?}",
            handoff.effects
        );
        assert!(
            handoff
                .effects
                .iter()
                .any(|effect| matches!(effect.kind, EffectKind::Fs)),
            "ceiling should have kept Fs read, got {:?}",
            handoff.effects
        );
    }

    #[test]
    fn spawn_handoff_effects_round_trip_via_serde() {
        let mut handoff = spawn_handoff("planner");
        handoff.effects.push(
            EffectRecord::new(EffectKind::Net, EffectScope::Write)
                .with_resource("https://api.example/v1/research"),
        );
        handoff.effects.push(EffectRecord::new(
            EffectKind::Llm {
                provider: Some("anthropic".to_string()),
                model: Some("claude-3-7-sonnet".to_string()),
            },
            EffectScope::Write,
        ));

        let encoded = serde_json::to_string(&handoff).expect("encode");
        let decoded: HandoffArtifact = serde_json::from_str(&encoded).expect("decode");
        assert_eq!(decoded.effects, handoff.effects);
    }

    #[test]
    fn attach_is_no_op_when_handoff_already_has_effects() {
        let source = r#"fn main(harness: Harness) { harness.net.get("https://example.test") }"#;
        let mut handoff = spawn_handoff("planner");
        let preset = EffectRecord::new(
            EffectKind::Persona {
                id: "auditor".to_string(),
            },
            EffectScope::Observe,
        );
        handoff.effects.push(preset.clone());
        attach_spawn_handoff_effects(&mut handoff, source, None);
        assert_eq!(handoff.effects, vec![preset]);
    }

    #[test]
    fn attach_is_no_op_when_source_is_empty() {
        let mut handoff = spawn_handoff("planner");
        attach_spawn_handoff_effects(&mut handoff, "", None);
        assert!(handoff.effects.is_empty());
    }

    #[test]
    fn enforce_returns_ok_when_no_parent_ceiling() {
        let mut handoff = spawn_handoff("planner");
        handoff
            .effects
            .push(EffectRecord::new(EffectKind::Net, EffectScope::Write));
        assert!(enforce_spawn_handoff_effects(&handoff, None).is_ok());
    }

    #[test]
    fn enforce_returns_ok_when_child_is_subset() {
        let parent = vec![EffectRecord::new(EffectKind::Net, EffectScope::Write)];
        let mut handoff = spawn_handoff("planner");
        handoff.effects.push(
            EffectRecord::new(EffectKind::Net, EffectScope::Write)
                .with_resource("https://api.example/v1"),
        );
        assert!(enforce_spawn_handoff_effects(&handoff, Some(&parent)).is_ok());
    }

    #[test]
    fn enforce_returns_violation_when_child_over_grants() {
        let parent = vec![EffectRecord::new(EffectKind::Fs, EffectScope::Read)];
        let mut handoff = spawn_handoff("planner");
        handoff
            .effects
            .push(EffectRecord::new(EffectKind::Net, EffectScope::Write));
        let violation = enforce_spawn_handoff_effects(&handoff, Some(&parent))
            .expect_err("over-granted child should fail");
        assert_eq!(violation.diagnostic_code, "HARN-CAP-301");
        assert_eq!(violation.repair_id, "policy/narrow-child-effects");
        assert_eq!(violation.repair_safety, "surface-changing");
        assert_eq!(violation.violations.len(), 1);
        assert!(matches!(violation.violations[0].kind, EffectKind::Net));
        assert!(violation.message.contains("EffectInheritanceViolation"));
        assert!(violation.message.contains("HARN-CAP-301"));
    }

    #[test]
    fn enforce_violation_serde_round_trips() {
        let parent = vec![EffectRecord::new(EffectKind::Stdio, EffectScope::Observe)];
        let mut handoff = spawn_handoff("planner");
        handoff
            .effects
            .push(EffectRecord::new(EffectKind::Net, EffectScope::Write));
        let violation = enforce_spawn_handoff_effects(&handoff, Some(&parent))
            .expect_err("over-granted child should fail");
        let encoded = serde_json::to_string(&violation).expect("encode");
        let decoded: EffectInheritanceViolation = serde_json::from_str(&encoded).expect("decode");
        assert_eq!(decoded, violation);
        assert_eq!(decoded.type_name, "effect_inheritance_violation");
    }

    #[test]
    fn enforce_empty_child_effects_always_ok() {
        let parent = vec![EffectRecord::new(EffectKind::Fs, EffectScope::Read)];
        let handoff = spawn_handoff("planner");
        assert!(enforce_spawn_handoff_effects(&handoff, Some(&parent)).is_ok());
    }
}
