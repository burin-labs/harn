//! Synthesize a canonical Merge Captain JSONL transcript from a
//! `PlaygroundState`. This is what the existing `--backend mock` driver
//! produces when pointed at a real on-disk playground (vs. the transcript
//! replay path, which still works for the preexisting JSON manifest).
//!
//! The transcript intentionally mirrors the same `PersistedAgentEvent`
//! envelope that the audit oracle and JSONL sink consume, so byte-stable
//! receipts can be diffed across runs.

use serde_json::{json, Value};

use crate::agent_events::{AgentEvent, PersistedAgentEvent, ToolCallStatus};

use super::state::{PlaygroundPullRequest, PlaygroundState};

pub struct TranscriptOptions {
    pub session_id: String,
}

impl Default for TranscriptOptions {
    fn default() -> Self {
        TranscriptOptions {
            session_id: "merge-captain-playground".to_string(),
        }
    }
}

pub fn synthesize_sweep(
    state: &PlaygroundState,
    options: &TranscriptOptions,
) -> Vec<PersistedAgentEvent> {
    let mut events: Vec<PersistedAgentEvent> = Vec::new();
    let mut now = state.now_ms;
    let mut idx: u64 = 0;
    let bump = |delta: i64, now: &mut i64, idx: &mut u64| {
        *now = now.saturating_add(delta);
        let value = *idx;
        *idx += 1;
        value
    };

    let session_id = options.session_id.clone();
    let envelope = |index: u64, at: i64, event: AgentEvent| PersistedAgentEvent {
        index,
        emitted_at_ms: at,
        frame_depth: Some(0),
        event,
    };

    let i = bump(0, &mut now, &mut idx);
    events.push(envelope(
        i,
        now,
        AgentEvent::IterationStart {
            session_id: session_id.clone(),
            iteration: 1,
            provider: String::new(),
            model: String::new(),
        },
    ));

    let i = bump(10, &mut now, &mut idx);
    events.push(envelope(
        i,
        now,
        AgentEvent::AgentThoughtChunk {
            session_id: session_id.clone(),
            content: format!(
                "Sweep playground scenario={} ({} repos, {} PRs)",
                state.scenario,
                state.repos.len(),
                state.pull_requests.len()
            ),
        },
    ));

    let mut prs: Vec<&PlaygroundPullRequest> = state
        .pull_requests
        .values()
        .filter(|pr| pr.state == "open")
        .collect();
    prs.sort_by_key(|pr| (pr.repo.clone(), pr.number));

    let mut tool_call_counter = 0u64;
    for pr in prs {
        tool_call_counter += 1;
        let intake_id = format!("call_{tool_call_counter}");
        let i = bump(20, &mut now, &mut idx);
        events.push(envelope(
            i,
            now,
            AgentEvent::ToolCall {
                session_id: session_id.clone(),
                tool_call_id: intake_id.clone(),
                tool_name: "gh_pull_request_get".to_string(),
                kind: None,
                status: ToolCallStatus::Pending,
                raw_input: json!({"repo": format!("{}/{}", state.owner, pr.repo), "pr_number": pr.number}),
                parsing: None,
                audit: None,
            },
        ));
        let i = bump(40, &mut now, &mut idx);
        events.push(envelope(
            i,
            now,
            AgentEvent::ToolCallUpdate {
                session_id: session_id.clone(),
                tool_call_id: intake_id,
                tool_name: "gh_pull_request_get".to_string(),
                status: ToolCallStatus::Completed,
                raw_output: Some(pr_summary(pr)),
                error: None,
                duration_ms: Some(40),
                execution_duration_ms: Some(40),
                error_category: None,
                executor: None,
                parsing: None,
                raw_input: None,
                raw_input_partial: None,
                audit: None,
            },
        ));

        let i = bump(20, &mut now, &mut idx);
        events.push(envelope(
            i,
            now,
            AgentEvent::Plan {
                session_id: session_id.clone(),
                plan: pr_plan(pr),
            },
        ));

        tool_call_counter += 1;
        let checks_id = format!("call_{tool_call_counter}");
        let i = bump(20, &mut now, &mut idx);
        events.push(envelope(
            i,
            now,
            AgentEvent::ToolCall {
                session_id: session_id.clone(),
                tool_call_id: checks_id.clone(),
                tool_name: "gh_pr_checks_list".to_string(),
                kind: None,
                status: ToolCallStatus::Pending,
                raw_input: json!({"repo": format!("{}/{}", state.owner, pr.repo), "pr_number": pr.number}),
                parsing: None,
                audit: None,
            },
        ));
        let i = bump(40, &mut now, &mut idx);
        events.push(envelope(
            i,
            now,
            AgentEvent::ToolCallUpdate {
                session_id: session_id.clone(),
                tool_call_id: checks_id,
                tool_name: "gh_pr_checks_list".to_string(),
                status: ToolCallStatus::Completed,
                raw_output: Some(checks_summary(pr)),
                error: None,
                duration_ms: Some(40),
                execution_duration_ms: Some(40),
                error_category: None,
                executor: None,
                parsing: None,
                raw_input: None,
                raw_input_partial: None,
                audit: None,
            },
        ));

        let i = bump(10, &mut now, &mut idx);
        events.push(envelope(
            i,
            now,
            AgentEvent::Plan {
                session_id: session_id.clone(),
                plan: risk_plan(pr),
            },
        ));
    }

    let i = bump(50, &mut now, &mut idx);
    events.push(envelope(
        i,
        now,
        AgentEvent::AgentThoughtChunk {
            session_id: session_id.clone(),
            content: format!(
                "Sweep complete: {} PR(s) inspected, {} require follow-up",
                state
                    .pull_requests
                    .values()
                    .filter(|p| p.state == "open")
                    .count(),
                state
                    .pull_requests
                    .values()
                    .filter(|p| p.state == "open" && needs_followup(p))
                    .count()
            ),
        },
    ));

    let _ = bump(0, &mut now, &mut idx);
    events
}

fn pr_summary(pr: &PlaygroundPullRequest) -> Value {
    let failing: Vec<String> = pr
        .checks
        .iter()
        .filter(|c| {
            c.conclusion
                .as_deref()
                .map(|c| matches!(c, "failure" | "timed_out" | "cancelled"))
                .unwrap_or(false)
        })
        .map(|c| c.name.clone())
        .collect();
    json!({
        "repo": pr.repo,
        "pr_number": pr.number,
        "title": pr.title,
        "state": pr.state,
        "head_branch": pr.head_branch,
        "base_branch": pr.base_branch,
        "mergeable": pr.mergeable,
        "mergeable_state": pr.mergeable_state,
        "failing_checks": failing,
        "stale_threads": Vec::<String>::new(),
        "merge_conflicts": pr.mergeable_state == "dirty",
        "merge_queue_status": pr.merge_queue_status,
    })
}

fn pr_plan(pr: &PlaygroundPullRequest) -> Value {
    json!({
        "step": "intake",
        "repo": pr.repo,
        "pr_number": pr.number,
        "head_branch": pr.head_branch,
        "approval_required": false,
    })
}

fn risk_plan(pr: &PlaygroundPullRequest) -> Value {
    let risk = if needs_followup(pr) { "high" } else { "low" };
    json!({
        "step": "decide_risk",
        "repo": pr.repo,
        "pr_number": pr.number,
        "review_risk": risk,
        "approval_required": false,
    })
}

fn checks_summary(pr: &PlaygroundPullRequest) -> Value {
    let runs: Vec<Value> = pr
        .checks
        .iter()
        .map(|c| {
            json!({
                "name": c.name,
                "status": c.status,
                "conclusion": c.conclusion,
            })
        })
        .collect();
    json!({"check_runs": runs})
}

fn needs_followup(pr: &PlaygroundPullRequest) -> bool {
    pr.mergeable_state == "behind"
        || pr.mergeable_state == "dirty"
        || pr.mergeable_state == "blocked"
        || pr
            .checks
            .iter()
            .any(|c| matches!(c.conclusion.as_deref(), Some("failure" | "timed_out")))
}
