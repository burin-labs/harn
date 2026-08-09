//! Project a [`RunRecord`] from a persisted agent session.
//!
//! Every `harn runs` surface — `report`, `review`, `view`, `inspect`,
//! `export-training` — opens a run record. Nothing on the agent-session path
//! writes one: `save_run_record` is called from `std/records`, `std/workflow`,
//! and `run_review`, so a record exists only when a Harn *script* asks for one.
//! A host that drives the agent loop directly, which is the canonical path for
//! an IDE, gets full event persistence and no run record, and every reporting
//! tool is inapplicable to the run it just finished. Issue #6120 is one such
//! run: 9 252 persisted events, 2 046 session events, and nothing to open.
//!
//! Rather than making run-record emission a per-host obligation — discovered
//! only when someone tries to report on a run and finds nothing — Harn projects
//! the record from the session it already persisted. The one place that knows
//! how to build a `RunRecord` stays in Harn, and any host on Harn's session
//! store gets the whole reporting surface without writing code.
//!
//! ## What a projection can and cannot recover
//!
//! A projected record is explicitly marked as one (see [`PROJECTION_SOURCE`]
//! and the `projected_from` metadata block) so no consumer mistakes it for a
//! recorder-written record.
//!
//! Workflow-shaped fields — stages, transitions, checkpoints, pending and
//! completed nodes — come back empty. That is not loss: an agent session has no
//! stages, so empty is the accurate reading rather than a missing value.
//!
//! [`UNRECOVERABLE_FIELDS`] names the fields that are genuinely unavailable
//! from a session alone, and a test asserts that list stays exactly the set the
//! projector leaves at its default. That way the list cannot rot in either
//! direction: naming a field that is now populated fails, and populating a
//! field without removing it from the list fails too.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use harn_session_store::{
    EventId, ListFilter, ReadRange, SessionMeta, SessionStatus, SessionStore, StoreError,
    StoredEvent, MAX_READ_BATCH,
};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde_json::json;

use super::types::{LlmUsageRecord, RunChildRecord, RunRecord, RunTraceSpanRecord, ToolCallRecord};
use crate::agent_sessions::event_facts as facts;
use crate::value::VmError;

/// Marker written into `metadata.projected_from.source`, identifying both that
/// this record was projected and what it was projected from.
pub const PROJECTION_SOURCE: &str = "harn.session_store.v1";

/// Placeholder workflow id for a run that had no workflow.
///
/// An agent session is a run, but not a workflow run. Naming that explicitly
/// beats an empty string, which reads as "we forgot" rather than "there wasn't
/// one".
pub const AGENT_SESSION_WORKFLOW_ID: &str = "agent-session";

/// Dotted `RunRecord` field paths a session projection cannot source.
///
/// - `usage.total_duration_ms` and `trace_spans[].duration_ms`: `llm_call`
///   session events carry tokens, cost, model, and provider, but no per-call
///   latency. `harn runs report --events-db` already joins the event log, which
///   does record it; that is the seam for latency rather than a guess made
///   here. Each projected span carries `duration_available: false` so a zero is
///   not mistaken for a measurement.
/// - `policy`: the capability policy is a launch-time input, not something the
///   session replays.
/// - `replay_fixture`: derived by `save_run_record` from the assembled record,
///   so a projection has nothing of its own to contribute.
pub const UNRECOVERABLE_FIELDS: [&str; 4] = [
    "usage.total_duration_ms",
    "trace_spans[].duration_ms",
    "policy",
    "replay_fixture",
];

/// Project the session `session_id` into a [`RunRecord`].
///
/// Reads only through the store, so the same projection serves a SQLite store
/// on disk and an in-memory one under test.
pub async fn project_run_record_from_session(
    store: &dyn SessionStore,
    session_id: &str,
) -> Result<RunRecord, VmError> {
    let meta = store
        .describe(session_id)
        .await
        .map_err(|error| match error {
            StoreError::NotFound(_) => VmError::Runtime(format!(
                "runs: no session '{session_id}' in this store. `harn session list` shows the \
             sessions this workspace has persisted."
            )),
            other => VmError::Runtime(format!("runs: failed to describe session: {other}")),
        })?;
    let events = drain_events(store, session_id).await?;
    let children = child_records(store, session_id).await?;
    let root = root_session_id(store, &meta).await?;
    Ok(assemble(meta, events, children, root))
}

/// Walk `parent_session_id` to the top of the delegation chain.
///
/// A grandchild's root is its grandparent, not its parent, so this cannot stop
/// at one hop. The visited set bounds a store whose lineage has somehow become
/// cyclic: reporting the last session before the cycle beats looping forever
/// inside a reporting command.
async fn root_session_id(store: &dyn SessionStore, meta: &SessionMeta) -> Result<String, VmError> {
    let mut visited = std::collections::HashSet::from([meta.id.clone()]);
    let mut current = meta.parent_session_id.clone();
    let mut root = meta.id.clone();
    while let Some(parent) = current {
        if !visited.insert(parent.clone()) {
            break;
        }
        match store.describe(&parent).await {
            Ok(parent_meta) => {
                root = parent_meta.id.clone();
                current = parent_meta.parent_session_id;
            }
            // A parent pruned by retention leaves the deepest session we can
            // still see as the root we can honestly name.
            Err(StoreError::NotFound(_)) => {
                root = parent;
                break;
            }
            Err(error) => {
                return Err(VmError::Runtime(format!(
                    "runs: failed to walk session lineage: {error}"
                )))
            }
        }
    }
    Ok(root)
}

/// Project `session_id` out of the canonical store under `root` and persist the
/// result, returning the written path.
///
/// This is the whole host-facing surface: a host that already writes to Harn's
/// session store needs one call to make every `harn runs` tool apply to a run.
pub async fn materialize_session_run_record(
    root: &Path,
    session_id: &str,
    out: Option<&Path>,
) -> Result<String, VmError> {
    let store =
        crate::stdlib::session_store::open_existing_canonical_store(root)?.ok_or_else(|| {
            VmError::Runtime(format!(
                "runs: no session store under {}. A projected run record needs \
                 `.harn/session-store.sqlite`; pass the workspace root that holds it.",
                root.display()
            ))
        })?;
    let run = project_run_record_from_session(&store, session_id).await?;
    let path = out
        .map(Path::to_path_buf)
        .unwrap_or_else(|| default_projection_path(root, session_id));
    super::persistence::save_run_record(&run, Some(&path.to_string_lossy()))
}

/// Where a projected record lands when the caller names no path.
///
/// Deterministic by session id so a host can resolve the record for a session
/// without threading a path back through its own state — which is the join
/// burin-code#5831 needs and the `sessions` table has no column for.
pub fn default_projection_path(root: &Path, session_id: &str) -> PathBuf {
    crate::runtime_paths::run_root(root).join(format!("{session_id}.json"))
}

/// One row of `harn session list`: enough to pick a session to report on
/// without opening the store by hand.
///
/// Built from [`SessionMeta`] alone rather than by draining events, so listing
/// a workspace with thousands of persisted events stays a single query.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct SessionRunSummary {
    pub session_id: String,
    /// The session's own lifecycle status: `open`, `closed`, `soft_deleted`,
    /// or `hard_deleted`. Not the run's status — a host that exits without
    /// closing leaves a finished run's session `open`.
    pub session_status: String,
    pub title: Option<String>,
    pub parent_session_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub event_count: usize,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd_micros: u64,
    /// Path to an already-materialized run record for this session, when one
    /// exists. `None` means `--from-session` would project a fresh one.
    pub run_record_path: Option<String>,
}

/// List the sessions persisted under `root`, newest first.
///
/// Without this, `--from-session` needs an id the caller can only get by
/// opening SQLite by hand, so the reporting surface stays as unreachable as it
/// was before it accepted sessions at all.
pub async fn list_session_runs(
    root: &Path,
    limit: Option<usize>,
) -> Result<Vec<SessionRunSummary>, VmError> {
    let Some(store) = crate::stdlib::session_store::open_existing_canonical_store(root)? else {
        return Ok(Vec::new());
    };
    let sessions = store
        .list(ListFilter {
            limit,
            sort_by: harn_session_store::ListSortKey::CreatedAt,
            order: harn_session_store::ListOrder::Descending,
            ..ListFilter::default()
        })
        .await
        .map_err(|error| VmError::Runtime(format!("runs: failed to list sessions: {error}")))?;
    Ok(sessions
        .into_iter()
        .map(|meta| {
            let record = default_projection_path(root, &meta.id);
            SessionRunSummary {
                session_status: status_discriminator(&meta.status).to_string(),
                title: meta.title.clone(),
                parent_session_id: meta.parent_session_id.clone(),
                created_at: meta.created_at.clone(),
                updated_at: meta.updated_at.clone(),
                event_count: meta.event_count,
                input_tokens: meta.usage_input,
                output_tokens: meta.usage_output,
                cost_usd_micros: meta.usage_cost_usd_micros,
                run_record_path: record
                    .is_file()
                    .then(|| record.to_string_lossy().into_owned()),
                session_id: meta.id,
            }
        })
        .collect())
}

async fn drain_events(
    store: &dyn SessionStore,
    session_id: &str,
) -> Result<Vec<StoredEvent>, VmError> {
    let mut all = Vec::new();
    let mut cursor: Option<EventId> = None;
    loop {
        let page = store
            .read(
                session_id,
                ReadRange {
                    from_event_id: cursor,
                    to_event_id: None,
                    limit: Some(MAX_READ_BATCH),
                },
            )
            .await
            .map_err(|error| {
                VmError::Runtime(format!("runs: failed to read session events: {error}"))
            })?;
        let next = page.next_cursor;
        all.extend(page.events);
        match next {
            Some(next_cursor) => cursor = Some(next_cursor),
            None => break,
        }
    }
    Ok(all)
}

/// Direct children of this session, as the store's own lineage records them.
///
/// `sessions.parent_session_id` is the delegation edge, so children come back
/// without re-deriving lineage from worker metadata the way a recorder-written
/// record has to.
async fn child_records(
    store: &dyn SessionStore,
    session_id: &str,
) -> Result<Vec<RunChildRecord>, VmError> {
    let children = store
        .list(ListFilter {
            parent_session_id: Some(session_id.to_string()),
            ..ListFilter::default()
        })
        .await
        .map_err(|error| {
            VmError::Runtime(format!("runs: failed to list child sessions: {error}"))
        })?;
    Ok(children
        .into_iter()
        .map(|child| RunChildRecord {
            worker_id: child.id.clone(),
            worker_name: child.persona.clone().unwrap_or_default(),
            session_id: Some(child.id.clone()),
            parent_session_id: Some(session_id.to_string()),
            task: child.title.clone().unwrap_or_default(),
            status: run_status_for(&child.status, None, None).to_string(),
            started_at: child.created_at.clone(),
            finished_at: child.closed_at.clone(),
            run_id: Some(child.id.clone()),
            ..RunChildRecord::default()
        })
        .collect())
}

/// Facts folded out of one pass over the session's events.
#[derive(Default)]
struct SessionFold {
    task: Option<String>,
    usage: LlmUsageRecord,
    models: Vec<String>,
    providers: Vec<String>,
    cache_read_tokens: i64,
    cache_write_tokens: i64,
    /// Provider requests across every call, and why the extra ones happened.
    /// Distinct from `usage.call_count`, which counts logical calls: a run
    /// whose provider rejected a third of its requests with a retryable 429
    /// reported a clean call count and no contention signal at all (#5847).
    provider_attempts: i64,
    rate_limited_attempts: i64,
    empty_completion_attempts: i64,
    other_retry_attempts: i64,
    /// Cost accumulated as an exact decimal rather than by adding `f64`s.
    /// Summing 96 float costs from a real run produced
    /// `0.6060984600000002`; money is a base-10 quantity and a run report is
    /// read by people reconciling spend, so the accumulator is exact and the
    /// single conversion to `f64` happens once at the boundary the record type
    /// requires.
    total_cost: Decimal,
    tools: Vec<ToolCallRecord>,
    /// Index into `tools` by provider tool-call id, so a later update or result
    /// lands on the call it belongs to rather than on whichever call was last.
    tool_index: BTreeMap<String, usize>,
    iteration: usize,
    max_iteration: usize,
    terminal: Option<TerminalFacts>,
    llm_calls: Vec<LlmCallFacts>,
}

/// One provider call as the session recorded it.
///
/// Kept separate from the running `usage` aggregate because the report's
/// per-call view needs each call individually, and a total cannot be
/// un-summed.
struct LlmCallFacts {
    at_ms: i64,
    model: Option<String>,
    provider: Option<String>,
    input_tokens: i64,
    output_tokens: i64,
    cache_read_tokens: i64,
    cache_write_tokens: i64,
    cost_usd: Option<f64>,
}

struct TerminalFacts {
    final_status: Option<String>,
    stop_reason: Option<String>,
    error: Option<String>,
    class: Option<String>,
    kind: Option<crate::agent_events::AgentTerminalKind>,
    owner: Option<String>,
    reason: Option<String>,
    at: String,
}

fn assemble(
    meta: SessionMeta,
    events: Vec<StoredEvent>,
    children: Vec<RunChildRecord>,
    root_run_id: String,
) -> RunRecord {
    let mut fold = SessionFold::default();
    for event in &events {
        fold.absorb(event);
    }

    let status = run_status_for(
        &meta.status,
        fold.terminal.as_ref().and_then(|terminal| terminal.kind),
        fold.terminal
            .as_ref()
            .and_then(|t| t.final_status.as_deref()),
    )
    .to_string();
    // A session left `open` by a host that exited without closing it still has
    // a terminal event; that event's timestamp is when the run actually ended,
    // and preferring `closed_at` when present keeps a cleanly closed session
    // authoritative over it.
    let finished_at = meta
        .closed_at
        .clone()
        .or_else(|| fold.terminal.as_ref().map(|t| t.at.clone()));

    let mut metadata = BTreeMap::new();
    metadata.insert(
        "projected_from".to_string(),
        json!({
            "source": PROJECTION_SOURCE,
            "session_id": meta.id,
            "session_status": status_discriminator(&meta.status),
            "session_event_count": meta.event_count,
            // Named for the *source*, not the file: `save_run_record` derives a
            // `replay_fixture` from the assembled record on the way to disk, so
            // a persisted projection has one even though the session never
            // carried it. The claim here is about what the session could tell
            // us, which is what a reader deciding whether to trust a field
            // needs to know.
            "not_recoverable_from_session": UNRECOVERABLE_FIELDS,
        }),
    );
    // Wall clock is the session's own span. It is deliberately not folded into
    // `usage.total_duration_ms`, which means time spent inside LLM calls and is
    // not recoverable here — reporting one as the other would overstate model
    // time by every second the run spent running tools.
    metadata.insert(
        "wall_clock_ms".to_string(),
        json!(meta.updated_at_ms.saturating_sub(meta.created_at_ms)),
    );
    if fold.max_iteration > 0 {
        metadata.insert("iterations".to_string(), json!(fold.max_iteration));
    }
    if fold.cache_read_tokens > 0 || fold.cache_write_tokens > 0 {
        metadata.insert(
            "cache_tokens".to_string(),
            json!({"read": fold.cache_read_tokens, "write": fold.cache_write_tokens}),
        );
    }
    if !fold.providers.is_empty() {
        metadata.insert("providers".to_string(), json!(fold.providers));
    }
    // Only reported when the run actually retried. A block of zeroes on every
    // clean run would train a reader to skip the one place the contention
    // signal appears.
    if fold.provider_attempts > fold.usage.call_count {
        metadata.insert(
            "provider_attempts".to_string(),
            json!({
                "total": fold.provider_attempts,
                "retries": fold.provider_attempts - fold.usage.call_count,
                "rate_limited": fold.rate_limited_attempts,
                "empty_completion": fold.empty_completion_attempts,
                "other": fold.other_retry_attempts,
            }),
        );
    }
    if let Some(terminal) = &fold.terminal {
        if let Some(stop_reason) = &terminal.stop_reason {
            metadata.insert("stop_reason".to_string(), json!(stop_reason));
        }
        if let Some(class) = &terminal.class {
            metadata.insert("terminal_class".to_string(), json!(class));
        }
        if let Some(error) = &terminal.error {
            metadata.insert("terminal_error".to_string(), json!(error));
        }
        if let Some(kind) = terminal.kind {
            metadata.insert(
                "terminal".to_string(),
                json!({
                    "kind": kind.as_str(),
                    "reason": terminal.reason.as_deref().or(terminal.stop_reason.as_deref()),
                    "owner": terminal.owner.as_deref().unwrap_or_else(|| kind.owner()),
                }),
            );
        }
    }

    let usage = LlmUsageRecord {
        models: fold.models.clone(),
        total_cost: fold.total_cost.to_f64().unwrap_or_default(),
        ..fold.usage
    };

    RunRecord {
        type_name: "run".to_string(),
        id: meta.id.clone(),
        workflow_id: AGENT_SESSION_WORKFLOW_ID.to_string(),
        workflow_name: meta.persona.clone(),
        // Title when a host or person named the run; otherwise the first thing
        // the run was actually asked to do. Not truncated: what the run was
        // given is the fact, and display surfaces can shorten it.
        task: meta.title.clone().or(fold.task).unwrap_or_default(),
        status,
        started_at: meta.created_at.clone(),
        finished_at,
        parent_run_id: meta.parent_session_id.clone(),
        root_run_id: Some(root_run_id),
        child_runs: children,
        usage: (usage.call_count > 0).then_some(usage),
        trace_spans: llm_call_spans(&meta, &fold.llm_calls),
        tool_recordings: fold.tools,
        execution: None,
        metadata,
        ..RunRecord::default()
    }
}

impl SessionFold {
    fn absorb(&mut self, event: &StoredEvent) {
        match event.kind.discriminator() {
            "message" => self.absorb_message(event),
            "tool_call" => self.absorb_tool_call(event),
            "tool_call_update" => self.absorb_tool_update(event),
            "tool_result" => self.absorb_tool_result(event),
            "llm_call" => self.absorb_llm_call(event),
            "loop_checkpoint" => self.absorb_checkpoint(event),
            "agent_run_terminal" => self.absorb_terminal(event),
            _ => {}
        }
    }

    fn absorb_message(&mut self, event: &StoredEvent) {
        if self.task.is_some() {
            return;
        }
        let is_user = event.actor.as_deref() == Some("user")
            || facts::semantic_string(&event.payload, &facts::ROLE).as_deref() == Some("user");
        if is_user {
            self.task = facts::semantic_string(&event.payload, &facts::TEXT);
        }
    }

    fn absorb_llm_call(&mut self, event: &StoredEvent) {
        let payload = &event.payload;
        self.llm_calls.push(LlmCallFacts {
            at_ms: event.ts_ms,
            model: facts::string_at(payload, facts::MODEL),
            provider: facts::string_at(payload, facts::PROVIDER),
            input_tokens: facts::i64_at(payload, facts::INPUT_TOKENS).unwrap_or(0),
            output_tokens: facts::i64_at(payload, facts::OUTPUT_TOKENS).unwrap_or(0),
            cache_read_tokens: facts::i64_at(payload, facts::CACHE_READ_TOKENS).unwrap_or(0),
            cache_write_tokens: facts::i64_at(payload, facts::CACHE_WRITE_TOKENS).unwrap_or(0),
            cost_usd: facts::f64_at(payload, facts::COST_USD),
        });
        self.usage.call_count += 1;
        self.usage.input_tokens += facts::i64_at(payload, facts::INPUT_TOKENS).unwrap_or(0);
        self.usage.output_tokens += facts::i64_at(payload, facts::OUTPUT_TOKENS).unwrap_or(0);
        if let Some(cost) = facts::f64_at(payload, facts::COST_USD) {
            self.total_cost += Decimal::from_f64_retain(cost).unwrap_or_default();
        }
        self.cache_read_tokens += facts::i64_at(payload, facts::CACHE_READ_TOKENS).unwrap_or(0);
        self.cache_write_tokens += facts::i64_at(payload, facts::CACHE_WRITE_TOKENS).unwrap_or(0);
        // A call recorded before provider attempts existed has no entry. It
        // still made at least one request, so counting 1 keeps the total a
        // lower bound rather than under-reporting a mixed-age session.
        self.provider_attempts += facts::i64_at(payload, facts::PROVIDER_ATTEMPTS_TOTAL)
            .filter(|total| *total > 0)
            .unwrap_or(1);
        self.rate_limited_attempts +=
            facts::i64_at(payload, facts::PROVIDER_ATTEMPTS_RATE_LIMITED).unwrap_or(0);
        self.empty_completion_attempts +=
            facts::i64_at(payload, facts::PROVIDER_ATTEMPTS_EMPTY).unwrap_or(0);
        self.other_retry_attempts +=
            facts::i64_at(payload, facts::PROVIDER_ATTEMPTS_OTHER).unwrap_or(0);
        if let Some(model) = facts::string_at(payload, facts::MODEL) {
            push_distinct(&mut self.models, model);
        }
        if let Some(provider) = facts::string_at(payload, facts::PROVIDER) {
            push_distinct(&mut self.providers, provider);
        }
    }

    fn absorb_checkpoint(&mut self, event: &StoredEvent) {
        if facts::string_at(&event.payload, facts::CHECKPOINT_KIND).as_deref()
            != Some("iteration_start")
        {
            return;
        }
        if let Some(iteration) = facts::i64_at(&event.payload, facts::ITERATION) {
            self.iteration = usize::try_from(iteration).unwrap_or(0);
            self.max_iteration = self.max_iteration.max(self.iteration);
        }
    }

    fn absorb_tool_call(&mut self, event: &StoredEvent) {
        let payload = &event.payload;
        let Some(tool_call_id) = facts::string_at(payload, facts::TOOL_CALL_ID) else {
            return;
        };
        if self.tool_index.contains_key(&tool_call_id) {
            return;
        }
        let args = facts::semantic_value(payload, &[facts::TOOL_RAW_INPUT])
            .unwrap_or(serde_json::Value::Null);
        let tool_name = facts::semantic_string(payload, &facts::TOOL_NAME_ANY).unwrap_or_default();
        self.tool_index
            .insert(tool_call_id.clone(), self.tools.len());
        self.tools.push(ToolCallRecord {
            args_hash: super::types::tool_fixture_hash(&tool_name, &args),
            tool_name,
            tool_use_id: tool_call_id,
            iteration: self.iteration,
            timestamp: event.ts.clone(),
            ..ToolCallRecord::default()
        });
    }

    fn absorb_tool_update(&mut self, event: &StoredEvent) {
        let payload = &event.payload;
        let Some(record) = self.tool_for(payload) else {
            return;
        };
        let status = facts::string_at(payload, facts::TOOL_STATUS);
        // Only a terminal update carries a duration, and only a terminal update
        // should overwrite a rejection already recorded for this call.
        match status.as_deref() {
            Some("completed") | Some("failed") | Some("rejected") => {
                record.is_rejected = status.as_deref() == Some("rejected");
                if let Some(duration) = facts::i64_at(payload, facts::TOOL_DURATION_MS) {
                    record.duration_ms = u64::try_from(duration).unwrap_or(0);
                }
            }
            _ => {}
        }
    }

    /// Attach a tool's output to the call it answers.
    ///
    /// `is_error` is deliberately not folded into `is_rejected`: a tool that
    /// ran and failed is not a tool whose call was refused, and a rejected call
    /// still emits a result event, so treating the two as one would clear the
    /// rejection recorded moments earlier. The failure stays legible in
    /// `result`, which carries the error text verbatim.
    fn absorb_tool_result(&mut self, event: &StoredEvent) {
        let payload = &event.payload;
        let text = facts::semantic_string(payload, &facts::TEXT).unwrap_or_default();
        let Some(record) = self.tool_for(payload) else {
            return;
        };
        record.result = text;
    }

    /// Resolve the recorded call this event belongs to, by provider tool-call
    /// id. Returns `None` for an event that names no call or names one this
    /// session never opened.
    fn tool_for(&mut self, payload: &serde_json::Value) -> Option<&mut ToolCallRecord> {
        let tool_call_id = facts::string_at(payload, facts::TOOL_CALL_ID)?;
        let index = *self.tool_index.get(&tool_call_id)?;
        self.tools.get_mut(index)
    }

    fn absorb_terminal(&mut self, event: &StoredEvent) {
        self.terminal = Some(TerminalFacts {
            final_status: facts::string_at(&event.payload, facts::FINAL_STATUS),
            stop_reason: facts::string_at(&event.payload, facts::STOP_REASON),
            error: facts::string_at(&event.payload, facts::TERMINAL_ERROR),
            class: facts::string_at(&event.payload, facts::TERMINAL_CLASS),
            kind: facts::string_at(&event.payload, facts::TERMINAL_KIND)
                .as_deref()
                .and_then(crate::agent_events::AgentTerminalKind::from_wire),
            owner: facts::string_at(&event.payload, facts::TERMINAL_OWNER),
            reason: facts::string_at(&event.payload, facts::TERMINAL_REASON),
            at: event.ts.clone(),
        });
    }
}

/// Map a session's lifecycle status and its loop's terminal status onto the
/// run-record status vocabulary.
///
/// The loop's own verdict wins when it left one: a host that exits without
/// closing the session leaves `status = open`, which would otherwise report a
/// finished run as still running.
fn run_status_for(
    session_status: &SessionStatus,
    terminal_kind: Option<crate::agent_events::AgentTerminalKind>,
    final_status: Option<&str>,
) -> &'static str {
    if let Some(kind) = terminal_kind {
        return kind.lifecycle_state().wire_name();
    }
    if let Some(final_status) = final_status.filter(|value| !value.is_empty()) {
        return if crate::llm::session_status_indicates_error(final_status) {
            "failed"
        } else {
            "completed"
        };
    }
    match session_status {
        SessionStatus::Open => "running",
        SessionStatus::Closed => "completed",
        SessionStatus::SoftDeleted | SessionStatus::HardDeleted => "deleted",
    }
}

fn status_discriminator(status: &SessionStatus) -> &'static str {
    match status {
        SessionStatus::Open => "open",
        SessionStatus::Closed => "closed",
        SessionStatus::SoftDeleted => "soft_deleted",
        SessionStatus::HardDeleted => "hard_deleted",
    }
}

/// Project each recorded provider call into an `llm_call` trace span.
///
/// Without these the run report's `llm_calls` array comes back empty, which
/// reads as "this run made no model calls" rather than "the per-call view has
/// no source here" — affirmatively wrong for a run that made 96 of them.
///
/// `duration_ms` is 0 because a session `llm_call` event records tokens, cost,
/// model, and provider but no latency, and the field is not optional. Each span
/// says so in its metadata rather than letting a zero pass as a measurement:
/// `--events-db` is the seam that carries real timing, and
/// [`UNRECOVERABLE_FIELDS`] names this alongside `usage.total_duration_ms`.
///
/// `start_ms` is relative to the session's creation, matching the collector's
/// epoch-relative convention. Absolute epoch milliseconds would be a different
/// unit in the same field.
fn llm_call_spans(meta: &SessionMeta, calls: &[LlmCallFacts]) -> Vec<RunTraceSpanRecord> {
    calls
        .iter()
        .enumerate()
        .map(|(index, call)| {
            let mut metadata = BTreeMap::from([
                (
                    crate::tracing::meta::INPUT_TOKENS.to_string(),
                    json!(call.input_tokens),
                ),
                (
                    crate::tracing::meta::OUTPUT_TOKENS.to_string(),
                    json!(call.output_tokens),
                ),
                (
                    crate::tracing::meta::CACHE_READ_TOKENS.to_string(),
                    json!(call.cache_read_tokens),
                ),
                (
                    crate::tracing::meta::CACHE_WRITE_TOKENS.to_string(),
                    json!(call.cache_write_tokens),
                ),
                ("duration_available".to_string(), json!(false)),
            ]);
            if let Some(model) = &call.model {
                metadata.insert(crate::tracing::meta::MODEL.to_string(), json!(model));
            }
            if let Some(provider) = &call.provider {
                metadata.insert(crate::tracing::meta::PROVIDER.to_string(), json!(provider));
            }
            RunTraceSpanRecord {
                trace_id: meta.id.clone(),
                // One-based so span ids stay distinguishable from the absent
                // parent, which is `None` rather than 0.
                span_id: index as u64 + 1,
                parent_id: None,
                kind: "llm_call".to_string(),
                name: call.model.clone().unwrap_or_else(|| "llm_call".to_string()),
                start_ms: u64::try_from(call.at_ms.saturating_sub(meta.created_at_ms)).unwrap_or(0),
                duration_ms: 0,
                ttft_ms: None,
                metadata,
                links: Vec::new(),
                cost_usd: call.cost_usd,
            }
        })
        .collect()
}

fn push_distinct(values: &mut Vec<String>, value: String) {
    if !values.contains(&value) {
        values.push(value);
    }
}

#[cfg(test)]
mod tests;
