use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};

use super::*;
pub use crate::llm::helpers::{GoalPinCriterion, GoalPinSpec};

pub const MAX_SESSION_GOAL_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionGoalMetadata {
    pub acceptance_note: String,
    pub criterion_sources: BTreeMap<String, String>,
    pub verification: Option<SessionGoalVerification>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    pub source: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionGoalVerification {
    pub command: String,
    pub source: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionGoalTurn {
    #[default]
    Idle,
    Open,
}

/// Orchestration state shares the session's journal, admission and lifecycle.
/// Metadata is retained for callers, never implicitly inserted into a prompt.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionGoalState {
    pub spec: Option<GoalPinSpec>,
    pub metadata: SessionGoalMetadata,
    pub generation: u64,
    pub created_at: String,
    pub updated_at: String,
    pub turn: SessionGoalTurn,
}

impl SessionGoalState {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self
            .metadata
            .target
            .as_ref()
            .is_some_and(|target| target.trim().is_empty())
        {
            return Err("session goal target must be non-blank".into());
        }
        let mut ids = HashSet::new();
        if let Some(spec) = &self.spec {
            if spec.objective.trim().is_empty() {
                return Err("session goal objective must be non-blank".into());
            }
            for criterion in spec.success_criteria.iter().chain(&spec.retired_criteria) {
                if criterion.id.trim().is_empty() || criterion.description.trim().is_empty() {
                    return Err(
                        "session goal criteria require non-blank ids and descriptions".into(),
                    );
                }
                ids.insert(criterion.id.as_str());
            }
            let mut active_ids = HashSet::new();
            for criterion in &spec.success_criteria {
                if !active_ids.insert(criterion.id.as_str()) {
                    return Err(format!(
                        "duplicate active session goal criterion '{}'",
                        criterion.id
                    ));
                }
            }
        }
        if let Some(id) = self
            .metadata
            .criterion_sources
            .keys()
            .find(|id| !ids.contains(id.as_str()))
        {
            return Err(format!(
                "session goal source references unknown criterion '{id}'"
            ));
        }
        let size = serde_json::to_vec(self)
            .map_err(|error| error.to_string())?
            .len();
        if size > MAX_SESSION_GOAL_BYTES {
            return Err(format!(
                "session goal is {size} bytes; max is {MAX_SESSION_GOAL_BYTES}"
            ));
        }
        Ok(())
    }

    fn advance(&mut self) -> Result<(), String> {
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or_else(|| "session goal generation exhausted".to_string())?;
        self.updated_at = crate::clock::system_now_rfc3339();
        Ok(())
    }

    fn clear(&mut self) -> Result<bool, String> {
        let existed = self.spec.take().is_some();
        self.metadata = SessionGoalMetadata::default();
        self.created_at.clear();
        self.advance()?;
        Ok(existed)
    }
}

fn admitted(state: &SessionState, action: &str) -> Result<(), String> {
    if state.admission_role != SessionAdmissionRole::Admitted {
        return Err(format!(
            "session '{}' is a parent reference; {action} requires admission",
            state.id
        ));
    }
    state.ensure_run_accepts_mutation(action)
}

fn commit(state: &mut SessionState, next: SessionGoalState, action: &str) -> Result<(), String> {
    admitted(state, action)?;
    next.validate()?;
    let event = goal_event(&state.id, &next);
    crate::agent_session_journal::enqueue_audit_event(
        &mut state.transcript_journal,
        crate::llm::helpers::vm_value_to_json(&event),
    );
    state.goal = next;
    state.touch();
    Ok(())
}

fn goal_event(id: &str, goal: &SessionGoalState) -> VmValue {
    crate::llm::helpers::transcript_event(
        "agent_goal",
        "system",
        "internal",
        "Session goal state changed",
        Some(serde_json::json!({"schema":"harn.session_goal.v1", "session_id":id, "state":goal})),
    )
}

/// A validated reset, computed without touching the session so a caller can
/// persist it before applying it.
#[derive(Clone)]
pub(super) struct ResetPlan {
    pub(super) goal: SessionGoalState,
    event: serde_json::Value,
}

pub(super) fn plan_reset(state: &SessionState) -> Result<ResetPlan, String> {
    admitted(state, "reset_session_goal")?;
    let mut goal = state.goal.clone();
    goal.clear()?;
    goal.turn = SessionGoalTurn::Idle;
    goal.validate()?;
    let event = crate::llm::helpers::vm_value_to_json(&goal_event(&state.id, &goal));
    Ok(ResetPlan { goal, event })
}

impl ResetPlan {
    /// Queue the canonical events this reset appends, oldest first.
    pub(super) fn enqueue(&self, journal: &mut Option<crate::agent_session_journal::JournalState>) {
        crate::agent_session_journal::enqueue_messages_replaced(journal, vec![], None, vec![]);
        crate::agent_session_journal::enqueue_audit_event(journal, self.event.clone());
    }
}

fn mutate<T>(
    id: &str,
    action: &str,
    change: impl FnOnce(&mut SessionGoalState) -> Result<T, String>,
) -> Result<T, String> {
    SESSIONS.with(|sessions| {
        let mut sessions = sessions.borrow_mut();
        let state = sessions
            .get_mut(id)
            .ok_or_else(|| format!("unknown session '{id}'"))?;
        admitted(state, action)?;
        let mut next = state.goal.clone();
        let result = change(&mut next)?;
        commit(state, next, action)?;
        Ok(result)
    })
}

pub fn session_goal(id: &str) -> Result<SessionGoalState, String> {
    SESSIONS.with(|sessions| {
        let sessions = sessions.borrow();
        let state = sessions
            .get(id)
            .ok_or_else(|| format!("unknown session '{id}'"))?;
        if state.admission_role != SessionAdmissionRole::Admitted {
            return Err(format!("session '{id}' requires admission"));
        }
        Ok(state.goal.clone())
    })
}

pub fn set_session_goal(
    id: &str,
    spec: GoalPinSpec,
    metadata: SessionGoalMetadata,
) -> Result<SessionGoalState, String> {
    mutate(id, "set_session_goal", |goal| {
        goal.spec = Some(spec);
        goal.metadata = metadata;
        goal.advance()?;
        if goal.created_at.is_empty() {
            goal.created_at.clone_from(&goal.updated_at);
        }
        Ok(goal.clone())
    })
}

pub fn clear_session_goal(id: &str) -> Result<bool, String> {
    mutate(id, "clear_session_goal", SessionGoalState::clear)
}

pub fn begin_session_goal_turn(id: &str) -> Result<bool, String> {
    mutate(id, "begin_session_goal_turn", |goal| {
        let retired = if goal.turn == SessionGoalTurn::Open {
            goal.clear()?
        } else {
            false
        };
        goal.turn = SessionGoalTurn::Open;
        Ok(retired)
    })
}

pub fn end_session_goal_turn(id: &str, cancelled: bool) -> Result<(), String> {
    mutate(id, "end_session_goal_turn", |goal| {
        if cancelled {
            goal.clear()?;
        }
        goal.turn = SessionGoalTurn::Idle;
        Ok(())
    })
}

pub(crate) fn has_pending_fork_goal(id: &str) -> bool {
    SESSIONS.with(|sessions| {
        sessions
            .borrow()
            .get(id)
            .is_some_and(|state| state.goal_pending_fork)
    })
}

pub(crate) fn persist_pending_fork_goal(id: &str) -> Result<(), String> {
    SESSIONS.with(|sessions| {
        let mut sessions = sessions.borrow_mut();
        let state = sessions
            .get_mut(id)
            .ok_or_else(|| format!("unknown session '{id}'"))?;
        if !state.goal_pending_fork {
            return Ok(());
        }
        if !state
            .transcript_journal
            .as_ref()
            .is_some_and(|journal| journal.execution_id().is_some() && journal.task_id().is_some())
        {
            return Err("fork goal requires a claimed canonical run".into());
        }
        let next = state.goal.clone();
        commit(state, next, "persist_fork_goal")?;
        state.goal_pending_fork = false;
        Ok(())
    })
}

pub(crate) fn restore_session_goal(id: &str, mut goal: SessionGoalState) -> Result<(), String> {
    goal.validate()?;
    SESSIONS.with(|sessions| {
        let mut sessions = sessions.borrow_mut();
        let state = sessions
            .get_mut(id)
            .ok_or_else(|| format!("unknown session '{id}'"))?;
        admitted(state, "restore_session_goal")?;
        if state.goal_pending_fork {
            goal.turn = SessionGoalTurn::Idle;
        }
        state.goal = goal;
        state.goal_pending_fork = false;
        Ok(())
    })
}

/// Terminal ownership closes the marker before the journal seals. A caller
/// cannot append a late goal mutation behind the persisted terminal boundary.
pub(super) fn finish_before_terminal(
    state: &mut SessionState,
    cancelled: bool,
) -> Result<(), String> {
    if state.goal.turn == SessionGoalTurn::Idle && (!cancelled || state.goal.spec.is_none()) {
        return Ok(());
    }
    let mut next = state.goal.clone();
    if cancelled {
        next.clear()?;
    }
    next.turn = SessionGoalTurn::Idle;
    commit(state, next, "finish_session_goal")
}

#[cfg(test)]
mod tests {
    use super::*;
    use harn_session_store::{CanonicalSessionBoundary, SessionStore};

    fn spec() -> GoalPinSpec {
        GoalPinSpec {
            objective: "Complete the task".into(),
            success_criteria: vec![GoalPinCriterion {
                id: "check".into(),
                description: "Verification succeeds".into(),
            }],
            retired_criteria: vec![],
            constraints: vec!["Keep scope".into()],
            budget: serde_json::Map::new(),
        }
    }

    #[test]
    fn initial_begin_keeps_goal_and_dropped_open_turn_retires_it() {
        let id = open_or_create(None).unwrap();
        let saved = set_session_goal(&id, spec(), SessionGoalMetadata::default()).unwrap();
        assert!(!begin_session_goal_turn(&id).unwrap());
        assert_eq!(session_goal(&id).unwrap().spec, saved.spec);
        end_session_goal_turn(&id, false).unwrap();
        assert!(!begin_session_goal_turn(&id).unwrap());
        assert!(begin_session_goal_turn(&id).unwrap());
        let cleared = session_goal(&id).unwrap();
        assert!(cleared.spec.is_none());
        assert_eq!(cleared.generation, saved.generation + 1);
    }

    #[test]
    fn rejected_payload_does_not_mutate_or_advance_generation() {
        let id = open_or_create(None).unwrap();
        let saved = set_session_goal(&id, spec(), SessionGoalMetadata::default()).unwrap();
        let mut metadata = SessionGoalMetadata::default();
        metadata
            .criterion_sources
            .insert("missing".into(), "caller".into());
        assert!(set_session_goal(&id, spec(), metadata).is_err());
        let empty_target = SessionGoalMetadata {
            target: Some("  ".into()),
            ..Default::default()
        };
        assert!(set_session_goal(&id, spec(), empty_target).is_err());
        let mut oversized = spec();
        oversized.objective = "x".repeat(MAX_SESSION_GOAL_BYTES);
        assert!(set_session_goal(&id, oversized, SessionGoalMetadata::default()).is_err());
        assert_eq!(session_goal(&id).unwrap(), saved);
        assert!(set_session_goal("unknown", spec(), SessionGoalMetadata::default()).is_err());
    }

    #[test]
    fn retargeted_criteria_preserve_retired_identities_but_refuse_duplicate_active_ids() {
        let id = open_or_create(None).unwrap();
        let mut retargeted = spec();
        retargeted.retired_criteria = vec![
            GoalPinCriterion {
                id: "check".into(),
                description: "Earlier requirement".into(),
            },
            GoalPinCriterion {
                id: "check".into(),
                description: "Another retired requirement".into(),
            },
        ];
        let mut metadata = SessionGoalMetadata::default();
        metadata
            .criterion_sources
            .insert("check".into(), "opaque-source".into());
        let saved = set_session_goal(&id, retargeted.clone(), metadata.clone()).unwrap();
        assert_eq!(
            saved.spec.as_ref().unwrap().retired_criteria,
            retargeted.retired_criteria
        );
        retargeted
            .success_criteria
            .push(retargeted.success_criteria[0].clone());
        assert!(set_session_goal(&id, retargeted, metadata).is_err());
        assert_eq!(session_goal(&id).unwrap(), saved);
    }

    #[test]
    fn snapshot_carries_goal_without_duplicating_it_into_prompt_state() {
        let id = open_or_create(None).unwrap();
        let saved = set_session_goal(&id, spec(), SessionGoalMetadata::default()).unwrap();
        let snapshot = crate::llm::helpers::vm_value_to_json(&snapshot(&id).unwrap());
        assert_eq!(snapshot["goal"], serde_json::to_value(saved).unwrap());
        let transcript = crate::llm::helpers::vm_value_to_json(&transcript(&id).unwrap());
        assert!(transcript["metadata"].get("agent_goal").is_none());
        assert_eq!(transcript["events"], serde_json::json!([]));
        assert!(prompt_state_json(&id).messages.is_empty());
    }

    #[test]
    fn fork_has_isolated_goal_and_idle_turn_reset_clears_it() {
        let id = open_or_create(None).unwrap();
        let saved = set_session_goal(&id, spec(), SessionGoalMetadata::default()).unwrap();
        begin_session_goal_turn(&id).unwrap();
        let child = fork(&id, None).unwrap().unwrap();
        assert_eq!(session_goal(&child).unwrap().spec, saved.spec);
        assert_eq!(session_goal(&child).unwrap().turn, SessionGoalTurn::Idle);
        assert!(!begin_session_goal_turn(&child).unwrap());
        clear_session_goal(&child).unwrap();
        assert_eq!(session_goal(&id).unwrap().spec, saved.spec);
        assert!(reset_transcript_in_memory(&id));
        assert!(session_goal(&id).unwrap().spec.is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn canonical_restore_retains_goal_metadata_and_unreturned_marker() {
        let root = tempfile::tempdir().unwrap();
        let id = open_or_create(None).unwrap();
        let mut options = crate::value::DictMap::new();
        crate::value::VmDictExt::put_str(&mut options, "root", root.path().to_str().unwrap());
        let prepared = crate::agent_session_journal::prepare(
            &id,
            &options,
            "goal-run".into(),
            "goal-turn".into(),
        )
        .await
        .unwrap();
        install_journal(&id, prepared.state).unwrap();
        let metadata = SessionGoalMetadata {
            acceptance_note: "Exact operator note\nKeep this line.".into(),
            criterion_sources: BTreeMap::from([("check".into(), "opaque-origin".into())]),
            verification: Some(SessionGoalVerification {
                command: "printf '%s\\n' 'literal text'".into(),
                source: "caller-origin".into(),
            }),
            target: Some("src/calc.sh".into()),
            source: "operator".into(),
        };
        let mut input = spec();
        input
            .budget
            .insert("max_cost_usd".into(), serde_json::json!(2.5));
        input.retired_criteria.push(GoalPinCriterion {
            id: "old".into(),
            description: "Retired requirement".into(),
        });
        set_session_goal(&id, input, metadata).unwrap();
        begin_session_goal_turn(&id).unwrap();
        let before = session_goal(&id).unwrap();
        crate::agent_session_journal::flush(&id).await.unwrap();
        let store = journal_store(&id).unwrap();
        let events = store.read_all(&id).await.unwrap();
        let boundary = CanonicalSessionBoundary::acknowledged(events.last().unwrap());
        clear_journal(&id);
        assert!(close(&id));
        open_or_create(Some(id.clone())).unwrap();
        let child = fork_canonical(&store, root.path(), &id, Some(boundary), None)
            .await
            .unwrap()
            .unwrap();
        assert!(child.source_boundary.event_id.is_some());
        let child_id = child.session_id;
        assert_eq!(session_goal(&child_id).unwrap().turn, SessionGoalTurn::Idle);
        close(&child_id);
        let child_run = crate::agent_session_journal::prepare(
            &child_id,
            &options,
            "child-goal-run".into(),
            "child-goal-turn".into(),
        )
        .await
        .unwrap();
        open_or_create(Some(child_id.clone())).unwrap();
        child_run.transcript.restore_goal(&child_id).unwrap();
        install_journal(&child_id, child_run.state).unwrap();
        let forked_goal = session_goal(&child_id).unwrap();
        assert_eq!(forked_goal.spec, before.spec);
        assert_eq!(forked_goal.metadata, before.metadata);
        assert_eq!(forked_goal.turn, SessionGoalTurn::Idle);
        assert!(!begin_session_goal_turn(&child_id).unwrap());
        clear_journal(&child_id);
        close(&child_id);
        let prepared = crate::agent_session_journal::prepare(
            &id,
            &options,
            "restored-run".into(),
            "restored-turn".into(),
        )
        .await
        .unwrap();
        assert!(
            prepared.transcript.messages.is_empty(),
            "goal-only history is the cold-session control"
        );
        open_or_create(Some(id.clone())).unwrap();
        prepared.transcript.restore_goal(&id).unwrap();
        install_journal(&id, prepared.state).unwrap();
        assert_eq!(session_goal(&id).unwrap(), before);
        assert!(begin_session_goal_turn(&id).unwrap());
        assert_eq!(session_goal(&id).unwrap().generation, before.generation + 1);
        clear_journal(&id);
        close(&id);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancellation_clears_before_terminal_seals_late_mutations() {
        let root = tempfile::tempdir().unwrap();
        let id = open_or_create(None).unwrap();
        let mut options = crate::value::DictMap::new();
        crate::value::VmDictExt::put_str(&mut options, "root", root.path().to_str().unwrap());
        let prepared = crate::agent_session_journal::prepare(
            &id,
            &options,
            "cancel-goal-run".into(),
            "cancel-goal-turn".into(),
        )
        .await
        .unwrap();
        install_journal(&id, prepared.state).unwrap();
        let saved = set_session_goal(&id, spec(), SessionGoalMetadata::default()).unwrap();
        begin_session_goal_turn(&id).unwrap();
        let terminal = crate::llm::helpers::transcript_event(
            "agent_run_terminal",
            "assistant",
            "internal",
            "Cancelled",
            Some(serde_json::json!({"final_status":"cancelled"})),
        );
        append_terminal_event_once(&id, terminal.clone()).unwrap();
        let ended = session_goal(&id).unwrap();
        assert!(ended.spec.is_none());
        assert_eq!(ended.turn, SessionGoalTurn::Idle);
        assert_eq!(ended.generation, saved.generation + 1);
        assert!(set_session_goal(&id, spec(), SessionGoalMetadata::default()).is_err());
        assert!(begin_session_goal_turn(&id).is_err());
        append_terminal_event_once(&id, terminal).unwrap();
        assert_eq!(
            session_goal(&id).unwrap(),
            ended,
            "terminal retry must not advance twice"
        );
        crate::agent_session_journal::flush(&id).await.unwrap();
        clear_journal(&id);
        close(&id);
    }
}
