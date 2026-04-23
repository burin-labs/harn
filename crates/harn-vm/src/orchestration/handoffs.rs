use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::{current_mutation_session, new_id, now_rfc3339, ArtifactRecord, RunRecord};

const HANDOFF_TYPE: &str = "handoff_artifact";
const HANDOFF_ARTIFACT_KIND: &str = "handoff";
const RUN_RECEIPT_LINK_KIND: &str = "run_receipt";

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct HandoffTargetRecord {
    pub kind: String,
    pub id: Option<String>,
    pub label: Option<String>,
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
        self
    }

    pub fn display_name(&self) -> String {
        self.label
            .clone()
            .or_else(|| self.id.clone())
            .unwrap_or_else(|| "unknown".to_string())
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
    pub budget_remaining: Option<HandoffBudgetRemainingRecord>,
    pub deadline_checkback: Option<HandoffDeadlineCheckbackRecord>,
    pub confidence: Option<f64>,
    pub receipt_links: Vec<HandoffReceiptLinkRecord>,
    pub created_at: String,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

impl HandoffArtifact {
    pub fn normalize(mut self) -> Self {
        if self.type_name.is_empty() {
            self.type_name = HANDOFF_TYPE.to_string();
        }
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
        self.confidence = self.confidence.map(|value| value.clamp(0.0, 1.0));
        self
    }
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
        _ => "persona".to_string(),
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
    if left.budget_remaining.is_none() {
        left.budget_remaining = right.budget_remaining;
    }
    if left.deadline_checkback.is_none() {
        left.deadline_checkback = right.deadline_checkback;
    }
    if left.confidence.is_none() {
        left.confidence = right.confidence;
    }
    left.receipt_links = merge_receipt_links(left.receipt_links, right.receipt_links);
    for (key, value) in right.metadata {
        left.metadata.entry(key).or_insert(value);
    }
    left
}

pub fn handoff_context_text(handoff: &HandoffArtifact) -> String {
    let mut lines = vec![
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
