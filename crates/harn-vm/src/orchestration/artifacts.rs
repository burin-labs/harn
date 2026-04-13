//! Artifact types, normalization, selection, and context rendering.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::{microcompact_tool_output, new_id, now_rfc3339, ContextPolicy};

// ── Adaptive context assembly ─────────────────────────────────────────

/// Snip an artifact's text to fit within a token budget.
pub fn microcompact_artifact(artifact: &mut ArtifactRecord, max_tokens: usize) {
    let max_chars = max_tokens * 4;
    if let Some(ref text) = artifact.text {
        if text.len() > max_chars && max_chars >= 200 {
            artifact.text = Some(microcompact_tool_output(text, max_chars));
            artifact.estimated_tokens = Some(max_tokens);
        }
    }
}

/// Deduplicate artifacts by removing those with identical text content,
/// keeping the one with higher priority.
pub fn dedup_artifacts(artifacts: &mut Vec<ArtifactRecord>) {
    let mut seen_hashes: BTreeSet<u64> = BTreeSet::new();
    artifacts.retain(|artifact| {
        let text = artifact.text.as_deref().unwrap_or("");
        if text.is_empty() {
            return true;
        }
        // Simple hash for dedup
        let hash = {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            text.hash(&mut hasher);
            hasher.finish()
        };
        seen_hashes.insert(hash)
    });
}

/// Enhanced artifact selection: dedup, microcompact oversized artifacts,
/// then delegate to the standard `select_artifacts`.
pub fn select_artifacts_adaptive(
    mut artifacts: Vec<ArtifactRecord>,
    policy: &ContextPolicy,
) -> Vec<ArtifactRecord> {
    // Phase 1: deduplicate
    dedup_artifacts(&mut artifacts);

    // Phase 2: microcompact oversized artifacts relative to budget.
    // Cap individual artifacts to a fraction of the total budget, but don't
    // let the per-artifact cap exceed the total budget (avoid overrun).
    if let Some(max_tokens) = policy.max_tokens {
        let count = artifacts.len().max(1);
        let per_artifact_budget = max_tokens / count;
        // Floor of 500 tokens, but never more than total budget
        let cap = per_artifact_budget.max(500).min(max_tokens);
        for artifact in &mut artifacts {
            let est = artifact.estimated_tokens.unwrap_or(0);
            if est > cap * 2 {
                microcompact_artifact(artifact, cap);
            }
        }
    }

    // Phase 3: standard selection with budget
    select_artifacts(artifacts, policy)
}

fn normalize_artifact_kind(kind: &str) -> String {
    match kind {
        "resource"
        | "workspace_file"
        | "editor_selection"
        | "workspace_snapshot"
        | "transcript_summary"
        | "summary"
        | "plan"
        | "diff"
        | "git_diff"
        | "patch"
        | "patch_set"
        | "patch_proposal"
        | "diff_review"
        | "review_decision"
        | "verification_bundle"
        | "apply_intent"
        | "verification_result"
        | "test_result"
        | "command_result"
        | "provider_payload"
        | "worker_result"
        | "worker_notification"
        | "artifact" => kind.to_string(),
        "file" => "workspace_file".to_string(),
        "transcript" => "transcript_summary".to_string(),
        "verification" => "verification_result".to_string(),
        "test" => "test_result".to_string(),
        other if other.trim().is_empty() => "artifact".to_string(),
        other => other.to_string(),
    }
}

fn default_artifact_priority(kind: &str) -> i64 {
    match kind {
        "verification_result" | "test_result" => 100,
        "verification_bundle" => 95,
        "diff" | "git_diff" | "patch" | "patch_set" | "patch_proposal" | "diff_review"
        | "review_decision" | "apply_intent" => 90,
        "plan" => 80,
        "workspace_file" | "workspace_snapshot" | "editor_selection" | "resource" => 70,
        "summary" | "transcript_summary" => 60,
        "command_result" => 50,
        _ => 40,
    }
}

fn freshness_rank(value: Option<&str>) -> i64 {
    match value.unwrap_or_default() {
        "fresh" | "live" => 3,
        "recent" => 2,
        "stale" => 0,
        _ => 1,
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ArtifactRecord {
    #[serde(rename = "_type")]
    pub type_name: String,
    pub id: String,
    pub kind: String,
    pub title: Option<String>,
    pub text: Option<String>,
    pub data: Option<serde_json::Value>,
    pub source: Option<String>,
    pub created_at: String,
    pub freshness: Option<String>,
    pub priority: Option<i64>,
    pub lineage: Vec<String>,
    pub relevance: Option<f64>,
    pub estimated_tokens: Option<usize>,
    pub stage: Option<String>,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

impl ArtifactRecord {
    pub fn normalize(mut self) -> Self {
        if self.type_name.is_empty() {
            self.type_name = "artifact".to_string();
        }
        if self.id.is_empty() {
            self.id = new_id("artifact");
        }
        if self.created_at.is_empty() {
            self.created_at = now_rfc3339();
        }
        if self.kind.is_empty() {
            self.kind = "artifact".to_string();
        }
        self.kind = normalize_artifact_kind(&self.kind);
        if self.estimated_tokens.is_none() {
            self.estimated_tokens = self
                .text
                .as_ref()
                .map(|text| ((text.len() as f64) / 4.0).ceil() as usize);
        }
        if self.priority.is_none() {
            self.priority = Some(default_artifact_priority(&self.kind));
        }
        self
    }
}

pub fn select_artifacts(
    mut artifacts: Vec<ArtifactRecord>,
    policy: &ContextPolicy,
) -> Vec<ArtifactRecord> {
    artifacts.retain(|artifact| {
        (policy.include_kinds.is_empty() || policy.include_kinds.contains(&artifact.kind))
            && !policy.exclude_kinds.contains(&artifact.kind)
            && (policy.include_stages.is_empty()
                || artifact
                    .stage
                    .as_ref()
                    .is_some_and(|stage| policy.include_stages.contains(stage)))
    });
    artifacts.sort_by(|a, b| {
        let b_pinned = policy.pinned_ids.contains(&b.id);
        let a_pinned = policy.pinned_ids.contains(&a.id);
        b_pinned
            .cmp(&a_pinned)
            .then_with(|| {
                let b_prio_kind = policy.prioritize_kinds.contains(&b.kind);
                let a_prio_kind = policy.prioritize_kinds.contains(&a.kind);
                b_prio_kind.cmp(&a_prio_kind)
            })
            .then_with(|| {
                b.priority
                    .unwrap_or_default()
                    .cmp(&a.priority.unwrap_or_default())
            })
            .then_with(|| {
                if policy.prefer_fresh {
                    freshness_rank(b.freshness.as_deref())
                        .cmp(&freshness_rank(a.freshness.as_deref()))
                } else {
                    std::cmp::Ordering::Equal
                }
            })
            .then_with(|| {
                if policy.prefer_recent {
                    b.created_at.cmp(&a.created_at)
                } else {
                    std::cmp::Ordering::Equal
                }
            })
            .then_with(|| {
                b.relevance
                    .partial_cmp(&a.relevance)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| {
                a.estimated_tokens
                    .unwrap_or(usize::MAX)
                    .cmp(&b.estimated_tokens.unwrap_or(usize::MAX))
            })
    });

    let mut selected = Vec::new();
    let mut used_tokens = 0usize;
    let reserve_tokens = policy.reserve_tokens.unwrap_or(0);
    let effective_max_tokens = policy
        .max_tokens
        .map(|max| max.saturating_sub(reserve_tokens));
    for artifact in artifacts {
        if let Some(max_artifacts) = policy.max_artifacts {
            if selected.len() >= max_artifacts {
                break;
            }
        }
        let next_tokens = artifact.estimated_tokens.unwrap_or(0);
        if let Some(max_tokens) = effective_max_tokens {
            if used_tokens + next_tokens > max_tokens {
                continue;
            }
        }
        used_tokens += next_tokens;
        selected.push(artifact);
    }
    selected
}

pub fn render_artifacts_context(artifacts: &[ArtifactRecord], policy: &ContextPolicy) -> String {
    let mut parts = Vec::new();
    for artifact in artifacts {
        let title = artifact
            .title
            .clone()
            .unwrap_or_else(|| format!("{} {}", artifact.kind, artifact.id));
        let body = artifact
            .text
            .clone()
            .or_else(|| artifact.data.as_ref().map(|v| v.to_string()))
            .unwrap_or_default();
        match policy.render.as_deref() {
            Some("json") => {
                parts.push(
                    serde_json::json!({
                        "id": artifact.id,
                        "kind": artifact.kind,
                        "title": title,
                        "source": artifact.source,
                        "freshness": artifact.freshness,
                        "priority": artifact.priority,
                        "text": body,
                    })
                    .to_string(),
                );
            }
            _ => parts.push(format!(
                "<artifact>\n<title>{}</title>\n<kind>{}</kind>\n<source>{}</source>\n\
<freshness>{}</freshness>\n<priority>{}</priority>\n<body>\n{}\n</body>\n</artifact>",
                escape_prompt_text(&title),
                escape_prompt_text(&artifact.kind),
                escape_prompt_text(
                    artifact
                        .source
                        .clone()
                        .unwrap_or_else(|| "unknown".to_string())
                        .as_str(),
                ),
                escape_prompt_text(
                    artifact
                        .freshness
                        .clone()
                        .unwrap_or_else(|| "normal".to_string())
                        .as_str(),
                ),
                artifact.priority.unwrap_or_default(),
                body
            )),
        }
    }
    parts.join("\n\n")
}

pub fn render_workflow_prompt(
    task: &str,
    task_label: Option<&str>,
    rendered_context: &str,
) -> String {
    let label = task_label
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("Task");
    let mut prompt = format!(
        "<workflow_task>\n<label>{}</label>\n<instructions>\n{}\n</instructions>\n</workflow_task>",
        escape_prompt_text(label),
        task.trim(),
    );
    let context = rendered_context.trim();
    if !context.is_empty() {
        prompt.push_str("\n\n<workflow_context>\n");
        prompt.push_str(context);
        prompt.push_str("\n</workflow_context>");
    }
    prompt.push_str(
        "\n\n<workflow_response_contract>\n\
Respond to the workflow task above. Do not continue the trailing artifact text verbatim. \
Keep commentary minimal and use the active tool-calling contract for concrete progress.\n\
</workflow_response_contract>",
    );
    prompt
}

fn escape_prompt_text(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

pub fn normalize_artifact(
    value: &crate::value::VmValue,
) -> Result<ArtifactRecord, crate::value::VmError> {
    let artifact: ArtifactRecord = super::parse_json_value(value)?;
    Ok(artifact.normalize())
}
