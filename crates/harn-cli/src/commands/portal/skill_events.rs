//! Extract skill-activation and tool-search events from a persisted
//! `RunRecord` for the portal's observability panels.
//!
//! The agent loop writes these events into `state.transcript_events`
//! at the time they fire (see `crates/harn-vm/src/llm/agent/`), which
//! ultimately land under `run.transcript.events` and under each
//! stage's `stage.transcript.events` when pipelines flush their own
//! per-stage transcripts. This module walks both locations, pairs
//! `skill_activated` / `skill_deactivated` into intervals, and pairs
//! `tool_search_query` / `tool_search_result` into waterfall rows so
//! the React side can render them without re-parsing JSON shapes.

use std::collections::HashMap;

use super::dto::{
    PortalSkillMatchCandidate, PortalSkillMatchEvent, PortalSkillTimelineEntry, PortalToolLoadEvent,
};

/// Aggregated view of all skill-/tool-related events extracted from a
/// run. Each bucket is empty when the run didn't exercise that path.
pub(super) struct ExtractedEvents {
    pub(super) skill_timeline: Vec<PortalSkillTimelineEntry>,
    pub(super) skill_matches: Vec<PortalSkillMatchEvent>,
    pub(super) tool_loads: Vec<PortalToolLoadEvent>,
    pub(super) active_skills: Vec<String>,
}

/// Collect events from every transcript embedded in the run record
/// (run-level + per-stage) and fold them into
/// [`ExtractedEvents`]. Safe to call with a run that recorded no
/// transcript at all — returns an empty aggregate.
pub(super) fn extract(run: &harn_vm::orchestration::RunRecord) -> ExtractedEvents {
    let mut raw_events: Vec<(String, serde_json::Value)> = Vec::new();

    if let Some(transcript) = &run.transcript {
        collect_events(transcript, "run", &mut raw_events);
    }
    for stage in &run.stages {
        if let Some(transcript) = &stage.transcript {
            collect_events(
                transcript,
                &format!("stage:{}", stage.node_id),
                &mut raw_events,
            );
        }
    }

    // Preserve discovery order so iteration progression reads
    // top-to-bottom in the UI.
    build_aggregate(&raw_events)
}

fn collect_events(
    value: &serde_json::Value,
    scope: &str,
    out: &mut Vec<(String, serde_json::Value)>,
) {
    let Some(events) = value.get("events").and_then(|v| v.as_array()) else {
        return;
    };
    for event in events {
        out.push((scope.to_string(), event.clone()));
    }
}

fn build_aggregate(events: &[(String, serde_json::Value)]) -> ExtractedEvents {
    let mut timeline: Vec<PortalSkillTimelineEntry> = Vec::new();
    let mut pending_skill: HashMap<String, usize> = HashMap::new();
    let mut matches: Vec<PortalSkillMatchEvent> = Vec::new();
    let mut tool_loads: Vec<PortalToolLoadEvent> = Vec::new();
    let mut tool_query_by_id: HashMap<String, usize> = HashMap::new();
    let mut active_skills_set: Vec<String> = Vec::new();

    for (scope, event) in events {
        let kind = event
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let metadata = event.get("metadata");
        match kind {
            "skill_matched" => matches.push(extract_skill_match(
                metadata.cloned().unwrap_or(serde_json::Value::Null),
            )),
            "skill_activated" => {
                let entry = extract_skill_activated(
                    metadata.cloned().unwrap_or(serde_json::Value::Null),
                    scope,
                );
                if !active_skills_set.contains(&entry.name) {
                    active_skills_set.push(entry.name.clone());
                }
                pending_skill.insert(entry.name.clone(), timeline.len());
                timeline.push(entry);
            }
            "skill_deactivated" => {
                let name = metadata
                    .and_then(|m| m.get("name").and_then(|v| v.as_str()))
                    .unwrap_or_default()
                    .to_string();
                let iter = metadata.and_then(|m| m.get("iteration").and_then(|v| v.as_i64()));
                if let Some(idx) = pending_skill.remove(&name) {
                    if let Some(entry) = timeline.get_mut(idx) {
                        entry.deactivated_iteration = iter;
                    }
                }
            }
            "skill_scope_tools" => {
                let name = metadata
                    .and_then(|m| m.get("name").and_then(|v| v.as_str()))
                    .unwrap_or_default()
                    .to_string();
                let tools = metadata
                    .and_then(|m| m.get("allowed_tools").and_then(|v| v.as_array()))
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(str::to_string))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                if let Some(&idx) = pending_skill.get(&name) {
                    if let Some(entry) = timeline.get_mut(idx) {
                        if !tools.is_empty() {
                            entry.allowed_tools = tools;
                        }
                    }
                }
            }
            "tool_search_query" => {
                let id = metadata
                    .and_then(|m| m.get("id").and_then(|v| v.as_str()))
                    .unwrap_or_default()
                    .to_string();
                let query_raw = metadata
                    .and_then(|m| m.get("query"))
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                let query_str = query_raw
                    .get("query")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                    .or_else(|| query_raw.as_str().map(str::to_string))
                    .unwrap_or_else(|| query_raw.to_string());
                let strategy = metadata
                    .and_then(|m| m.get("strategy").and_then(|v| v.as_str()))
                    .unwrap_or("")
                    .to_string();
                let mode = metadata
                    .and_then(|m| m.get("mode").and_then(|v| v.as_str()))
                    .unwrap_or("")
                    .to_string();
                let entry = PortalToolLoadEvent {
                    query: query_str,
                    strategy,
                    mode,
                    tool_use_id: if id.is_empty() {
                        None
                    } else {
                        Some(id.clone())
                    },
                    promoted: Vec::new(),
                    references: Vec::new(),
                    iteration: None,
                    scope: scope.to_string(),
                };
                if !id.is_empty() {
                    tool_query_by_id.insert(id, tool_loads.len());
                }
                tool_loads.push(entry);
            }
            "tool_search_result" => {
                let tool_use_id = metadata
                    .and_then(|m| m.get("tool_use_id").and_then(|v| v.as_str()))
                    .unwrap_or_default()
                    .to_string();
                let promoted = metadata
                    .and_then(|m| m.get("promoted").and_then(|v| v.as_array()))
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(str::to_string))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let references = metadata
                    .and_then(|m| m.get("tool_references").and_then(|v| v.as_array()))
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| {
                                v.get("tool_name")
                                    .and_then(|n| n.as_str())
                                    .map(str::to_string)
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                if let Some(idx) = tool_query_by_id.remove(&tool_use_id) {
                    if let Some(entry) = tool_loads.get_mut(idx) {
                        entry.promoted = promoted;
                        entry.references = references;
                    }
                } else {
                    // Orphan result with no preceding query — still surface it
                    // so replay views don't silently swallow a tool-load row.
                    tool_loads.push(PortalToolLoadEvent {
                        query: String::new(),
                        strategy: String::new(),
                        mode: String::new(),
                        tool_use_id: if tool_use_id.is_empty() {
                            None
                        } else {
                            Some(tool_use_id)
                        },
                        promoted,
                        references,
                        iteration: None,
                        scope: scope.to_string(),
                    });
                }
            }
            _ => {}
        }
    }

    ExtractedEvents {
        skill_timeline: timeline,
        skill_matches: matches,
        tool_loads,
        active_skills: active_skills_set,
    }
}

fn extract_skill_match(metadata: serde_json::Value) -> PortalSkillMatchEvent {
    let iteration = metadata
        .get("iteration")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let strategy = metadata
        .get("strategy")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let reassess = metadata
        .get("reassess")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let working_files = metadata
        .get("working_files")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let candidates = metadata
        .get("candidates")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .map(|c| PortalSkillMatchCandidate {
                    name: c
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    score: c.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0),
                    reason: c
                        .get("reason")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    activated: false,
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    PortalSkillMatchEvent {
        iteration,
        strategy,
        reassess,
        working_files,
        candidates,
    }
}

fn extract_skill_activated(metadata: serde_json::Value, scope: &str) -> PortalSkillTimelineEntry {
    let name = metadata
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let description = metadata
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let iteration = metadata
        .get("iteration")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let score = metadata.get("score").and_then(|v| v.as_f64());
    let reason = metadata
        .get("reason")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let allowed_tools = metadata
        .get("allowed_tools")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    PortalSkillTimelineEntry {
        name,
        description,
        activated_iteration: iteration,
        deactivated_iteration: None,
        score,
        reason,
        allowed_tools,
        scope: scope.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn event(kind: &str, metadata: serde_json::Value) -> serde_json::Value {
        json!({"kind": kind, "metadata": metadata})
    }

    #[test]
    fn pairs_activation_with_deactivation() {
        let raw = vec![
            (
                "run".to_string(),
                event(
                    "skill_matched",
                    json!({
                        "iteration": 0,
                        "strategy": "metadata",
                        "reassess": false,
                        "candidates": [{"name": "deploy", "score": 2.1, "reason": "mention"}],
                        "working_files": []
                    }),
                ),
            ),
            (
                "run".to_string(),
                event(
                    "skill_activated",
                    json!({
                        "name": "deploy",
                        "description": "Deploy the app",
                        "iteration": 0,
                        "score": 2.1,
                        "reason": "mention",
                        "allowed_tools": ["run", "read_file"]
                    }),
                ),
            ),
            (
                "run".to_string(),
                event(
                    "skill_deactivated",
                    json!({"name": "deploy", "iteration": 3}),
                ),
            ),
        ];
        let agg = build_aggregate(&raw);
        assert_eq!(agg.skill_timeline.len(), 1);
        assert_eq!(agg.skill_timeline[0].name, "deploy");
        assert_eq!(agg.skill_timeline[0].activated_iteration, 0);
        assert_eq!(agg.skill_timeline[0].deactivated_iteration, Some(3));
        assert_eq!(agg.skill_matches.len(), 1);
        assert_eq!(agg.active_skills, vec!["deploy".to_string()]);
    }

    #[test]
    fn pairs_tool_search_query_with_result() {
        let raw = vec![
            (
                "run".to_string(),
                event(
                    "tool_search_query",
                    json!({
                        "id": "tool_1",
                        "query": {"query": "send slack"},
                        "strategy": "bm25",
                        "mode": "client",
                    }),
                ),
            ),
            (
                "run".to_string(),
                event(
                    "tool_search_result",
                    json!({
                        "tool_use_id": "tool_1",
                        "promoted": ["slack_post"],
                        "tool_references": [{"tool_name": "slack_post"}]
                    }),
                ),
            ),
        ];
        let agg = build_aggregate(&raw);
        assert_eq!(agg.tool_loads.len(), 1);
        assert_eq!(agg.tool_loads[0].query, "send slack");
        assert_eq!(agg.tool_loads[0].promoted, vec!["slack_post".to_string()]);
        assert_eq!(agg.tool_loads[0].references, vec!["slack_post".to_string()]);
    }
}
