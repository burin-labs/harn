//! Bridge-backed skill scoring for the Harn-driven agent loop.
//!
//! Activation, metadata matching, sticky reuse, prompt composition, and
//! tool-surface policy live in `std/agent/skills.harn`. This module is the
//! primitive host/embedding bridge adapter for semantic matchers.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::bridge::HostBridge;
use crate::value::{VmError, VmValue};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SkillMatchStrategy {
    Host,
    Embedding,
}

impl SkillMatchStrategy {
    fn parse(value: Option<&VmValue>) -> Result<Self, VmError> {
        let strategy = value
            .map(VmValue::display)
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        match strategy.as_str() {
            "host" => Ok(Self::Host),
            "embedding" => Ok(Self::Embedding),
            "metadata" | "" => Err(VmError::Runtime(
                "metadata skill scoring is implemented in std/agent/skills.harn".to_string(),
            )),
            other => Err(VmError::Runtime(format!(
                "unknown skill scoring strategy '{other}'"
            ))),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::Embedding => "embedding",
        }
    }
}

#[derive(Clone, Debug)]
struct SkillCandidate {
    id: String,
    score: f64,
    trigger: String,
}

pub(crate) async fn score_skill_registry(
    context: &VmValue,
    registry: &VmValue,
    options: &VmValue,
    bridge: Option<Arc<HostBridge>>,
) -> Result<VmValue, VmError> {
    let skills = extract_skills(registry);
    let options = options.as_dict().cloned().unwrap_or_default();
    let strategy = SkillMatchStrategy::parse(options.get("strategy"))?;
    let task = context
        .as_dict()
        .and_then(|dict| dict.get("task"))
        .map(VmValue::display)
        .unwrap_or_default();
    let working_files = list_strings(context.as_dict().and_then(|dict| dict.get("working_files")));

    let scored =
        score_via_bridge(bridge.as_deref(), &skills, &task, &working_files, strategy).await?;

    let scored_values = scored.into_iter().map(candidate_to_vm).collect();
    Ok(VmValue::Dict(std::sync::Arc::new(BTreeMap::from([(
        "scored".to_string(),
        VmValue::List(std::sync::Arc::new(scored_values)),
    )]))))
}

fn candidate_to_vm(candidate: SkillCandidate) -> VmValue {
    VmValue::Dict(std::sync::Arc::new(BTreeMap::from([
        (
            "id".to_string(),
            VmValue::String(std::sync::Arc::from(candidate.id.clone())),
        ),
        (
            "name".to_string(),
            VmValue::String(std::sync::Arc::from(candidate.id)),
        ),
        ("score".to_string(), VmValue::Float(candidate.score)),
        (
            "trigger".to_string(),
            VmValue::String(std::sync::Arc::from(candidate.trigger.clone())),
        ),
        (
            "reason".to_string(),
            VmValue::String(std::sync::Arc::from(candidate.trigger)),
        ),
    ])))
}

fn extract_skills(registry: &VmValue) -> Vec<VmValue> {
    let Some(dict) = registry.as_dict() else {
        return Vec::new();
    };
    match dict.get("skills") {
        Some(VmValue::List(list)) => list.iter().cloned().collect(),
        _ => Vec::new(),
    }
}

fn list_strings(value: Option<&VmValue>) -> Vec<String> {
    match value {
        Some(VmValue::List(list)) => list
            .iter()
            .map(VmValue::display)
            .filter(|text| !text.is_empty())
            .collect(),
        Some(VmValue::String(text)) if !text.is_empty() => vec![text.to_string()],
        _ => Vec::new(),
    }
}

async fn score_via_bridge(
    bridge: Option<&HostBridge>,
    skills: &[VmValue],
    task: &str,
    working_files: &[String],
    strategy: SkillMatchStrategy,
) -> Result<Vec<SkillCandidate>, VmError> {
    let Some(bridge) = bridge else {
        return Err(VmError::Runtime(
            "skill scoring strategy requires a host bridge".to_string(),
        ));
    };
    let candidates: Vec<serde_json::Value> = skills
        .iter()
        .filter_map(VmValue::as_dict)
        .map(|dict| {
            serde_json::json!({
                "name": dict.get("name").map(VmValue::display).unwrap_or_default(),
                "description": dict.get("description").map(VmValue::display).unwrap_or_default(),
                "when_to_use": dict.get("when_to_use").map(VmValue::display).unwrap_or_default(),
                "paths": list_strings(dict.get("paths")),
            })
        })
        .collect();
    let response = bridge
        .call(
            "skill/match",
            serde_json::json!({
                "strategy": strategy.as_str(),
                "prompt": task,
                "task": task,
                "working_files": working_files,
                "candidates": candidates,
            }),
        )
        .await?;
    let list = response
        .get("matches")
        .or_else(|| response.get("skills"))
        .or_else(|| {
            response
                .get("result")
                .and_then(|result| result.get("matches"))
        })
        .cloned()
        .or_else(|| response.is_array().then_some(response))
        .unwrap_or_else(|| serde_json::Value::Array(Vec::new()));
    let mut out = Vec::new();
    for entry in list.as_array().cloned().unwrap_or_default() {
        let id = entry
            .get("id")
            .or_else(|| entry.get("name"))
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string();
        if id.is_empty() {
            continue;
        }
        let score = entry
            .get("score")
            .and_then(|value| value.as_f64())
            .unwrap_or(1.0);
        let trigger = entry
            .get("trigger")
            .or_else(|| entry.get("reason"))
            .and_then(|value| value.as_str())
            .unwrap_or("host match")
            .to_string();
        out.push(SkillCandidate { id, score, trigger });
    }
    out.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(out)
}
