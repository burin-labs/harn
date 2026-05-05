//! Side-effect-free skill scoring for the Harn-driven agent loop.
//!
//! Activation, sticky reuse, prompt composition, and tool-surface policy
//! live in `std/agent/skills.harn`. This module only turns a task context
//! plus a skill registry into ranked score records.

use std::collections::BTreeMap;
use std::rc::Rc;

use crate::bridge::HostBridge;
use crate::value::{VmError, VmValue};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum SkillMatchStrategy {
    #[default]
    Metadata,
    Host,
    Embedding,
}

impl SkillMatchStrategy {
    fn parse(value: Option<&VmValue>) -> Self {
        let Some(value) = value else {
            return Self::Metadata;
        };
        match value.display().trim().to_ascii_lowercase().as_str() {
            "host" => Self::Host,
            "embedding" => Self::Embedding,
            "metadata" | "" => Self::Metadata,
            other => {
                crate::events::log_warn(
                    "agent.skill_score",
                    &format!("unknown strategy '{other}', falling back to 'metadata'"),
                );
                Self::Metadata
            }
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Metadata => "metadata",
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
    bridge: Option<Rc<HostBridge>>,
) -> Result<VmValue, VmError> {
    let skills: Vec<VmValue> = extract_skills(registry)
        .into_iter()
        .filter(|skill| !skill_disabled_for_model(skill))
        .collect();
    let options = options.as_dict().cloned().unwrap_or_default();
    let strategy = SkillMatchStrategy::parse(options.get("strategy"));
    let task = context
        .as_dict()
        .and_then(|dict| dict.get("task"))
        .map(VmValue::display)
        .unwrap_or_default();
    let working_files = list_strings(context.as_dict().and_then(|dict| dict.get("working_files")));

    let scored = match strategy {
        SkillMatchStrategy::Metadata => score_metadata(&skills, &task, &working_files),
        SkillMatchStrategy::Host | SkillMatchStrategy::Embedding => {
            match score_via_bridge(bridge.as_deref(), &skills, &task, &working_files, strategy)
                .await
            {
                Ok(scored) => scored,
                Err(error) => {
                    crate::events::log_warn(
                        "agent.skill_score",
                        &format!(
                            "{} strategy failed: {error}; falling back to metadata scoring",
                            strategy.as_str()
                        ),
                    );
                    score_metadata(&skills, &task, &working_files)
                }
            }
        }
    };

    let scored_values = scored.into_iter().map(candidate_to_vm).collect();
    Ok(VmValue::Dict(Rc::new(BTreeMap::from([(
        "scored".to_string(),
        VmValue::List(Rc::new(scored_values)),
    )]))))
}

fn candidate_to_vm(candidate: SkillCandidate) -> VmValue {
    VmValue::Dict(Rc::new(BTreeMap::from([
        (
            "id".to_string(),
            VmValue::String(Rc::from(candidate.id.clone())),
        ),
        ("name".to_string(), VmValue::String(Rc::from(candidate.id))),
        ("score".to_string(), VmValue::Float(candidate.score)),
        (
            "trigger".to_string(),
            VmValue::String(Rc::from(candidate.trigger.clone())),
        ),
        (
            "reason".to_string(),
            VmValue::String(Rc::from(candidate.trigger)),
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

fn skill_disabled_for_model(skill: &VmValue) -> bool {
    let Some(dict) = skill.as_dict() else {
        return false;
    };
    matches!(
        dict.get("disable-model-invocation")
            .or_else(|| dict.get("disable_model_invocation")),
        Some(VmValue::Bool(true))
    )
}

fn score_metadata(skills: &[VmValue], task: &str, working_files: &[String]) -> Vec<SkillCandidate> {
    let tokens = tokenize_lower(task);
    let lower_task = task.to_lowercase();
    let mut candidates = Vec::new();
    for skill in skills {
        if skill_disabled_for_model(skill) {
            continue;
        }
        let Some(dict) = skill.as_dict() else {
            continue;
        };
        let id = dict.get("name").map(VmValue::display).unwrap_or_default();
        if id.is_empty() {
            continue;
        }
        let description = dict
            .get("description")
            .map(VmValue::display)
            .unwrap_or_default();
        let when_to_use = dict
            .get("when_to_use")
            .map(VmValue::display)
            .unwrap_or_default();
        let paths = list_strings(dict.get("paths"));

        let mut score = 0.0_f64;
        let mut triggers = Vec::new();
        let keyword_hits =
            count_term_hits(&tokens, &description) + count_term_hits(&tokens, &when_to_use);
        if keyword_hits > 0 {
            let bm25 = (keyword_hits as f64) / (keyword_hits as f64 + 1.5);
            score += bm25;
            triggers.push(format!("{keyword_hits} keyword hit(s)"));
        }
        if lower_task.contains(&id.to_lowercase()) {
            score += 2.0;
            triggers.push(format!("task mentions '{id}'"));
        }
        let path_hits = count_path_hits(&paths, working_files);
        if path_hits > 0 {
            score += 1.5 * (path_hits as f64);
            triggers.push(format!("{path_hits} path glob(s) matched"));
        }
        if score > 0.0 {
            candidates.push(SkillCandidate {
                id,
                score,
                trigger: triggers.join("; "),
            });
        }
    }
    candidates.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.id.cmp(&right.id))
    });
    candidates
}

fn tokenize_lower(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|token| token.len() > 2)
        .map(str::to_lowercase)
        .collect()
}

fn count_term_hits(terms: &[String], haystack: &str) -> usize {
    if terms.is_empty() || haystack.is_empty() {
        return 0;
    }
    let lower = haystack.to_lowercase();
    terms
        .iter()
        .filter(|term| lower.contains(term.as_str()))
        .count()
}

fn count_path_hits(patterns: &[String], working_files: &[String]) -> usize {
    let mut hits = 0;
    for pattern in patterns {
        for file in working_files {
            if glob_match(pattern, file) {
                hits += 1;
                break;
            }
        }
    }
    hits
}

fn glob_match(pattern: &str, path: &str) -> bool {
    glob_match_inner(pattern.as_bytes(), 0, path.as_bytes(), 0)
}

fn glob_match_inner(pattern: &[u8], mut pi: usize, path: &[u8], si: usize) -> bool {
    let mut si = si;
    while pi < pattern.len() {
        match pattern[pi] {
            b'*' => {
                let double = pi + 1 < pattern.len() && pattern[pi + 1] == b'*';
                let next_pi = if double { pi + 2 } else { pi + 1 };
                let next_pi = if double && next_pi < pattern.len() && pattern[next_pi] == b'/' {
                    next_pi + 1
                } else {
                    next_pi
                };
                if next_pi >= pattern.len() {
                    return double || !path[si..].contains(&b'/');
                }
                for try_si in si..=path.len() {
                    if !double && path[si..try_si].contains(&b'/') {
                        break;
                    }
                    if glob_match_inner(pattern, next_pi, path, try_si) {
                        return true;
                    }
                }
                return false;
            }
            b'?' => {
                if si >= path.len() || path[si] == b'/' {
                    return false;
                }
                pi += 1;
                si += 1;
            }
            expected => {
                if si >= path.len() || path[si] != expected {
                    return false;
                }
                pi += 1;
                si += 1;
            }
        }
    }
    si == path.len()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(fields: &[(&str, VmValue)]) -> VmValue {
        VmValue::Dict(Rc::new(
            fields
                .iter()
                .map(|(key, value)| ((*key).to_string(), value.clone()))
                .collect(),
        ))
    }

    #[test]
    fn metadata_ranks_keyword_matches() {
        let ranked = score_metadata(
            &[
                skill(&[
                    ("name", VmValue::String(Rc::from("deploy"))),
                    (
                        "description",
                        VmValue::String(Rc::from("Deploy application releases")),
                    ),
                    (
                        "when_to_use",
                        VmValue::String(Rc::from("User asks to ship or deploy")),
                    ),
                ]),
                skill(&[
                    ("name", VmValue::String(Rc::from("metrics"))),
                    (
                        "description",
                        VmValue::String(Rc::from("Inspect dashboards")),
                    ),
                ]),
            ],
            "please deploy the service",
            &[],
        );
        assert_eq!(ranked[0].id, "deploy");
    }

    #[test]
    fn path_globs_match_working_files() {
        let ranked = score_metadata(
            &[skill(&[
                ("name", VmValue::String(Rc::from("infra"))),
                ("description", VmValue::String(Rc::from("Infrastructure"))),
                (
                    "paths",
                    VmValue::List(Rc::new(vec![
                        VmValue::String(Rc::from("infra/**")),
                        VmValue::String(Rc::from("Dockerfile")),
                    ])),
                ),
            ])],
            "unrelated",
            &["infra/terraform/main.tf".to_string()],
        );
        assert_eq!(ranked.len(), 1);
        assert!(ranked[0].trigger.contains("path"));
    }

    #[test]
    fn disabled_skills_are_hidden_from_metadata_scoring() {
        let ranked = score_metadata(
            &[skill(&[
                ("name", VmValue::String(Rc::from("secret"))),
                (
                    "description",
                    VmValue::String(Rc::from("Rotate database credentials")),
                ),
                ("disable-model-invocation", VmValue::Bool(true)),
            ])],
            "rotate database credentials",
            &[],
        );
        assert!(ranked.is_empty());
    }
}
