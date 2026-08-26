//! Deterministic prompt-turn recaps projected from canonical session events.
//!
//! A recap is a read-only view over [`SessionStore`]. It never writes a derived
//! record and never calls a provider. The store's signed event chain remains the
//! source of truth; this module groups those facts by the existing `run_id`,
//! `turn_id`, and `loop_checkpoint` iteration boundaries.

use std::collections::HashMap;

use harn_session_store::{ReadRange, SessionEventKind, SessionStore, StoredEvent, MAX_READ_BATCH};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::agent_sessions::event_facts as facts;
use crate::redact::{current_policy, RedactionPolicy};

pub const SESSION_RECAP_SCHEMA_VERSION: u32 = 1;
pub const DEFAULT_RECAP_SOURCE_LIMIT: usize = 4_096;
pub const MAX_RECAP_SOURCE_LIMIT: usize = 32_768;

/// One bounded read over a durable agent session.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct SessionRecapQuery {
    #[serde(alias = "session_id")]
    pub session_id: String,
    #[serde(alias = "run_id")]
    pub run_id: Option<String>,
    #[serde(alias = "turn_id")]
    pub turn_id: Option<String>,
    #[serde(alias = "from_event_id")]
    pub from_event_id: Option<u64>,
    pub limit: Option<usize>,
}

impl SessionRecapQuery {
    pub fn for_session(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            ..Self::default()
        }
    }

    fn limit(&self) -> usize {
        self.limit
            .unwrap_or(DEFAULT_RECAP_SOURCE_LIMIT)
            .clamp(1, MAX_RECAP_SOURCE_LIMIT)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionRecapSnapshot {
    pub schema_version: u32,
    pub session_id: String,
    pub query: SessionRecapQuery,
    pub cursor: SessionRecapCursor,
    pub coverage: SessionRecapCoverage,
    pub source: SessionRecapSource,
    pub content_hash: String,
    pub projection_hash: String,
    pub turns: Vec<PromptTurnRecap>,
}

/// Why a terminal result could not carry a deterministic recap snapshot.
///
/// The reason remains explicit so a projection failure cannot masquerade as
/// either an empty session or a successful result with no recap facts.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionRecapUnavailableReason {
    JournalUnavailable,
    SessionMissing,
    ProjectionFailed,
    AdmissionTerminal,
}

/// Terminal `AgentResult` projection of recap availability.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SessionRecapAvailability {
    Available {
        snapshot: SessionRecapSnapshot,
    },
    Unavailable {
        reason: SessionRecapUnavailableReason,
    },
}

impl SessionRecapAvailability {
    pub fn available(snapshot: SessionRecapSnapshot) -> Self {
        Self::Available { snapshot }
    }

    pub fn unavailable(reason: SessionRecapUnavailableReason) -> Self {
        Self::Unavailable { reason }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct SessionRecapCursor {
    pub last_event_id: Option<u64>,
    pub next_event_id: Option<u64>,
}

/// Visible instrument shape for a bounded projection.
///
/// `scanned` counts canonical source rows read. `matched` counts rows that
/// contributed a recap fact. `pending` counts rows beyond this bounded read.
/// `unassigned` counts recognized facts lacking the identities or iteration
/// boundary needed to place them without guessing.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct SessionRecapCoverage {
    pub scanned: usize,
    pub matched: usize,
    pub pending: usize,
    pub unassigned: usize,
    pub truncated: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct SessionRecapSource {
    pub first_event_id: Option<u64>,
    pub last_event_id: Option<u64>,
    pub events: Vec<SessionRecapSourceEvent>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionRecapSourceEvent {
    pub event_id: u64,
    pub record_hash: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RecapCompletionState {
    Open,
    Complete,
    Incomplete,
    Unassigned,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PromptTurnRecap {
    pub turn_id: String,
    pub run_id: String,
    pub state: RecapCompletionState,
    pub prompts: Vec<RecapTextFact>,
    pub iterations: Vec<IterationRecap>,
    pub terminal: Option<RecapTerminalFact>,
    pub source_event_ids: Vec<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct IterationRecap {
    /// `None` is an explicit unassigned segment for a recognized fact that
    /// arrived outside durable iteration checkpoints. It is not a new persisted
    /// identity and never aliases a real iteration number.
    pub iteration: Option<i64>,
    pub state: RecapCompletionState,
    pub assistant_text: Vec<RecapTextFact>,
    pub tools: Vec<RecapToolExchange>,
    pub plans: Vec<RecapPlanFact>,
    pub progress: Vec<RecapProgressFact>,
    pub source_event_ids: Vec<u64>,
    #[serde(skip)]
    started: bool,
    #[serde(skip)]
    ended: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RecapTextFact {
    pub text: String,
    pub source_event_id: u64,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RecapToolState {
    Open,
    Completed,
    Failed,
    Incomplete,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RecapToolExchange {
    pub tool_call_id: String,
    pub tool_name: Option<String>,
    pub state: RecapToolState,
    pub call_observed: bool,
    pub result_observed: bool,
    pub input: Option<serde_json::Value>,
    pub output: Option<serde_json::Value>,
    pub verification: Option<RecapVerificationFact>,
    pub source_event_ids: Vec<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RecapVerificationFact {
    pub schema: String,
    pub status: String,
    pub verified_paths: Vec<String>,
    pub source_event_id: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RecapPlanFact {
    pub document_id: String,
    pub revision_id: String,
    pub title: String,
    pub summary: String,
    pub steps: Vec<RecapPlanStep>,
    pub event: Option<RecapPlanEventFact>,
    pub source_event_id: u64,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RecapPlanStepStatus {
    Pending,
    InProgress,
    Completed,
    Blocked,
    Cancelled,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RecapPlanStep {
    pub id: String,
    pub content: String,
    pub status: RecapPlanStepStatus,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RecapPlanEventKind {
    Created,
    Updated,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RecapPlanEventFact {
    pub kind: RecapPlanEventKind,
    pub event_id: String,
    pub input_revision_id: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RecapProgressStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RecapProgressPriority {
    High,
    Medium,
    Low,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RecapProgressEntry {
    pub content: String,
    pub status: RecapProgressStatus,
    pub priority: Option<RecapProgressPriority>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RecapProgressFact {
    pub message: Option<String>,
    pub entries: Vec<RecapProgressEntry>,
    pub replace: bool,
    pub source_event_id: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RecapTerminalFact {
    pub state: RecapCompletionState,
    pub final_status: Option<String>,
    pub stop_reason: Option<String>,
    pub kind: Option<String>,
    pub owner: Option<String>,
    pub reason: Option<String>,
    pub source_event_id: u64,
}

#[derive(Debug)]
pub struct SessionRecapError(String);

impl std::fmt::Display for SessionRecapError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for SessionRecapError {}

/// Project one canonical session through the deterministic recap interface.
///
/// `None` means the session does not exist. An existing session with no events
/// returns `Some` with zero scanned rows and zero turns.
pub async fn query_session_recap(
    store: &dyn SessionStore,
    query: SessionRecapQuery,
) -> Result<Option<SessionRecapSnapshot>, SessionRecapError> {
    if query.session_id.trim().is_empty() {
        return Err(SessionRecapError(
            "session recap requires a non-empty session_id".to_string(),
        ));
    }
    let meta = match store.describe(&query.session_id).await {
        Ok(meta) => meta,
        Err(harn_session_store::StoreError::NotFound(_)) => return Ok(None),
        Err(error) => return Err(SessionRecapError(error.to_string())),
    };
    let source_events = read_source_events(store, &query).await?;
    let last_event_id = source_events.last().map(|event| event.event_id);
    let next_event_id = last_event_id
        .filter(|last| meta.last_event_id.is_some_and(|tail| tail > *last))
        .map(|last| last.saturating_add(1));
    let pending = last_event_id
        .zip(meta.last_event_id)
        .map(|(last, tail)| tail.saturating_sub(last) as usize)
        .unwrap_or(0);
    let policy = current_policy();
    let mut projector = RecapProjector::new(&query, &policy);
    for event in &source_events {
        projector.absorb(event);
    }
    let (turns, matched, unassigned) = projector.finish();
    let source = SessionRecapSource {
        first_event_id: source_events.first().map(|event| event.event_id),
        last_event_id,
        events: source_events
            .iter()
            .map(|event| SessionRecapSourceEvent {
                event_id: event.event_id,
                record_hash: event.source_record_hash().to_string(),
            })
            .collect(),
    };
    let content_hash = sha256_canonical(&source);
    let cursor = SessionRecapCursor {
        last_event_id,
        next_event_id,
    };
    let coverage = SessionRecapCoverage {
        scanned: source_events.len(),
        matched,
        pending,
        unassigned,
        truncated: next_event_id.is_some(),
    };
    let projection_hash = sha256_canonical(&serde_json::json!({
        "schema_version": SESSION_RECAP_SCHEMA_VERSION,
        "session_id": &query.session_id,
        "query": &query,
        "cursor": &cursor,
        "coverage": &coverage,
        "content_hash": &content_hash,
        "turns": &turns,
    }));
    Ok(Some(SessionRecapSnapshot {
        schema_version: SESSION_RECAP_SCHEMA_VERSION,
        session_id: query.session_id.clone(),
        query,
        cursor,
        coverage,
        source,
        content_hash,
        projection_hash,
        turns,
    }))
}

async fn read_source_events(
    store: &dyn SessionStore,
    query: &SessionRecapQuery,
) -> Result<Vec<StoredEvent>, SessionRecapError> {
    let mut events = Vec::with_capacity(query.limit().min(MAX_READ_BATCH));
    let mut from = query.from_event_id;
    while events.len() < query.limit() {
        let remaining = query.limit() - events.len();
        let page = store
            .read(
                &query.session_id,
                ReadRange {
                    from_event_id: from,
                    limit: Some(remaining.min(MAX_READ_BATCH)),
                    ..ReadRange::default()
                },
            )
            .await
            .map_err(|error| SessionRecapError(error.to_string()))?;
        events.extend(page.events);
        let Some(next) = page.next_cursor else {
            break;
        };
        if events.len() >= query.limit() {
            break;
        }
        from = Some(next);
    }
    Ok(events)
}

struct TurnDraft {
    recap: PromptTurnRecap,
    iteration_positions: HashMap<Option<i64>, usize>,
    current_iteration: Option<i64>,
}

struct RecapProjector<'a> {
    query: &'a SessionRecapQuery,
    policy: &'a RedactionPolicy,
    turns: Vec<TurnDraft>,
    turn_positions: HashMap<(String, String), usize>,
    matched: usize,
    unassigned: usize,
}

impl<'a> RecapProjector<'a> {
    fn new(query: &'a SessionRecapQuery, policy: &'a RedactionPolicy) -> Self {
        Self {
            query,
            policy,
            turns: Vec::new(),
            turn_positions: HashMap::new(),
            matched: 0,
            unassigned: 0,
        }
    }

    fn absorb(&mut self, event: &StoredEvent) {
        if !is_recap_event(event) {
            return;
        }
        let run_id = event.headers.get("run_id").cloned();
        let turn_id = event.headers.get("turn_id").cloned();
        if run_id.is_none() || turn_id.is_none() {
            self.unassigned += 1;
            return;
        }
        if self
            .query
            .run_id
            .as_ref()
            .is_some_and(|expected| run_id.as_ref() != Some(expected))
            || self
                .query
                .turn_id
                .as_ref()
                .is_some_and(|expected| turn_id.as_ref() != Some(expected))
        {
            return;
        }
        let (Some(run_id), Some(turn_id)) = (run_id, turn_id) else {
            self.unassigned += 1;
            return;
        };
        let turn_index = self.turn_index(run_id, turn_id);
        self.turns[turn_index]
            .recap
            .source_event_ids
            .push(event.event_id);

        if is_checkpoint(event) {
            self.absorb_checkpoint(turn_index, event);
        } else if matches!(event.kind, SessionEventKind::Message) {
            self.absorb_message(turn_index, event);
        } else if matches!(event.kind, SessionEventKind::ToolCall) {
            self.absorb_tool_call(turn_index, event);
        } else if matches!(event.kind, SessionEventKind::ToolResult) {
            self.absorb_tool_result(turn_index, event);
        } else if matches!(event.kind, SessionEventKind::Plan) {
            self.absorb_plan(turn_index, event);
        } else if event.kind.discriminator() == "progress_reported" {
            self.absorb_progress(turn_index, event);
        } else if event.kind.discriminator() == "agent_run_terminal" {
            self.absorb_terminal(turn_index, event);
        }
    }

    fn turn_index(&mut self, run_id: String, turn_id: String) -> usize {
        let key = (run_id.clone(), turn_id.clone());
        if let Some(index) = self.turn_positions.get(&key) {
            return *index;
        }
        let index = self.turns.len();
        self.turns.push(TurnDraft {
            recap: PromptTurnRecap {
                turn_id,
                run_id,
                state: RecapCompletionState::Open,
                prompts: Vec::new(),
                iterations: Vec::new(),
                terminal: None,
                source_event_ids: Vec::new(),
            },
            iteration_positions: HashMap::new(),
            current_iteration: None,
        });
        self.turn_positions.insert(key, index);
        index
    }

    fn iteration_index(&mut self, turn_index: usize, iteration: Option<i64>) -> usize {
        if let Some(index) = self.turns[turn_index].iteration_positions.get(&iteration) {
            return *index;
        }
        let index = self.turns[turn_index].recap.iterations.len();
        self.turns[turn_index]
            .recap
            .iterations
            .push(IterationRecap {
                iteration,
                state: if iteration.is_some() {
                    RecapCompletionState::Open
                } else {
                    RecapCompletionState::Unassigned
                },
                assistant_text: Vec::new(),
                tools: Vec::new(),
                plans: Vec::new(),
                progress: Vec::new(),
                source_event_ids: Vec::new(),
                started: false,
                ended: false,
            });
        self.turns[turn_index]
            .iteration_positions
            .insert(iteration, index);
        index
    }

    fn current_segment(&mut self, turn_index: usize) -> usize {
        let iteration = self.turns[turn_index].current_iteration;
        if iteration.is_none() {
            self.unassigned += 1;
        }
        self.iteration_index(turn_index, iteration)
    }

    fn mark_segment_event(&mut self, turn_index: usize, segment_index: usize, event_id: u64) {
        self.turns[turn_index].recap.iterations[segment_index]
            .source_event_ids
            .push(event_id);
        self.matched += 1;
    }

    fn absorb_checkpoint(&mut self, turn_index: usize, event: &StoredEvent) {
        let kind = facts::string_at(&event.payload, facts::CHECKPOINT_KIND);
        let iteration = facts::i64_at(&event.payload, facts::ITERATION);
        let Some(iteration) = iteration else {
            self.unassigned += 1;
            return;
        };
        let segment_index = self.iteration_index(turn_index, Some(iteration));
        let segment = &mut self.turns[turn_index].recap.iterations[segment_index];
        segment.source_event_ids.push(event.event_id);
        match kind.as_deref() {
            Some("iteration_start") => {
                segment.started = true;
                self.turns[turn_index].current_iteration = Some(iteration);
                self.matched += 1;
            }
            Some("iteration_end") => {
                segment.ended = true;
                if self.turns[turn_index].current_iteration == Some(iteration) {
                    self.turns[turn_index].current_iteration = None;
                }
                self.matched += 1;
            }
            _ => self.unassigned += 1,
        }
    }

    fn absorb_message(&mut self, turn_index: usize, event: &StoredEvent) {
        let role = facts::semantic_string(&event.payload, &facts::ROLE);
        let visibility = facts::string_at(&event.payload, facts::VISIBILITY);
        let Some(text) = facts::semantic_string(&event.payload, &facts::TEXT) else {
            return;
        };
        let text = self.policy.redact_string(&text).into_owned();
        match role.as_deref() {
            Some("user") if visibility.as_deref().is_none_or(|value| value == "public") => {
                self.turns[turn_index].recap.prompts.push(RecapTextFact {
                    text,
                    source_event_id: event.event_id,
                });
                self.matched += 1;
            }
            Some("assistant") if visibility.as_deref() == Some("public") => {
                let segment_index = self.current_segment(turn_index);
                self.turns[turn_index].recap.iterations[segment_index]
                    .assistant_text
                    .push(RecapTextFact {
                        text,
                        source_event_id: event.event_id,
                    });
                self.mark_segment_event(turn_index, segment_index, event.event_id);
            }
            _ => {}
        }
    }

    fn absorb_tool_call(&mut self, turn_index: usize, event: &StoredEvent) {
        let segment_index = self.current_segment(turn_index);
        let Some(tool_call_id) = tool_call_id(event) else {
            self.unassigned += 1;
            return;
        };
        let input = facts::semantic_value(&event.payload, &facts::TOOL_INPUT_ANY)
            .map(|value| self.policy.redact_json(&value));
        let tool = RecapToolExchange {
            tool_call_id,
            tool_name: facts::semantic_string(&event.payload, &facts::TOOL_NAME_ANY),
            state: RecapToolState::Open,
            call_observed: true,
            result_observed: false,
            input,
            output: None,
            verification: None,
            source_event_ids: vec![event.event_id],
        };
        upsert_tool(
            &mut self.turns[turn_index].recap.iterations,
            segment_index,
            tool,
        );
        self.mark_segment_event(turn_index, segment_index, event.event_id);
    }

    fn absorb_tool_result(&mut self, turn_index: usize, event: &StoredEvent) {
        let Some(tool_call_id) = tool_call_id(event) else {
            self.unassigned += 1;
            return;
        };
        let existing = find_tool(&self.turns[turn_index].recap.iterations, &tool_call_id);
        let segment_index = existing
            .map(|(segment, _)| segment)
            .unwrap_or_else(|| self.current_segment(turn_index));
        let output = facts::semantic_value(&event.payload, &facts::TOOL_OUTPUT_ANY)
            .map(|value| self.policy.redact_json(&value));
        let failed = facts::bool_at_any(&event.payload, &facts::TOOL_IS_ERROR_ANY);
        let verification = verification_fact(event, self.policy);
        let result = RecapToolExchange {
            tool_call_id,
            tool_name: facts::semantic_string(&event.payload, &facts::TOOL_NAME_ANY),
            state: if failed {
                RecapToolState::Failed
            } else {
                RecapToolState::Completed
            },
            call_observed: false,
            result_observed: true,
            input: None,
            output,
            verification,
            source_event_ids: vec![event.event_id],
        };
        upsert_tool(
            &mut self.turns[turn_index].recap.iterations,
            segment_index,
            result,
        );
        self.mark_segment_event(turn_index, segment_index, event.event_id);
    }

    fn absorb_plan(&mut self, turn_index: usize, event: &StoredEvent) {
        let Some(document_value) = event.payload.pointer(facts::PLAN_DOCUMENT).cloned() else {
            self.unassigned += 1;
            return;
        };
        let Ok(document) = serde_json::from_value::<crate::llm::plan::PlanDocument>(document_value)
        else {
            self.unassigned += 1;
            return;
        };
        if document.validate().is_err() {
            self.unassigned += 1;
            return;
        }
        let plan_event = match event.payload.pointer(facts::PLAN_DOCUMENT_EVENT) {
            Some(value) => {
                let Ok(plan_event) =
                    serde_json::from_value::<crate::llm::plan::PlanDocumentEvent>(value.clone())
                else {
                    self.unassigned += 1;
                    return;
                };
                if plan_event.document() != &document || plan_event.document().validate().is_err() {
                    self.unassigned += 1;
                    return;
                }
                Some(match plan_event {
                    crate::llm::plan::PlanDocumentEvent::Created { event_id, .. } => {
                        RecapPlanEventFact {
                            kind: RecapPlanEventKind::Created,
                            event_id,
                            input_revision_id: None,
                        }
                    }
                    crate::llm::plan::PlanDocumentEvent::Updated {
                        event_id,
                        input_revision_id,
                        ..
                    } => RecapPlanEventFact {
                        kind: RecapPlanEventKind::Updated,
                        event_id,
                        input_revision_id: Some(input_revision_id),
                    },
                })
            }
            None => None,
        };
        let plan = &document.current_revision.plan;
        let document_id = document.document_id.clone();
        let revision_id = document.current_revision.revision_id.clone();
        let title = self.policy.redact_string(&plan.title).into_owned();
        let summary = self.policy.redact_string(&plan.summary).into_owned();
        let steps = plan
            .steps
            .iter()
            .map(|step| RecapPlanStep {
                id: step.id.clone(),
                content: self.policy.redact_string(&step.content).into_owned(),
                status: recap_plan_step_status(&step.status),
            })
            .collect();
        let segment_index = self.current_segment(turn_index);
        self.turns[turn_index].recap.iterations[segment_index]
            .plans
            .push(RecapPlanFact {
                document_id,
                revision_id,
                title,
                summary,
                steps,
                event: plan_event,
                source_event_id: event.event_id,
            });
        self.mark_segment_event(turn_index, segment_index, event.event_id);
    }

    fn absorb_progress(&mut self, turn_index: usize, event: &StoredEvent) {
        let metadata = event
            .payload
            .pointer(facts::TRANSCRIPT_METADATA)
            .unwrap_or(&serde_json::Value::Null);
        let message = metadata
            .get("message")
            .and_then(serde_json::Value::as_str)
            .map(|value| self.policy.redact_string(value).into_owned());
        let entries = metadata
            .get("entries")
            .and_then(serde_json::Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .map(|entry| progress_entry(entry, self.policy))
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose();
        let Ok(entries) = entries else {
            self.unassigned += 1;
            return;
        };
        let entries = entries.unwrap_or_default();
        if message.is_none() && entries.is_empty() {
            self.unassigned += 1;
            return;
        }
        let segment_index = self.current_segment(turn_index);
        self.turns[turn_index].recap.iterations[segment_index]
            .progress
            .push(RecapProgressFact {
                message,
                entries,
                replace: metadata
                    .get("replace")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(true),
                source_event_id: event.event_id,
            });
        self.mark_segment_event(turn_index, segment_index, event.event_id);
    }

    fn absorb_terminal(&mut self, turn_index: usize, event: &StoredEvent) {
        let kind = facts::string_at(&event.payload, facts::TERMINAL_KIND);
        let owner = facts::string_at(&event.payload, facts::TERMINAL_OWNER);
        let state = if kind.is_some() && owner.is_some() {
            RecapCompletionState::Complete
        } else {
            RecapCompletionState::Incomplete
        };
        let redact =
            |value: Option<String>| value.map(|text| self.policy.redact_string(&text).into_owned());
        self.turns[turn_index].recap.terminal = Some(RecapTerminalFact {
            state,
            final_status: redact(facts::string_at(&event.payload, facts::FINAL_STATUS)),
            stop_reason: redact(facts::string_at(&event.payload, facts::STOP_REASON)),
            kind: redact(kind),
            owner: redact(owner),
            reason: redact(facts::string_at(&event.payload, facts::TERMINAL_REASON)),
            source_event_id: event.event_id,
        });
        self.matched += 1;
    }

    fn finish(mut self) -> (Vec<PromptTurnRecap>, usize, usize) {
        for turn in &mut self.turns {
            let has_terminal = turn.recap.terminal.is_some();
            let mut incomplete = turn
                .recap
                .terminal
                .as_ref()
                .is_some_and(|terminal| terminal.state == RecapCompletionState::Incomplete);
            for iteration in &mut turn.recap.iterations {
                if iteration.iteration.is_none() {
                    iteration.state = RecapCompletionState::Unassigned;
                    incomplete = true;
                } else if iteration.started && iteration.ended {
                    iteration.state = RecapCompletionState::Complete;
                } else {
                    iteration.state = RecapCompletionState::Incomplete;
                    incomplete = true;
                }
                for tool in &mut iteration.tools {
                    if tool.call_observed && tool.result_observed {
                        // Preserve the result-owned completed/failed state.
                    } else if has_terminal || tool.call_observed || tool.result_observed {
                        tool.state = RecapToolState::Incomplete;
                        incomplete = true;
                    } else {
                        tool.state = RecapToolState::Incomplete;
                        incomplete = true;
                    }
                }
            }
            turn.recap.state = if incomplete {
                RecapCompletionState::Incomplete
            } else if has_terminal {
                RecapCompletionState::Complete
            } else if turn.recap.iterations.is_empty() {
                RecapCompletionState::Open
            } else {
                RecapCompletionState::Incomplete
            };
        }
        (
            self.turns.into_iter().map(|turn| turn.recap).collect(),
            self.matched,
            self.unassigned,
        )
    }
}

fn is_recap_event(event: &StoredEvent) -> bool {
    matches!(
        event.kind,
        SessionEventKind::Message
            | SessionEventKind::ToolCall
            | SessionEventKind::ToolResult
            | SessionEventKind::Plan
    ) || matches!(
        event.kind.discriminator(),
        "loop_checkpoint" | "progress_reported" | "agent_run_terminal"
    )
}

fn is_checkpoint(event: &StoredEvent) -> bool {
    event.kind.discriminator() == "loop_checkpoint"
}

fn tool_call_id(event: &StoredEvent) -> Option<String> {
    event
        .headers
        .get("tool_call_id")
        .cloned()
        .or_else(|| facts::string_at(&event.payload, facts::TOOL_CALL_ID))
        .or_else(|| {
            event
                .payload
                .pointer(facts::TOOL_RESULT_FACT_CALL_ID)
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
}

fn find_tool(iterations: &[IterationRecap], call_id: &str) -> Option<(usize, usize)> {
    iterations
        .iter()
        .enumerate()
        .find_map(|(segment, iteration)| {
            iteration
                .tools
                .iter()
                .position(|tool| tool.tool_call_id == call_id)
                .map(|tool| (segment, tool))
        })
}

fn upsert_tool(
    iterations: &mut [IterationRecap],
    segment_index: usize,
    incoming: RecapToolExchange,
) {
    if let Some((existing_segment, existing_tool)) = find_tool(iterations, &incoming.tool_call_id) {
        let tool = &mut iterations[existing_segment].tools[existing_tool];
        if incoming.tool_name.is_some() {
            tool.tool_name = incoming.tool_name;
        }
        tool.call_observed |= incoming.call_observed;
        tool.result_observed |= incoming.result_observed;
        if incoming.input.is_some() {
            tool.input = incoming.input;
        }
        if incoming.output.is_some() {
            tool.output = incoming.output;
        }
        if incoming.verification.is_some() {
            tool.verification = incoming.verification;
        }
        if incoming.result_observed {
            tool.state = incoming.state;
        }
        tool.source_event_ids.extend(incoming.source_event_ids);
        return;
    }
    iterations[segment_index].tools.push(incoming);
}

fn recap_plan_step_status(status: &crate::llm::plan::PlanStepStatus) -> RecapPlanStepStatus {
    match status {
        crate::llm::plan::PlanStepStatus::Pending => RecapPlanStepStatus::Pending,
        crate::llm::plan::PlanStepStatus::InProgress => RecapPlanStepStatus::InProgress,
        crate::llm::plan::PlanStepStatus::Completed => RecapPlanStepStatus::Completed,
        crate::llm::plan::PlanStepStatus::Blocked => RecapPlanStepStatus::Blocked,
        crate::llm::plan::PlanStepStatus::Cancelled => RecapPlanStepStatus::Cancelled,
    }
}

fn progress_entry(
    value: &serde_json::Value,
    policy: &RedactionPolicy,
) -> Result<RecapProgressEntry, ()> {
    let content = value
        .get("content")
        .and_then(serde_json::Value::as_str)
        .filter(|content| !content.trim().is_empty())
        .ok_or(())?;
    let status = match value.get("status").and_then(serde_json::Value::as_str) {
        Some("pending") => RecapProgressStatus::Pending,
        Some("in_progress") => RecapProgressStatus::InProgress,
        Some("completed") => RecapProgressStatus::Completed,
        _ => return Err(()),
    };
    let priority = match value.get("priority") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(priority)) => Some(match priority.as_str() {
            "high" => RecapProgressPriority::High,
            "medium" => RecapProgressPriority::Medium,
            "low" => RecapProgressPriority::Low,
            _ => return Err(()),
        }),
        _ => return Err(()),
    };
    Ok(RecapProgressEntry {
        content: policy.redact_string(content).into_owned(),
        status,
        priority,
    })
}

fn verification_fact(
    event: &StoredEvent,
    policy: &RedactionPolicy,
) -> Option<RecapVerificationFact> {
    let value = event.payload.pointer(facts::TOOL_VERIFICATION)?;
    let schema = value.get("schema")?.as_str()?;
    let status = value.get("status")?.as_str()?;
    if schema != "harn.agent_tool_postcondition.v1" || status != "passed" {
        return None;
    }
    let verified_paths = value
        .get("verified_paths")?
        .as_array()?
        .iter()
        .map(serde_json::Value::as_str)
        .collect::<Option<Vec<_>>>()?
        .into_iter()
        .map(|path| policy.redact_string(path).into_owned())
        .collect();
    Some(RecapVerificationFact {
        schema: schema.to_string(),
        status: status.to_string(),
        verified_paths,
        source_event_id: event.event_id,
    })
}

fn sha256_canonical<T: Serialize>(value: &T) -> String {
    let json = serde_json::to_value(value).expect("recap contract must serialize");
    format!(
        "sha256:{}",
        hex::encode(Sha256::digest(crate::canonical_json::to_vec(&json)))
    )
}

#[cfg(test)]
#[path = "session_recap_tests.rs"]
mod tests;
