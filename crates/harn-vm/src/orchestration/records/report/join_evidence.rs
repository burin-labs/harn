//! Canonical delegated-child join evidence for run reports.

use std::collections::{BTreeMap, BTreeSet};

use crate::agent_events::AgentEvent;
use crate::session_timeline::SessionTimelineSnapshot;

use super::{check, RunReportCheck, RunReportDelegation};

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct DelegationKey {
    parent_agent_id: String,
    child_agent_id: String,
    worker_id: String,
}

impl DelegationKey {
    fn from_delegation(delegation: &RunReportDelegation) -> Self {
        Self {
            parent_agent_id: delegation.parent_agent_id.clone(),
            child_agent_id: delegation.child_agent_id.clone(),
            worker_id: delegation.worker_id.clone(),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(super) struct JoinEvidenceProjection {
    pub(super) complete: bool,
    pub(super) max_terminal_to_collection_ms: Option<u64>,
    pub(super) checks: Vec<RunReportCheck>,
    joined: BTreeSet<DelegationKey>,
}

impl JoinEvidenceProjection {
    pub(super) fn joined(&self, delegation: &RunReportDelegation) -> bool {
        self.joined
            .contains(&DelegationKey::from_delegation(delegation))
    }
}

pub(super) fn project_join_evidence(
    delegations: &[RunReportDelegation],
    timelines: &[SessionTimelineSnapshot],
    event_source_available: bool,
) -> JoinEvidenceProjection {
    let expected: BTreeMap<_, _> = delegations
        .iter()
        .map(|delegation| (DelegationKey::from_delegation(delegation), delegation))
        .collect();
    let run_sessions = timelines
        .iter()
        .filter_map(|timeline| {
            Some((
                timeline.query.run_id.as_deref()?,
                timeline.query.session_id.as_deref()?,
            ))
        })
        .collect::<BTreeMap<_, _>>();
    let mut projection = JoinEvidenceProjection {
        complete: event_source_available
            && delegations.iter().all(|delegation| {
                let Some(parent_run_id) = delegation.parent_agent_id.strip_prefix("run:") else {
                    return false;
                };
                let Some(child_run_id) = delegation.child_agent_id.strip_prefix("run:") else {
                    return false;
                };
                run_sessions.contains_key(child_run_id)
                    && timelines.iter().any(|timeline| {
                        timeline.query.run_id.as_deref() == Some(parent_run_id)
                            && timeline.query.session_id.is_some()
                            && !timeline.coverage.truncated
                    })
            }),
        ..JoinEvidenceProjection::default()
    };
    let mut receipt_lags = BTreeMap::<DelegationKey, Option<u64>>::new();

    for timeline in timelines {
        for node in &timeline.nodes {
            if node.category != "agent_event" {
                continue;
            }
            let event_value = node.attributes.get("event").unwrap_or(&node.attributes);
            if event_value.get("type").and_then(serde_json::Value::as_str) != Some("subagent_join")
            {
                continue;
            }
            let event = match serde_json::from_value::<AgentEvent>(event_value.clone()) {
                Ok(AgentEvent::SubagentJoin {
                    session_id,
                    lineage,
                    worker_id,
                    completed_at_ms,
                    joined_at_ms,
                }) => {
                    if session_id != lineage.parent.session_id
                        || timeline.query.session_id.as_deref()
                            != Some(lineage.parent.session_id.as_str())
                    {
                        projection.complete = false;
                        projection.checks.push(check(
                            "subagent_join_lineage_mismatch",
                            "error",
                            &format!("run:{}", lineage.child.run_id),
                            "join receipt session/run lineage disagrees with its event timeline"
                                .to_string(),
                        ));
                        continue;
                    }
                    if timeline.query.run_id.as_deref() != Some(lineage.parent.run_id.as_str()) {
                        // Agent-event topics are session-scoped, and one session
                        // may own several runs. The same valid receipt therefore
                        // appears in each run query for that session; only its
                        // named parent run may consume it.
                        continue;
                    }
                    if run_sessions.get(lineage.child.run_id.as_str()).copied()
                        != Some(lineage.child.session_id.as_str())
                    {
                        projection.complete = false;
                        projection.checks.push(check(
                            "subagent_join_lineage_mismatch",
                            "error",
                            &format!("run:{}", lineage.child.run_id),
                            "join receipt child session disagrees with the child run timeline"
                                .to_string(),
                        ));
                        continue;
                    }
                    (
                        DelegationKey {
                            parent_agent_id: format!("run:{}", lineage.parent.run_id),
                            child_agent_id: format!("run:{}", lineage.child.run_id),
                            worker_id,
                        },
                        completed_at_ms,
                        joined_at_ms,
                    )
                }
                Ok(_) => unreachable!("subagent_join type must decode to SubagentJoin"),
                Err(error) => {
                    projection.complete = false;
                    projection.checks.push(check(
                        "subagent_join_malformed",
                        "error",
                        &timeline
                            .query
                            .run_id
                            .as_deref()
                            .map(|run_id| format!("run:{run_id}"))
                            .unwrap_or_else(|| "run:unknown".to_string()),
                        format!("canonical join event could not be decoded: {error}"),
                    ));
                    continue;
                }
            };

            let (key, completed_at_ms, joined_at_ms) = event;
            if !expected.contains_key(&key) {
                projection.checks.push(check(
                    "subagent_join_without_delegation",
                    "error",
                    &key.child_agent_id,
                    format!(
                        "worker {} has a canonical join receipt but no matching parent/child delegation",
                        key.worker_id
                    ),
                ));
                continue;
            }
            projection.joined.insert(key.clone());
            let lag = joined_at_ms
                .checked_sub(completed_at_ms)
                .and_then(|lag| u64::try_from(lag).ok());
            if lag.is_none() {
                projection.checks.push(check(
                    "subagent_join_time_invalid",
                    "error",
                    &key.child_agent_id,
                    "join receipt precedes the child's terminal timestamp".to_string(),
                ));
            }
            if receipt_lags.insert(key.clone(), lag).is_some() {
                receipt_lags.insert(key.clone(), None);
                projection.checks.push(check(
                    "subagent_join_duplicate",
                    "error",
                    &key.child_agent_id,
                    format!(
                        "worker {} has more than one canonical join receipt",
                        key.worker_id
                    ),
                ));
            }
        }
    }

    if projection.complete {
        for (key, delegation) in expected {
            if crate::agent_events::WorkerEvent::status_is_terminal(&delegation.status)
                && !projection.joined.contains(&key)
            {
                projection.checks.push(check(
                    "subagent_join_missing",
                    "warning",
                    &key.child_agent_id,
                    format!(
                        "terminal worker {} has no canonical parent collection receipt",
                        key.worker_id
                    ),
                ));
            }
        }
        projection.max_terminal_to_collection_ms = receipt_lags.into_values().flatten().max();
    }
    projection
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_events::{AgentRunRef, DelegatedRunLineage};
    use crate::session_timeline::{
        SessionTimelineCoverage, SessionTimelineCursor, SessionTimelineNode, SessionTimelineQuery,
    };

    fn timeline(
        run_id: &str,
        session_id: &str,
        coverage: SessionTimelineCoverage,
        nodes: Vec<SessionTimelineNode>,
    ) -> SessionTimelineSnapshot {
        SessionTimelineSnapshot {
            schema_version: crate::session_timeline::SESSION_TIMELINE_SCHEMA_VERSION,
            query: SessionTimelineQuery {
                session_id: Some(session_id.to_string()),
                run_id: Some(run_id.to_string()),
                ..SessionTimelineQuery::default()
            },
            cursor: SessionTimelineCursor::default(),
            coverage,
            nodes,
        }
    }

    #[test]
    fn truncated_parent_timeline_keeps_join_results_unknown() {
        let delegation = RunReportDelegation {
            parent_agent_id: "run:parent".to_string(),
            child_agent_id: "run:child".to_string(),
            worker_id: "worker-1".to_string(),
            status: "completed".to_string(),
            ..RunReportDelegation::default()
        };
        let timeline = timeline(
            "parent",
            "parent-session",
            SessionTimelineCoverage {
                returned: 1024,
                available: None,
                truncated: true,
            },
            Vec::new(),
        );

        let evidence = project_join_evidence(&[delegation], &[timeline], true);

        assert!(!evidence.complete);
        assert!(evidence.joined.is_empty());
        assert_eq!(evidence.max_terminal_to_collection_ms, None);
        assert!(!evidence
            .checks
            .iter()
            .any(|check| check.code == "subagent_join_missing"));
    }

    #[test]
    fn shared_session_receipt_is_consumed_only_by_its_parent_run() {
        let delegation = RunReportDelegation {
            parent_agent_id: "run:parent".to_string(),
            child_agent_id: "run:child".to_string(),
            worker_id: "worker-1".to_string(),
            status: "completed".to_string(),
            ..RunReportDelegation::default()
        };
        let event = AgentEvent::SubagentJoin {
            session_id: "shared-session".to_string(),
            lineage: DelegatedRunLineage {
                parent: AgentRunRef {
                    session_id: "shared-session".to_string(),
                    run_id: "parent".to_string(),
                },
                child: AgentRunRef {
                    session_id: "child-session".to_string(),
                    run_id: "child".to_string(),
                },
            },
            worker_id: "worker-1".to_string(),
            completed_at_ms: 100,
            joined_at_ms: 125,
        };
        let node = SessionTimelineNode {
            id: "join-1".to_string(),
            parent_id: None,
            children: Vec::new(),
            category: "agent_event".to_string(),
            kind: "subagent_join".to_string(),
            name: "subagent_join".to_string(),
            status: "observed".to_string(),
            trace_id: None,
            span_id: None,
            occurred_at_ms: Some(125),
            start_ms: None,
            duration_ms: None,
            attributes: serde_json::json!({"event": event}),
            references: Vec::new(),
            links: Vec::new(),
            order: 1,
        };
        let complete = SessionTimelineCoverage {
            returned: 1,
            available: Some(1),
            truncated: false,
        };
        let timelines = [
            timeline(
                "parent",
                "shared-session",
                complete.clone(),
                vec![node.clone()],
            ),
            timeline("other-run", "shared-session", complete, vec![node]),
            timeline(
                "child",
                "child-session",
                SessionTimelineCoverage::default(),
                Vec::new(),
            ),
        ];

        let evidence = project_join_evidence(&[delegation.clone()], &timelines, true);

        assert!(evidence.complete);
        assert!(evidence.joined(&delegation));
        assert_eq!(evidence.max_terminal_to_collection_ms, Some(25));
        assert!(!evidence
            .checks
            .iter()
            .any(|check| check.code == "subagent_join_lineage_mismatch"));
    }
}
