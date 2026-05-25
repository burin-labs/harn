//! Myers diff primitives, unified-diff rendering, and the `diff_run_records` comparator.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::action_graph::derive_run_observability;
use super::types::{
    RunDiffReport, RunObservabilityDiffRecord, RunRecord, RunStageDiffRecord, RunStageRecord,
    ToolCallDiffRecord, ToolCallRecord,
};

/// Edit operation in a diff sequence.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum DiffOp {
    Equal,
    Delete,
    Insert,
}

/// Compute the shortest edit script using Myers' O(nd) algorithm.
/// Returns a sequence of (DiffOp, line_index_in_before_or_after).
/// Time: O(nd) where d = edit distance. Space: O(d * n).
pub(crate) fn myers_diff(a: &[&str], b: &[&str]) -> Vec<(DiffOp, usize)> {
    let n = a.len() as isize;
    let m = b.len() as isize;
    if n == 0 && m == 0 {
        return Vec::new();
    }
    if n == 0 {
        return (0..m as usize).map(|j| (DiffOp::Insert, j)).collect();
    }
    if m == 0 {
        return (0..n as usize).map(|i| (DiffOp::Delete, i)).collect();
    }

    let max_d = (n + m) as usize;
    let offset = max_d as isize;
    let v_size = 2 * max_d + 1;
    let mut v = vec![0isize; v_size];
    // trace[d] holds the `v` snapshot BEFORE step d ran — required for backtrack.
    let mut trace: Vec<Vec<isize>> = Vec::new();

    'outer: for d in 0..=max_d as isize {
        trace.push(v.clone());
        let mut new_v = v.clone();
        for k in (-d..=d).step_by(2) {
            let ki = (k + offset) as usize;
            // Myers diff: `k == -d` is the bottom boundary, `k != d` is the
            // top boundary — they're distinct edge cases, not a typo. Same
            // story for `x < n && y < m`: `n` bounds `a`, `m` bounds `b`.
            #[allow(clippy::suspicious_operation_groupings)]
            let mut x = if k == -d || (k != d && v[ki - 1] < v[ki + 1]) {
                v[ki + 1]
            } else {
                v[ki - 1] + 1
            };
            let mut y = x - k;
            #[allow(clippy::suspicious_operation_groupings)]
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            new_v[ki] = x;
            if x >= n && y >= m {
                let _ = new_v;
                break 'outer;
            }
        }
        v = new_v;
    }

    let mut ops: Vec<(DiffOp, usize)> = Vec::new();
    let mut x = n;
    let mut y = m;
    for d in (1..trace.len() as isize).rev() {
        let k = x - y;
        let v_prev = &trace[d as usize];
        let prev_k = if k == -d
            || (k != d && v_prev[(k - 1 + offset) as usize] < v_prev[(k + 1 + offset) as usize])
        {
            k + 1
        } else {
            k - 1
        };
        let prev_x = v_prev[(prev_k + offset) as usize];
        let prev_y = prev_x - prev_k;

        while x > prev_x && y > prev_y {
            x -= 1;
            y -= 1;
            ops.push((DiffOp::Equal, x as usize));
        }
        if prev_k < k {
            x -= 1;
            ops.push((DiffOp::Delete, x as usize));
        } else {
            y -= 1;
            ops.push((DiffOp::Insert, y as usize));
        }
    }
    while x > 0 && y > 0 {
        x -= 1;
        y -= 1;
        ops.push((DiffOp::Equal, x as usize));
    }
    ops.reverse();
    ops
}

pub fn render_unified_diff(path: Option<&str>, before: &str, after: &str) -> String {
    let before_lines: Vec<&str> = before.lines().collect();
    let after_lines: Vec<&str> = after.lines().collect();
    let ops = myers_diff(&before_lines, &after_lines);

    let mut diff = String::new();
    let file = path.unwrap_or("artifact");
    diff.push_str(&format!("--- a/{file}\n+++ b/{file}\n"));
    for &(op, idx) in &ops {
        match op {
            DiffOp::Equal => diff.push_str(&format!(" {}\n", before_lines[idx])),
            DiffOp::Delete => diff.push_str(&format!("-{}\n", before_lines[idx])),
            DiffOp::Insert => diff.push_str(&format!("+{}\n", after_lines[idx])),
        }
    }
    diff
}

pub fn diff_run_records(left: &RunRecord, right: &RunRecord) -> RunDiffReport {
    let mut stage_diffs = Vec::new();
    let mut all_node_ids = BTreeSet::new();
    let left_by_id: BTreeMap<&str, &RunStageRecord> = left
        .stages
        .iter()
        .map(|s| (s.node_id.as_str(), s))
        .collect();
    let right_by_id: BTreeMap<&str, &RunStageRecord> = right
        .stages
        .iter()
        .map(|s| (s.node_id.as_str(), s))
        .collect();
    all_node_ids.extend(left_by_id.keys().copied());
    all_node_ids.extend(right_by_id.keys().copied());

    for node_id in all_node_ids {
        let left_stage = left_by_id.get(node_id).copied();
        let right_stage = right_by_id.get(node_id).copied();
        match (left_stage, right_stage) {
            (Some(_), None) => stage_diffs.push(RunStageDiffRecord {
                node_id: node_id.to_string(),
                change: "removed".to_string(),
                details: vec!["stage missing from right run".to_string()],
            }),
            (None, Some(_)) => stage_diffs.push(RunStageDiffRecord {
                node_id: node_id.to_string(),
                change: "added".to_string(),
                details: vec!["stage missing from left run".to_string()],
            }),
            (Some(left_stage), Some(right_stage)) => {
                let mut details = Vec::new();
                if left_stage.status != right_stage.status {
                    details.push(format!(
                        "status: {} -> {}",
                        left_stage.status, right_stage.status
                    ));
                }
                if left_stage.outcome != right_stage.outcome {
                    details.push(format!(
                        "outcome: {} -> {}",
                        left_stage.outcome, right_stage.outcome
                    ));
                }
                if left_stage.branch != right_stage.branch {
                    details.push(format!(
                        "branch: {:?} -> {:?}",
                        left_stage.branch, right_stage.branch
                    ));
                }
                if left_stage.produced_artifact_ids.len() != right_stage.produced_artifact_ids.len()
                {
                    details.push(format!(
                        "produced_artifacts: {} -> {}",
                        left_stage.produced_artifact_ids.len(),
                        right_stage.produced_artifact_ids.len()
                    ));
                }
                if left_stage.artifacts.len() != right_stage.artifacts.len() {
                    details.push(format!(
                        "artifact_records: {} -> {}",
                        left_stage.artifacts.len(),
                        right_stage.artifacts.len()
                    ));
                }
                if !details.is_empty() {
                    stage_diffs.push(RunStageDiffRecord {
                        node_id: node_id.to_string(),
                        change: "changed".to_string(),
                        details,
                    });
                }
            }
            (None, None) => {}
        }
    }

    let mut tool_diffs = Vec::new();
    let left_tools: std::collections::BTreeMap<(String, String), &ToolCallRecord> = left
        .tool_recordings
        .iter()
        .map(|r| ((r.tool_name.clone(), r.args_hash.clone()), r))
        .collect();
    let right_tools: std::collections::BTreeMap<(String, String), &ToolCallRecord> = right
        .tool_recordings
        .iter()
        .map(|r| ((r.tool_name.clone(), r.args_hash.clone()), r))
        .collect();
    let all_tool_keys: std::collections::BTreeSet<_> = left_tools
        .keys()
        .chain(right_tools.keys())
        .cloned()
        .collect();
    for key in &all_tool_keys {
        let l = left_tools.get(key);
        let r = right_tools.get(key);
        let result_changed = match (l, r) {
            (Some(a), Some(b)) => a.result != b.result,
            _ => true,
        };
        if result_changed {
            tool_diffs.push(ToolCallDiffRecord {
                tool_name: key.0.clone(),
                args_hash: key.1.clone(),
                result_changed,
                left_result: l.map(|t| t.result.clone()),
                right_result: r.map(|t| t.result.clone()),
            });
        }
    }

    let left_observability = left.observability.clone().unwrap_or_else(|| {
        derive_run_observability(left, left.persisted_path.as_deref().map(Path::new))
    });
    let right_observability = right.observability.clone().unwrap_or_else(|| {
        derive_run_observability(right, right.persisted_path.as_deref().map(Path::new))
    });
    let mut observability_diffs = Vec::new();

    let left_workers = left_observability
        .worker_lineage
        .iter()
        .map(|worker| {
            (
                worker.worker_id.clone(),
                (
                    worker.status.clone(),
                    worker.run_id.clone(),
                    worker.run_path.clone(),
                ),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let right_workers = right_observability
        .worker_lineage
        .iter()
        .map(|worker| {
            (
                worker.worker_id.clone(),
                (
                    worker.status.clone(),
                    worker.run_id.clone(),
                    worker.run_path.clone(),
                ),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let worker_ids = left_workers
        .keys()
        .chain(right_workers.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    for worker_id in worker_ids {
        match (left_workers.get(&worker_id), right_workers.get(&worker_id)) {
            (Some(_), None) => observability_diffs.push(RunObservabilityDiffRecord {
                section: "worker_lineage".to_string(),
                label: worker_id,
                details: vec!["worker missing from right run".to_string()],
            }),
            (None, Some(_)) => observability_diffs.push(RunObservabilityDiffRecord {
                section: "worker_lineage".to_string(),
                label: worker_id,
                details: vec!["worker missing from left run".to_string()],
            }),
            (Some(left_worker), Some(right_worker)) if left_worker != right_worker => {
                let mut details = Vec::new();
                if left_worker.0 != right_worker.0 {
                    details.push(format!("status: {} -> {}", left_worker.0, right_worker.0));
                }
                if left_worker.1 != right_worker.1 {
                    details.push(format!(
                        "run_id: {:?} -> {:?}",
                        left_worker.1, right_worker.1
                    ));
                }
                if left_worker.2 != right_worker.2 {
                    details.push(format!(
                        "run_path: {:?} -> {:?}",
                        left_worker.2, right_worker.2
                    ));
                }
                observability_diffs.push(RunObservabilityDiffRecord {
                    section: "worker_lineage".to_string(),
                    label: worker_id,
                    details,
                });
            }
            _ => {}
        }
    }

    let left_rounds = left_observability
        .planner_rounds
        .iter()
        .map(|round| (round.stage_id.clone(), round))
        .collect::<BTreeMap<_, _>>();
    let right_rounds = right_observability
        .planner_rounds
        .iter()
        .map(|round| (round.stage_id.clone(), round))
        .collect::<BTreeMap<_, _>>();
    let round_ids = left_rounds
        .keys()
        .chain(right_rounds.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    for stage_id in round_ids {
        match (left_rounds.get(&stage_id), right_rounds.get(&stage_id)) {
            (Some(_), None) => observability_diffs.push(RunObservabilityDiffRecord {
                section: "planner_rounds".to_string(),
                label: stage_id,
                details: vec!["planner summary missing from right run".to_string()],
            }),
            (None, Some(_)) => observability_diffs.push(RunObservabilityDiffRecord {
                section: "planner_rounds".to_string(),
                label: stage_id,
                details: vec!["planner summary missing from left run".to_string()],
            }),
            (Some(left_round), Some(right_round)) => {
                let mut details = Vec::new();
                if left_round.iteration_count != right_round.iteration_count {
                    details.push(format!(
                        "iterations: {} -> {}",
                        left_round.iteration_count, right_round.iteration_count
                    ));
                }
                if left_round.tool_execution_count != right_round.tool_execution_count {
                    details.push(format!(
                        "tool_executions: {} -> {}",
                        left_round.tool_execution_count, right_round.tool_execution_count
                    ));
                }
                if left_round.native_text_tool_fallback_count
                    != right_round.native_text_tool_fallback_count
                {
                    details.push(format!(
                        "native_text_tool_fallbacks: {} -> {}",
                        left_round.native_text_tool_fallback_count,
                        right_round.native_text_tool_fallback_count
                    ));
                }
                if left_round.native_text_tool_fallback_rejection_count
                    != right_round.native_text_tool_fallback_rejection_count
                {
                    details.push(format!(
                        "native_text_tool_fallback_rejections: {} -> {}",
                        left_round.native_text_tool_fallback_rejection_count,
                        right_round.native_text_tool_fallback_rejection_count
                    ));
                }
                if left_round.empty_completion_retry_count
                    != right_round.empty_completion_retry_count
                {
                    details.push(format!(
                        "empty_completion_retries: {} -> {}",
                        left_round.empty_completion_retry_count,
                        right_round.empty_completion_retry_count
                    ));
                }
                if left_round.research_facts != right_round.research_facts {
                    details.push(format!(
                        "research_facts: {:?} -> {:?}",
                        left_round.research_facts, right_round.research_facts
                    ));
                }
                let left_deliverables = left_round
                    .task_ledger
                    .as_ref()
                    .map(|ledger| {
                        ledger
                            .deliverables
                            .iter()
                            .map(|item| format!("{}:{}", item.id, item.status))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let right_deliverables = right_round
                    .task_ledger
                    .as_ref()
                    .map(|ledger| {
                        ledger
                            .deliverables
                            .iter()
                            .map(|item| format!("{}:{}", item.id, item.status))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                if left_deliverables != right_deliverables {
                    details.push(format!(
                        "deliverables: {left_deliverables:?} -> {right_deliverables:?}"
                    ));
                }
                if left_round.successful_tools != right_round.successful_tools {
                    details.push(format!(
                        "successful_tools: {:?} -> {:?}",
                        left_round.successful_tools, right_round.successful_tools
                    ));
                }
                if !details.is_empty() {
                    observability_diffs.push(RunObservabilityDiffRecord {
                        section: "planner_rounds".to_string(),
                        label: left_round.node_id.clone(),
                        details,
                    });
                }
            }
            _ => {}
        }
    }

    let left_pointers = left_observability
        .transcript_pointers
        .iter()
        .map(|pointer| {
            (
                pointer.id.clone(),
                (
                    pointer.available,
                    pointer.path.clone(),
                    pointer.location.clone(),
                ),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let right_pointers = right_observability
        .transcript_pointers
        .iter()
        .map(|pointer| {
            (
                pointer.id.clone(),
                (
                    pointer.available,
                    pointer.path.clone(),
                    pointer.location.clone(),
                ),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let pointer_ids = left_pointers
        .keys()
        .chain(right_pointers.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    for pointer_id in pointer_ids {
        match (
            left_pointers.get(&pointer_id),
            right_pointers.get(&pointer_id),
        ) {
            (Some(_), None) => observability_diffs.push(RunObservabilityDiffRecord {
                section: "transcript_pointers".to_string(),
                label: pointer_id,
                details: vec!["pointer missing from right run".to_string()],
            }),
            (None, Some(_)) => observability_diffs.push(RunObservabilityDiffRecord {
                section: "transcript_pointers".to_string(),
                label: pointer_id,
                details: vec!["pointer missing from left run".to_string()],
            }),
            (Some(left_pointer), Some(right_pointer)) if left_pointer != right_pointer => {
                observability_diffs.push(RunObservabilityDiffRecord {
                    section: "transcript_pointers".to_string(),
                    label: pointer_id,
                    details: vec![format!(
                        "pointer: {:?} -> {:?}",
                        left_pointer, right_pointer
                    )],
                });
            }
            _ => {}
        }
    }

    let left_compactions = left_observability
        .compaction_events
        .iter()
        .map(|event| {
            (
                event.id.clone(),
                (
                    event.strategy.clone(),
                    event.archived_messages,
                    event.snapshot_asset_id.clone(),
                    event.available,
                ),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let right_compactions = right_observability
        .compaction_events
        .iter()
        .map(|event| {
            (
                event.id.clone(),
                (
                    event.strategy.clone(),
                    event.archived_messages,
                    event.snapshot_asset_id.clone(),
                    event.available,
                ),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let compaction_ids = left_compactions
        .keys()
        .chain(right_compactions.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    for compaction_id in compaction_ids {
        match (
            left_compactions.get(&compaction_id),
            right_compactions.get(&compaction_id),
        ) {
            (Some(_), None) => observability_diffs.push(RunObservabilityDiffRecord {
                section: "compaction_events".to_string(),
                label: compaction_id,
                details: vec!["compaction event missing from right run".to_string()],
            }),
            (None, Some(_)) => observability_diffs.push(RunObservabilityDiffRecord {
                section: "compaction_events".to_string(),
                label: compaction_id,
                details: vec!["compaction event missing from left run".to_string()],
            }),
            (Some(left_event), Some(right_event)) if left_event != right_event => {
                observability_diffs.push(RunObservabilityDiffRecord {
                    section: "compaction_events".to_string(),
                    label: compaction_id,
                    details: vec![format!("event: {:?} -> {:?}", left_event, right_event)],
                });
            }
            _ => {}
        }
    }

    let left_daemons = left_observability
        .daemon_events
        .iter()
        .map(|event| {
            (
                (event.daemon_id.clone(), event.kind, event.timestamp.clone()),
                (
                    event.name.clone(),
                    event.persist_path.clone(),
                    event.payload_summary.clone(),
                ),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let right_daemons = right_observability
        .daemon_events
        .iter()
        .map(|event| {
            (
                (event.daemon_id.clone(), event.kind, event.timestamp.clone()),
                (
                    event.name.clone(),
                    event.persist_path.clone(),
                    event.payload_summary.clone(),
                ),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let daemon_keys = left_daemons
        .keys()
        .chain(right_daemons.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    for daemon_key in daemon_keys {
        let label = format!("{}:{:?}:{}", daemon_key.0, daemon_key.1, daemon_key.2);
        match (
            left_daemons.get(&daemon_key),
            right_daemons.get(&daemon_key),
        ) {
            (Some(_), None) => observability_diffs.push(RunObservabilityDiffRecord {
                section: "daemon_events".to_string(),
                label,
                details: vec!["daemon event missing from right run".to_string()],
            }),
            (None, Some(_)) => observability_diffs.push(RunObservabilityDiffRecord {
                section: "daemon_events".to_string(),
                label,
                details: vec!["daemon event missing from left run".to_string()],
            }),
            (Some(left_event), Some(right_event)) if left_event != right_event => {
                observability_diffs.push(RunObservabilityDiffRecord {
                    section: "daemon_events".to_string(),
                    label,
                    details: vec![format!("event: {:?} -> {:?}", left_event, right_event)],
                });
            }
            _ => {}
        }
    }

    let left_verification = left_observability
        .verification_outcomes
        .iter()
        .map(|item| (item.stage_id.clone(), item))
        .collect::<BTreeMap<_, _>>();
    let right_verification = right_observability
        .verification_outcomes
        .iter()
        .map(|item| (item.stage_id.clone(), item))
        .collect::<BTreeMap<_, _>>();
    let verification_ids = left_verification
        .keys()
        .chain(right_verification.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    for stage_id in verification_ids {
        match (
            left_verification.get(&stage_id),
            right_verification.get(&stage_id),
        ) {
            (Some(_), None) => observability_diffs.push(RunObservabilityDiffRecord {
                section: "verification".to_string(),
                label: stage_id,
                details: vec!["verification missing from right run".to_string()],
            }),
            (None, Some(_)) => observability_diffs.push(RunObservabilityDiffRecord {
                section: "verification".to_string(),
                label: stage_id,
                details: vec!["verification missing from left run".to_string()],
            }),
            (Some(left_item), Some(right_item)) if left_item != right_item => {
                let mut details = Vec::new();
                if left_item.passed != right_item.passed {
                    details.push(format!(
                        "passed: {:?} -> {:?}",
                        left_item.passed, right_item.passed
                    ));
                }
                if left_item.summary != right_item.summary {
                    details.push(format!(
                        "summary: {:?} -> {:?}",
                        left_item.summary, right_item.summary
                    ));
                }
                observability_diffs.push(RunObservabilityDiffRecord {
                    section: "verification".to_string(),
                    label: left_item.node_id.clone(),
                    details,
                });
            }
            _ => {}
        }
    }

    let left_graph = (
        left_observability.action_graph_nodes.len(),
        left_observability.action_graph_edges.len(),
    );
    let right_graph = (
        right_observability.action_graph_nodes.len(),
        right_observability.action_graph_edges.len(),
    );
    if left_graph != right_graph {
        observability_diffs.push(RunObservabilityDiffRecord {
            section: "action_graph".to_string(),
            label: "shape".to_string(),
            details: vec![format!(
                "nodes/edges: {}/{} -> {}/{}",
                left_graph.0, left_graph.1, right_graph.0, right_graph.1
            )],
        });
    }

    let status_changed = left.status != right.status;
    let identical = !status_changed
        && stage_diffs.is_empty()
        && tool_diffs.is_empty()
        && observability_diffs.is_empty()
        && left.transitions.len() == right.transitions.len()
        && left.artifacts.len() == right.artifacts.len()
        && left.checkpoints.len() == right.checkpoints.len();

    RunDiffReport {
        left_run_id: left.id.clone(),
        right_run_id: right.id.clone(),
        identical,
        status_changed,
        left_status: left.status.clone(),
        right_status: right.status.clone(),
        stage_diffs,
        tool_diffs,
        observability_diffs,
        transition_count_delta: right.transitions.len() as isize - left.transitions.len() as isize,
        artifact_count_delta: right.artifacts.len() as isize - left.artifacts.len() as isize,
        checkpoint_count_delta: right.checkpoints.len() as isize - left.checkpoints.len() as isize,
    }
}
