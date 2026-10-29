//! Production run tape and branch replay.
//!
//! A run tape is the testbench [`EventTape`] format recorded from a real run:
//! every model call (request digest, full request at `recording: full`, and
//! the response), every agent tool dispatch (outcome plus the files it
//! changed), and the host reads the testbench axes already record. One tape
//! per process: the recorder is process-scoped so provider calls that run on
//! a worker thread land on it too.
//!
//! A branch re-runs the program against a tape. The first `live_after_calls`
//! model calls are served from the recording and verified: each request must
//! digest to the recorded one, or the branch fails closed and never goes live.
//! Tool calls named by a served response return their recorded outcome and
//! re-apply their recorded file effects, so the workspace evolves exactly as
//! it did and nothing in the prefix re-executes. From call `live_after_calls +
//! 1` everything is live.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use serde::{Deserialize, Serialize};

use crate::llm::api::{LlmRequestPayload, LlmResult, ProviderTelemetry};
use crate::testbench::tape::{
    content_hash, EventTape, FileEffect, TapeHeader, TapePayload, TapeRecordKind, TapeRecorder,
};

/// File name of the tape inside a run's tape directory; the CAS sidecar is
/// `run.tape.cas/` beside it.
pub const TAPE_FILE_NAME: &str = "run.tape";

/// How much of a run the tape keeps.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordingLevel {
    /// Full requests, responses, tool outcomes and file effects: branchable.
    #[default]
    Full,
    /// Digests, tokens and timing only. Readable, not branchable.
    Metadata,
    /// No tape.
    Off,
}

impl RecordingLevel {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "full" => Ok(Self::Full),
            "metadata" => Ok(Self::Metadata),
            "off" => Ok(Self::Off),
            other => Err(format!(
                "recording must be full, metadata, or off; got `{other}`"
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Metadata => "metadata",
            Self::Off => "off",
        }
    }
}

struct ProcessTape {
    recorder: Arc<TapeRecorder>,
    level: RecordingLevel,
    workspace_root: Option<PathBuf>,
    next_llm_call: AtomicU64,
    prompts: Mutex<prompts::PromptLedger>,
}

static PROCESS_TAPE: RwLock<Option<Arc<ProcessTape>>> = RwLock::new(None);
static PROCESS_BRANCH: Mutex<Option<BranchState>> = Mutex::new(None);
static EFFECT_OBSERVER: OnceLock<Box<dyn ToolEffectObserver>> = OnceLock::new();

fn process_tape() -> Option<Arc<ProcessTape>> {
    PROCESS_TAPE
        .read()
        .expect("run tape lock poisoned")
        .as_ref()
        .cloned()
}

/// The process run tape's recorder, consulted by
/// [`crate::testbench::tape::active_recorder`] when no thread-local testbench
/// recorder is installed.
pub fn process_recorder() -> Option<Arc<TapeRecorder>> {
    process_tape().map(|tape| tape.recorder.clone())
}

/// Ends recording when dropped and reports any write failure.
pub struct RunTapeGuard {
    recorder: Arc<TapeRecorder>,
    path: PathBuf,
}

impl RunTapeGuard {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The first write failure, if any. A tape with one is incomplete.
    pub fn error(&self) -> Option<String> {
        self.recorder.sink_error()
    }
}

impl Drop for RunTapeGuard {
    fn drop(&mut self) {
        let mut slot = PROCESS_TAPE.write().expect("run tape lock poisoned");
        if slot
            .as_ref()
            .is_some_and(|tape| Arc::ptr_eq(&tape.recorder, &self.recorder))
        {
            *slot = None;
        }
    }
}

/// Start recording this process's run into `dir/run.tape`. Returns `None` for
/// [`RecordingLevel::Off`]. `workspace_root` scopes tool file effects; without
/// it tool outcomes are recorded but effects are not, and the tape is marked
/// unservable for tools that changed files.
pub fn start_recording(
    dir: &Path,
    level: RecordingLevel,
    workspace_root: Option<PathBuf>,
) -> Result<Option<RunTapeGuard>, String> {
    if level == RecordingLevel::Off {
        return Ok(None);
    }
    let path = dir.join(TAPE_FILE_NAME);
    let header = TapeHeader::current(
        Some(crate::stdlib::clock::now_wall_ms()),
        None,
        vec![format!("recording={}", level.as_str())],
    );
    let recorder = Arc::new(TapeRecorder::streaming(&path, header)?);
    let mut slot = PROCESS_TAPE.write().expect("run tape lock poisoned");
    if slot.is_some() {
        return Err("a run tape is already recording in this process".to_string());
    }
    *slot = Some(Arc::new(ProcessTape {
        recorder: recorder.clone(),
        level,
        workspace_root,
        next_llm_call: AtomicU64::new(0),
        prompts: Mutex::default(),
    }));
    Ok(Some(RunTapeGuard { recorder, path }))
}

/// Record one prompt asset render and return its content identity. Renders
/// reach the tape only at `recording: full`; the id is computed regardless so
/// the live debugger registry shares it.
pub(crate) fn record_prompt_render(
    template_uri: &str,
    rendered: &str,
    spans: &[crate::stdlib::template::PromptSourceSpan],
) -> String {
    let render_id = prompts::render_id(template_uri, rendered);
    if let Some(tape) = process_tape().filter(|tape| tape.level == RecordingLevel::Full) {
        tape.prompts.lock().expect("prompt ledger poisoned").record(
            &tape.recorder,
            &render_id,
            template_uri,
            rendered,
            spans,
        );
    }
    render_id
}

/// The model calls (1-based) whose request contained this render.
pub(crate) fn render_consumers(render_id: &str) -> Vec<u64> {
    process_tape().map_or_else(Vec::new, |tape| {
        tape.prompts
            .lock()
            .expect("prompt ledger poisoned")
            .consumers(render_id)
    })
}

// ---------------------------------------------------------------------------
// Request surface
// ---------------------------------------------------------------------------

/// The request fields that decide a model's answer. One owner for the digest
/// that recording writes and branch verification checks.
fn request_surface(request: &LlmRequestPayload) -> serde_json::Map<String, serde_json::Value> {
    let call = crate::llm::mock::LlmMockCall::from_request(request);
    let mut surface = serde_json::Map::new();
    let mut put = |key: &str, value: serde_json::Value| {
        if !value.is_null() {
            surface.insert(key.to_string(), value);
        }
    };
    put("model", serde_json::json!(request.model));
    put("api_mode", serde_json::json!(call.api_mode));
    if call.mock_scope != crate::llm::mock::DEFAULT_MOCK_SCOPE {
        put("mock_scope", serde_json::json!(call.mock_scope));
    }
    put("system", serde_json::json!(call.system));
    put("messages", serde_json::json!(call.messages));
    put("tools", serde_json::json!(call.tools));
    put("provider_tools", serde_json::json!(call.provider_tools));
    put("tool_choice", serde_json::json!(call.tool_choice));
    put("thinking", call.thinking.clone());
    put("output_format", call.output_format.clone());
    put("max_tokens", serde_json::json!(call.max_tokens));
    put("temperature", serde_json::json!(call.temperature));
    put("top_p", serde_json::json!(call.top_p));
    put("top_k", serde_json::json!(call.top_k));
    put("seed", serde_json::json!(call.seed));
    put("stop", serde_json::json!(call.stop));
    put("logprobs", serde_json::json!(call.logprobs));
    if !call.logit_bias.is_empty() {
        put("logit_bias", serde_json::json!(call.logit_bias));
    }
    put("min_p", serde_json::json!(call.min_p));
    put(
        "repetition_penalty",
        serde_json::json!(call.repetition_penalty),
    );
    put(
        "frequency_penalty",
        serde_json::json!(call.frequency_penalty),
    );
    put("presence_penalty", serde_json::json!(call.presence_penalty));
    put("prediction", serde_json::json!(call.prediction));
    put("verbosity", serde_json::json!(call.verbosity));
    put("mirostat", serde_json::json!(call.mirostat));
    put(
        "parallel_tool_calls",
        serde_json::json!(call.parallel_tool_calls),
    );
    put(
        "previous_response_id",
        serde_json::json!(call.previous_response_id),
    );
    put("store", serde_json::json!(call.store));
    put("background", serde_json::json!(call.background));
    put("truncation", serde_json::json!(call.truncation));
    put("compact", serde_json::json!(call.compact));
    put("include", serde_json::json!(call.include));
    put("max_tool_calls", serde_json::json!(call.max_tool_calls));
    put("prefill", serde_json::json!(call.prefill));
    surface
}

fn surface_digest(surface: &serde_json::Map<String, serde_json::Value>) -> String {
    content_hash(&crate::canonical_json::to_vec(&serde_json::Value::Object(
        surface.clone(),
    )))
}

/// Record one completed model call on the active tape, if any.
pub(crate) fn record_llm_call(request: &LlmRequestPayload, result: &LlmResult) {
    let Some(recorder) = crate::testbench::tape::active_recorder() else {
        return;
    };
    let tape = process_tape().filter(|tape| Arc::ptr_eq(&tape.recorder, &recorder));
    let level = tape
        .as_ref()
        .map_or(RecordingLevel::Full, |tape| tape.level);
    let call_index = tape
        .as_ref()
        .map(|tape| tape.next_llm_call.fetch_add(1, Ordering::SeqCst) + 1);
    let mut surface = request_surface(request);
    let request_digest = surface_digest(&surface);
    let full = tape.is_some() && level == RecordingLevel::Full;
    let attribution = match (&tape, call_index) {
        (Some(tape), Some(index)) if full => Some(
            tape.prompts
                .lock()
                .expect("prompt ledger poisoned")
                .attribute(&surface, index),
        ),
        _ => None,
    };
    let (request_payload, messages) = if full {
        let messages = match surface.remove("messages") {
            Some(serde_json::Value::Array(messages)) => messages
                .iter()
                .map(|message| recorder.payload_cas(crate::canonical_json::to_vec(message)))
                .collect(),
            _ => Vec::new(),
        };
        let request = recorder.payload_from_bytes(crate::canonical_json::to_vec(
            &serde_json::Value::Object(surface),
        ));
        (Some(request), messages)
    } else {
        (None, Vec::new())
    };
    let response_bytes = if level == RecordingLevel::Metadata {
        serde_json::to_vec(&serde_json::json!({
            "model": result.model,
            "input_tokens": result.input_tokens,
            "output_tokens": result.output_tokens,
            "stop_reason": result.stop_reason,
        }))
    } else {
        serde_json::to_vec(result)
    }
    .unwrap_or_default();
    let response = recorder.payload_from_bytes(response_bytes);
    recorder.record(TapeRecordKind::LlmCall {
        request_digest,
        response,
        call_index,
        request: request_payload,
        messages,
        attribution,
    });
}

// ---------------------------------------------------------------------------
// Tool calls and file effects
// ---------------------------------------------------------------------------

/// What a tool call changed on disk, as an attribution scan sees it.
#[derive(Debug, Default)]
pub struct EffectScan {
    /// Root-relative path to post-call bytes; `None` is a deletion.
    pub changes: Vec<(String, Option<Vec<u8>>)>,
    /// True when the scan could not see some path, so the effect set may be
    /// missing changes.
    pub incomplete: bool,
}

/// Observes the files one tool call changes. Implemented by `harn-hostlib`'s
/// workspace checkpoint scanner, the one owner of attribution scans.
pub trait ToolEffectObserver: Send + Sync {
    fn begin(&self, root: &Path, call_id: &str);
    fn end(&self, root: &Path, call_id: &str) -> EffectScan;
}

/// Install the process's effect observer. The first registration wins.
pub fn register_tool_effect_observer(observer: Box<dyn ToolEffectObserver>) {
    let _ = EFFECT_OBSERVER.set(observer);
}

/// A tool outcome in a shape a branch can hand back to the agent loop.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RecordedToolOutcome {
    pub result: RecordedToolResult,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executor: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handler_outcome: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RecordedToolResult {
    Ok { value: serde_json::Value },
    Categorized { message: String, category: String },
    Thrown { message: String },
}

/// An in-flight tool call on the recording side.
pub(crate) struct ToolCallRecording {
    tape: Arc<ProcessTape>,
    call_id: String,
    observed: bool,
}

/// Begin recording a tool call. `None` when no run tape is recording.
pub(crate) fn begin_tool_call(call_id: &str) -> Option<ToolCallRecording> {
    let tape = process_tape()?;
    let observed = match (tape.level, &tape.workspace_root, EFFECT_OBSERVER.get()) {
        (RecordingLevel::Full, Some(root), Some(observer)) => {
            observer.begin(root, call_id);
            true
        }
        _ => false,
    };
    Some(ToolCallRecording {
        tape,
        call_id: call_id.to_string(),
        observed,
    })
}

pub(crate) fn args_digest(args: &serde_json::Value) -> String {
    content_hash(&crate::canonical_json::to_vec(args))
}

/// Finish recording a tool call. `outcome` is `None` when the dispatch result
/// has no faithful recorded shape; the branch then re-executes the call.
pub(crate) fn finish_tool_call(
    recording: ToolCallRecording,
    tool: &str,
    args: &serde_json::Value,
    outcome: Result<RecordedToolOutcome, String>,
) {
    let ToolCallRecording {
        tape,
        call_id,
        observed,
    } = recording;
    let recorder = &tape.recorder;
    let mut unservable = None;
    let mut effects = Vec::new();
    if observed {
        let root = tape
            .workspace_root
            .as_deref()
            .expect("observed implies root");
        let scan = EFFECT_OBSERVER
            .get()
            .expect("observed implies observer")
            .end(root, &call_id);
        if scan.incomplete {
            unservable = Some("the file-effect scan was incomplete".to_string());
        }
        for (path, content) in scan.changes {
            effects.push(FileEffect {
                path,
                content: content.map(|bytes| recorder.payload_cas(bytes)),
            });
        }
    } else if tape.level == RecordingLevel::Full {
        unservable = Some("no workspace effect observer was installed".to_string());
    }
    let outcome_bytes = match &outcome {
        Ok(outcome) if tape.level == RecordingLevel::Full => {
            serde_json::to_vec(outcome).unwrap_or_default()
        }
        Ok(_) => Vec::new(),
        Err(reason) => {
            unservable.get_or_insert_with(|| reason.clone());
            Vec::new()
        }
    };
    let outcome = recorder.payload_from_bytes(outcome_bytes);
    recorder.record(TapeRecordKind::ToolCall {
        call_id,
        tool: tool.to_string(),
        args_digest: args_digest(args),
        outcome,
        effects,
        unservable,
    });
}

// ---------------------------------------------------------------------------
// Branch replay
// ---------------------------------------------------------------------------

/// Where a branch replays from and where its workspace lives.
#[derive(Clone, Debug)]
pub struct BranchPlan {
    /// The recorded run, loaded from wherever it is stored. Verification
    /// needs only these bytes, never the place that recorded them.
    pub tape: EventTape,
    /// Model calls served from the recording before the branch goes live.
    pub live_after_calls: u64,
    /// The branch's workspace, a copy of the recorded run's starting tree.
    /// Served tool calls re-apply their file effects here.
    pub workspace_root: PathBuf,
}

/// The first point where a branch's prefix stopped matching its recording.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BranchDivergence {
    /// 1-based model call (or the model call whose tool call diverged).
    pub call: u64,
    pub what: String,
    pub detail: String,
}

/// Progress of the active branch, for hosts and receipts.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct BranchReport {
    pub live_after_calls: u64,
    pub served_calls: u64,
    pub served_tool_calls: u64,
    pub reexecuted_tool_calls: u64,
    pub applied_file_effects: u64,
    pub live_calls: u64,
    pub divergence: Option<BranchDivergence>,
}

struct RecordedCall {
    digest: String,
    surface: Option<serde_json::Map<String, serde_json::Value>>,
    result: LlmResult,
}

struct RecordedTool {
    tool: String,
    args_digest: String,
    outcome: Option<RecordedToolOutcome>,
    effects: Vec<(String, Option<Vec<u8>>)>,
    unservable: Option<String>,
}

struct BranchState {
    calls: Vec<RecordedCall>,
    tools: BTreeMap<String, RecordedTool>,
    /// Tool-call ids named by served responses: the only calls the prefix
    /// serves.
    served_tool_ids: BTreeSet<String>,
    /// Recorded cache lookups per (namespace, key), in recorded order.
    cache_lookups:
        BTreeMap<(String, String), std::collections::VecDeque<Option<serde_json::Value>>>,
    workspace_root: PathBuf,
    report: BranchReport,
}

/// Ends the branch's replay when dropped.
pub struct BranchGuard {
    _private: (),
}

impl BranchGuard {
    pub fn report(&self) -> BranchReport {
        branch_report().unwrap_or_default()
    }
}

impl Drop for BranchGuard {
    fn drop(&mut self) {
        *PROCESS_BRANCH.lock().expect("branch lock poisoned") = None;
    }
}

/// The active branch's progress, if a branch is replaying in this process.
pub fn branch_report() -> Option<BranchReport> {
    PROCESS_BRANCH
        .lock()
        .expect("branch lock poisoned")
        .as_ref()
        .map(|state| state.report.clone())
}

/// Load a tape and arm branch replay for this process.
pub fn start_branch(plan: BranchPlan) -> Result<BranchGuard, String> {
    if plan.live_after_calls == 0 {
        return Err("a branch must serve at least one recorded call".to_string());
    }
    let tape = plan.tape;
    if !tape.header.argv.iter().any(|arg| arg == "recording=full") {
        return Err(
            "this tape was not recorded with `recording: full`, so it can be read but not branched"
                .to_string(),
        );
    }
    let mut calls = Vec::new();
    let mut tools = BTreeMap::new();
    let mut cache_lookups: BTreeMap<_, std::collections::VecDeque<_>> = BTreeMap::new();
    for record in &tape.records {
        match &record.kind {
            TapeRecordKind::CacheLookup {
                namespace,
                key,
                value,
            } => {
                let value = match value {
                    Some(payload) => Some(
                        serde_json::from_slice(&tape.resolve_payload(payload)?)
                            .map_err(|err| format!("tape cache value: {err}"))?,
                    ),
                    None => None,
                };
                cache_lookups
                    .entry((namespace.clone(), key.clone()))
                    .or_default()
                    .push_back(value);
            }
            TapeRecordKind::LlmCall {
                request_digest,
                response,
                call_index: Some(_),
                request,
                messages,
                ..
            } => {
                let result: LlmResult = serde_json::from_slice(&tape.resolve_payload(response)?)
                    .map_err(|err| format!("tape model response: {err}"))?;
                calls.push(RecordedCall {
                    digest: request_digest.clone(),
                    surface: recorded_surface(&tape, request.as_ref(), messages),
                    result,
                });
            }
            TapeRecordKind::ToolCall {
                call_id,
                tool,
                args_digest,
                outcome,
                effects,
                unservable,
            } => {
                let bytes = tape.resolve_payload(outcome)?;
                let outcome = if bytes.is_empty() {
                    None
                } else {
                    Some(
                        serde_json::from_slice(&bytes)
                            .map_err(|err| format!("tape tool outcome: {err}"))?,
                    )
                };
                let mut resolved = Vec::new();
                for effect in effects {
                    let content = match &effect.content {
                        Some(payload) => Some(tape.resolve_payload(payload)?),
                        None => None,
                    };
                    resolved.push((effect.path.clone(), content));
                }
                tools.insert(
                    call_id.clone(),
                    RecordedTool {
                        tool: tool.clone(),
                        args_digest: args_digest.clone(),
                        outcome,
                        effects: resolved,
                        unservable: unservable.clone(),
                    },
                );
            }
            _ => {}
        }
    }
    if usize::try_from(plan.live_after_calls).map_or(true, |k| k > calls.len()) {
        return Err(format!(
            "the tape has {} model calls; it cannot serve {}",
            calls.len(),
            plan.live_after_calls
        ));
    }
    let mut slot = PROCESS_BRANCH.lock().expect("branch lock poisoned");
    if slot.is_some() {
        return Err("a branch is already replaying in this process".to_string());
    }
    *slot = Some(BranchState {
        calls,
        tools,
        served_tool_ids: BTreeSet::new(),
        cache_lookups,
        workspace_root: plan.workspace_root,
        report: BranchReport {
            live_after_calls: plan.live_after_calls,
            ..BranchReport::default()
        },
    });
    Ok(BranchGuard { _private: () })
}

fn recorded_surface(
    tape: &EventTape,
    request: Option<&TapePayload>,
    messages: &[TapePayload],
) -> Option<serde_json::Map<String, serde_json::Value>> {
    let bytes = tape.resolve_payload(request?).ok()?;
    let mut surface: serde_json::Map<String, serde_json::Value> =
        serde_json::from_slice(&bytes).ok()?;
    let messages = messages
        .iter()
        .map(|payload| {
            tape.resolve_payload(payload)
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        })
        .collect::<Option<Vec<serde_json::Value>>>()?;
    if !messages.is_empty() {
        surface.insert("messages".to_string(), serde_json::Value::Array(messages));
    }
    Some(surface)
}

/// Name the first field where two request surfaces differ.
fn first_difference(
    recorded: &serde_json::Map<String, serde_json::Value>,
    replayed: &serde_json::Map<String, serde_json::Value>,
) -> String {
    let excerpt = |value: Option<&serde_json::Value>| {
        value.map_or_else(
            || "<absent>".to_string(),
            |value| crate::text::truncate_end(&value.to_string(), 400),
        )
    };
    let keys: BTreeSet<&String> = recorded.keys().chain(replayed.keys()).collect();
    for key in keys {
        let (a, b) = (recorded.get(key), replayed.get(key));
        if a == b {
            continue;
        }
        if let (Some(serde_json::Value::Array(a)), Some(serde_json::Value::Array(b))) = (a, b) {
            for index in 0..a.len().max(b.len()) {
                if a.get(index) != b.get(index) {
                    return format!(
                        "{key}[{index}] differs: recorded {} vs replayed {}",
                        excerpt(a.get(index)),
                        excerpt(b.get(index))
                    );
                }
            }
        }
        return format!(
            "`{key}` differs: recorded {} vs replayed {}",
            excerpt(a),
            excerpt(b)
        );
    }
    "the digests differ but the surfaces match".to_string()
}

const DIVERGED_FIX: &str = "the program built a different request than the recording, so the \
branch cannot prove its prefix; record a fresh tape with this build, or branch from an earlier call";

fn divergence_error(divergence: &BranchDivergence) -> crate::value::VmError {
    crate::value::VmError::Runtime(format!(
        "branch prefix diverged at model call {}: {}: {}. {DIVERGED_FIX}",
        divergence.call, divergence.what, divergence.detail
    ))
}

/// Serve a model call from the branch prefix. `None` means the call is live.
/// After a divergence every later call fails closed, so a branch with an
/// unproven prefix never reaches a provider.
pub(crate) fn serve_llm_call(
    request: &LlmRequestPayload,
) -> Option<Result<LlmResult, crate::value::VmError>> {
    let mut guard = PROCESS_BRANCH.lock().expect("branch lock poisoned");
    let state = guard.as_mut()?;
    if let Some(divergence) = &state.report.divergence {
        return Some(Err(divergence_error(divergence)));
    }
    if state.report.served_calls >= state.report.live_after_calls {
        state.report.live_calls += 1;
        return None;
    }
    let index = usize::try_from(state.report.served_calls).expect("bounded by calls");
    let call_number = state.report.served_calls + 1;
    let recorded = &state.calls[index];
    let surface = request_surface(request);
    if surface_digest(&surface) != recorded.digest {
        let detail = recorded.surface.as_ref().map_or_else(
            || "the recorded request body was not kept".to_string(),
            |recorded| first_difference(recorded, &surface),
        );
        let divergence = BranchDivergence {
            call: call_number,
            what: "request".to_string(),
            detail,
        };
        let error = divergence_error(&divergence);
        state.report.divergence = Some(divergence);
        return Some(Err(error));
    }
    let mut result = recorded.result.clone();
    result.telemetry = Box::new(ProviderTelemetry::mock_replay(Some(0.0)));
    for call in &result.tool_calls {
        if let Some(id) = call.get("id").and_then(serde_json::Value::as_str) {
            state.served_tool_ids.insert(id.to_string());
        }
    }
    state.report.served_calls += 1;
    Some(Ok(result))
}

/// One persistent cache lookup. In a branch prefix the recorded answer is
/// returned without reading the cache; a lookup the recording never made
/// diverges. Otherwise `live` answers and, when recording, the answer is
/// taped.
pub(crate) fn cache_lookup(
    namespace: &str,
    key: &str,
    live: impl FnOnce() -> Result<Option<serde_json::Value>, crate::value::VmError>,
) -> Result<Option<serde_json::Value>, crate::value::VmError> {
    {
        let mut guard = PROCESS_BRANCH.lock().expect("branch lock poisoned");
        if let Some(state) = guard.as_mut() {
            if let Some(divergence) = &state.report.divergence {
                return Err(divergence_error(divergence));
            }
            if state.report.served_calls < state.report.live_after_calls {
                let slot = (namespace.to_string(), key.to_string());
                let recorded = state
                    .cache_lookups
                    .get_mut(&slot)
                    .and_then(|q| q.pop_front());
                let Some(value) = recorded else {
                    let divergence = BranchDivergence {
                        call: state.report.served_calls + 1,
                        what: "cache_lookup".to_string(),
                        detail: format!(
                            "the program read cache key `{key}` in `{namespace}`, which the recording never read"
                        ),
                    };
                    let error = divergence_error(&divergence);
                    state.report.divergence = Some(divergence);
                    return Err(error);
                };
                return Ok(value);
            }
        }
    }
    let value = live()?;
    if let Some(tape) = process_tape().filter(|tape| tape.level == RecordingLevel::Full) {
        let payload = value.as_ref().map(|value| {
            tape.recorder
                .payload_from_bytes(crate::canonical_json::to_vec(value))
        });
        tape.recorder.record(TapeRecordKind::CacheLookup {
            namespace: namespace.to_string(),
            key: key.to_string(),
            value: payload,
        });
    }
    Ok(value)
}

/// What a branch does with one tool dispatch.
pub(crate) enum ToolServe {
    /// Not part of a served prefix: run it.
    Live,
    /// Served from the recording; effects already applied.
    Served(RecordedToolOutcome),
    /// Prefix call the recording cannot serve: run it, then compare.
    Reexecute,
    /// The prefix diverged; refuse the call.
    Diverged(crate::value::VmError),
}

pub(crate) fn serve_tool_call(call_id: &str, tool: &str, args: &serde_json::Value) -> ToolServe {
    let mut guard = PROCESS_BRANCH.lock().expect("branch lock poisoned");
    let Some(state) = guard.as_mut() else {
        return ToolServe::Live;
    };
    if let Some(divergence) = &state.report.divergence {
        return ToolServe::Diverged(divergence_error(divergence));
    }
    if !state.served_tool_ids.contains(call_id) {
        return ToolServe::Live;
    }
    let call = state.report.served_calls;
    let diverge = |state: &mut BranchState, what: &str, detail: String| {
        let divergence = BranchDivergence {
            call,
            what: what.to_string(),
            detail,
        };
        let error = divergence_error(&divergence);
        state.report.divergence = Some(divergence);
        ToolServe::Diverged(error)
    };
    let Some(recorded) = state.tools.get(call_id) else {
        return diverge(
            state,
            "tool call",
            format!("`{tool}` call {call_id} has no recorded result"),
        );
    };
    if recorded.tool != tool || recorded.args_digest != args_digest(args) {
        let detail = format!(
            "call {call_id} recorded `{}` with different arguments than replayed `{tool}`",
            recorded.tool
        );
        return diverge(state, "tool call", detail);
    }
    let Some(outcome) = recorded
        .outcome
        .clone()
        .filter(|_| recorded.unservable.is_none())
    else {
        state.report.reexecuted_tool_calls += 1;
        return ToolServe::Reexecute;
    };
    let effects = recorded.effects.clone();
    let root = state.workspace_root.clone();
    for (path, content) in &effects {
        if let Err(error) = apply_effect(&root, path, content.as_deref()) {
            return diverge(state, "file effect", error);
        }
        state.report.applied_file_effects += 1;
    }
    state.report.served_tool_calls += 1;
    ToolServe::Served(outcome)
}

/// Compare a re-executed prefix tool call with its recording. A different
/// outcome is a divergence: the prefix is no longer the recorded one.
pub(crate) fn check_reexecuted_tool_call(
    call_id: &str,
    outcome: &Result<RecordedToolOutcome, String>,
) {
    let mut guard = PROCESS_BRANCH.lock().expect("branch lock poisoned");
    let Some(state) = guard.as_mut() else {
        return;
    };
    let Some(recorded) = state.tools.get(call_id) else {
        return;
    };
    let same = matches!((&recorded.outcome, outcome), (Some(a), Ok(b)) if a == b);
    if !same && state.report.divergence.is_none() {
        state.report.divergence = Some(BranchDivergence {
            call: state.report.served_calls,
            what: "re-executed tool call".to_string(),
            detail: format!(
                "`{}` call {call_id} returned a different result than recorded ({})",
                recorded.tool,
                recorded
                    .unservable
                    .as_deref()
                    .unwrap_or("re-executed because it could not be served")
            ),
        });
    }
}

fn apply_effect(root: &Path, path: &str, content: Option<&[u8]>) -> Result<(), String> {
    let relative = Path::new(path);
    if relative.is_absolute()
        || relative
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(format!("refusing effect outside the workspace: {path}"));
    }
    let target = root.join(relative);
    match content {
        Some(bytes) => {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|err| format!("mkdir {}: {err}", parent.display()))?;
            }
            std::fs::write(&target, bytes)
                .map_err(|err| format!("write {}: {err}", target.display()))
        }
        None => match std::fs::remove_file(&target) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(format!("delete {}: {err}", target.display())),
        },
    }
}

#[path = "run_tape_prompts.rs"]
mod prompts;

#[cfg(test)]
#[path = "run_tape_tests.rs"]
mod tests;
