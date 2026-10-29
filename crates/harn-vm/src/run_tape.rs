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
//! Tool calls named by a served response return their recorded result and
//! re-apply their recorded file effects, so the workspace evolves exactly as
//! it did and nothing in the prefix re-executes. Any other effect in the
//! prefix is served from the tape or refused (see `prefix`). From call
//! `live_after_calls + 1` everything is live.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use serde::{Deserialize, Serialize};

use crate::llm::api::{LlmRequestPayload, LlmResult};
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
    workspace: Option<TapeWorkspace>,
    next_llm_call: AtomicU64,
    prompts: Mutex<prompts::PromptLedger>,
}

impl ProcessTape {
    fn workspace_root(&self) -> Option<&Path> {
        self.workspace
            .as_ref()
            .map(|workspace| workspace.root.as_path())
    }
}

/// The workspace a run's tool calls change, and the observer that sees each
/// call's file effects. One value, so a tape cannot claim a workspace whose
/// effects nothing recorded.
pub struct TapeWorkspace {
    pub root: PathBuf,
    pub observer: Box<dyn ToolEffectObserver>,
}

/// How a production run tape was recorded, in its header. A branch reads
/// this, not the argv, to decide whether it may replay the tape.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunTapeHeader {
    pub recording: RecordingLevel,
    /// True when a workspace effect observer recorded each tool call's file
    /// changes. Without it a served tool call cannot restore the workspace.
    pub effects_observed: bool,
}

static PROCESS_TAPE: RwLock<Option<Arc<ProcessTape>>> = RwLock::new(None);
static PROCESS_BRANCH: Mutex<Option<BranchState>> = Mutex::new(None);
/// Mirrors "a branch is replaying its prefix" so dispatch hot paths check one
/// atomic instead of taking the branch lock.
static PREFIX_ACTIVE: AtomicBool = AtomicBool::new(false);
/// Mirrors "a `recording: full` tape is active", for the same reason.
static RECORDING_FULL: AtomicBool = AtomicBool::new(false);

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
    process_tape()
        .filter(|tape| tape.level == RecordingLevel::Full)
        .map(|tape| tape.recorder.clone())
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
            RECORDING_FULL.store(false, Ordering::Release);
        }
    }
}

/// Start recording this process's run into `dir/run.tape`. Returns `None` for
/// [`RecordingLevel::Off`]. Without a `workspace` tool results are recorded
/// but their file effects are not, so the tape can be read but not branched.
pub fn start_recording(
    dir: &Path,
    level: RecordingLevel,
    workspace: Option<TapeWorkspace>,
) -> Result<Option<RunTapeGuard>, String> {
    if level == RecordingLevel::Off {
        return Ok(None);
    }
    let path = dir.join(TAPE_FILE_NAME);
    let mut header =
        TapeHeader::current(Some(crate::stdlib::clock::now_wall_ms()), None, Vec::new());
    header.version = crate::testbench::tape::TAPE_FORMAT_VERSION;
    header.run = Some(RunTapeHeader {
        recording: level,
        effects_observed: workspace.is_some(),
    });
    let mut slot = PROCESS_TAPE.write().expect("run tape lock poisoned");
    if slot.is_some() {
        return Err("a run tape is already recording in this process".to_string());
    }
    let recorder = Arc::new(TapeRecorder::streaming(&path, header)?);
    RECORDING_FULL.store(level == RecordingLevel::Full, Ordering::Release);
    *slot = Some(Arc::new(ProcessTape {
        recorder: recorder.clone(),
        level,
        workspace,
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

/// Request fields the digest leaves out, each because it legitimately
/// differs between two runs that ask the model the same thing:
/// - `api_key`: a credential, which must never reach a tape;
/// - `session_id`: minted per run by the host.
const REQUEST_DIGEST_DENYLIST: &[&str] = &["api_key", "session_id"];

/// The request a model call's digest covers: every field the payload
/// serializes minus [`REQUEST_DIGEST_DENYLIST`], plus the fields it does not
/// serialize but that still shape the run. The one definition of "the same
/// request" that recording writes, the fidelity oracle compares and branch
/// verification checks. A new payload field is covered by default.
fn request_surface(request: &LlmRequestPayload) -> serde_json::Map<String, serde_json::Value> {
    let mut whole = match serde_json::to_value(request) {
        Ok(serde_json::Value::Object(map)) => map,
        _ => serde_json::Map::new(),
    };
    for key in REQUEST_DIGEST_DENYLIST {
        whole.remove(*key);
    }
    for (key, value) in [
        ("done_sentinel", &request.done_sentinel),
        ("done_sentinel_form", &request.done_sentinel_form),
        ("mock_scope", &request.mock_scope),
    ] {
        whole.insert(key.to_string(), serde_json::json!(value));
    }
    whole
}

fn surface_digest(surface: &serde_json::Map<String, serde_json::Value>) -> String {
    content_hash(&crate::canonical_json::to_vec(&serde_json::Value::Object(
        surface.clone(),
    )))
}

/// Record one completed model call on the active tape, if any.
pub(crate) fn record_llm_call(request: &LlmRequestPayload, result: &LlmResult) {
    // A metadata tape is not an axis recorder, so look it up directly.
    let Some(recorder) = crate::testbench::tape::active_recorder()
        .or_else(|| process_tape().map(|tape| tape.recorder.clone()))
    else {
        return;
    };
    let tape = process_tape().filter(|tape| Arc::ptr_eq(&tape.recorder, &recorder));
    let level = tape
        .as_ref()
        .map_or(RecordingLevel::Full, |tape| tape.level);
    let call_index = tape
        .as_ref()
        .map(|tape| tape.next_llm_call.fetch_add(1, Ordering::SeqCst) + 1);
    let surface = request_surface(request);
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
        let mut whole = surface;
        let messages = match whole.remove("messages") {
            Some(serde_json::Value::Array(messages)) => messages
                .iter()
                .map(|message| recorder.payload_cas(crate::canonical_json::to_vec(message)))
                .collect(),
            _ => Vec::new(),
        };
        let request = recorder.payload_from_bytes(crate::canonical_json::to_vec(
            &serde_json::Value::Object(whole),
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

/// What one agent tool dispatch returned to the agent loop, after policy,
/// approval, hooks and execution: the value a branch hands back in their
/// place.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RecordedToolResult {
    Ok { value: serde_json::Value },
    Categorized { message: String, category: String },
    Thrown { message: String },
    ThrownValue { value: serde_json::Value },
    Runtime { message: String },
}

/// An in-flight tool call on the recording side.
pub(crate) struct ToolCallRecording {
    tape: Arc<ProcessTape>,
    call_id: String,
    tool: String,
    args_digest: String,
    after_calls: u64,
    watch: EffectWatch,
}

enum EffectWatch {
    Scanned,
    ReadOnly,
    Unobserved,
}

/// Begin recording a tool call. `None` when no run tape is recording. A tool
/// declared read-only skips the two workspace scans; its effect set is empty.
pub(crate) fn begin_tool_call(
    call_id: &str,
    tool: &str,
    args: &serde_json::Value,
    read_only: bool,
) -> Option<ToolCallRecording> {
    let tape = process_tape()?;
    let watch = match (tape.level, &tape.workspace) {
        (RecordingLevel::Full, _) if read_only => EffectWatch::ReadOnly,
        (RecordingLevel::Full, Some(workspace)) => {
            workspace.observer.begin(&workspace.root, call_id);
            EffectWatch::Scanned
        }
        _ => EffectWatch::Unobserved,
    };
    Some(ToolCallRecording {
        after_calls: tape.next_llm_call.load(Ordering::SeqCst),
        tape,
        call_id: call_id.to_string(),
        tool: tool.to_string(),
        args_digest: args_digest(args),
        watch,
    })
}

pub(crate) fn args_digest(args: &serde_json::Value) -> String {
    content_hash(&crate::canonical_json::to_vec(args))
}

/// Finish recording a tool call. `Err` names why the dispatch result has no
/// faithful recorded shape; a branch then refuses to serve the call.
pub(crate) fn finish_tool_call(
    recording: ToolCallRecording,
    outcome: Result<RecordedToolResult, String>,
) {
    let ToolCallRecording {
        tape,
        call_id,
        tool,
        args_digest,
        after_calls,
        watch,
    } = recording;
    let recorder = &tape.recorder;
    let full = tape.level == RecordingLevel::Full;
    let mut unservable = None;
    let mut effects = Vec::new();
    match watch {
        EffectWatch::Scanned => {
            let workspace = tape.workspace.as_ref().expect("a scan implies a workspace");
            let scan = workspace.observer.end(&workspace.root, &call_id);
            if scan.incomplete {
                unservable = Some("the file-effect scan was incomplete".to_string());
            }
            for (path, content) in scan.changes {
                effects.push(FileEffect {
                    path,
                    content: content.map(|bytes| recorder.payload_cas(bytes)),
                });
            }
        }
        EffectWatch::ReadOnly => {}
        EffectWatch::Unobserved if full => {
            unservable = Some("no workspace effect observer was installed".to_string());
        }
        EffectWatch::Unobserved => {}
    }
    let outcome_bytes = match &outcome {
        Ok(outcome) if full => serde_json::to_vec(outcome).unwrap_or_default(),
        Ok(_) => Vec::new(),
        // A metadata tape keeps no tool text, error text included.
        Err(reason) if full => {
            unservable.get_or_insert_with(|| reason.clone());
            Vec::new()
        }
        Err(_) => {
            unservable.get_or_insert_with(|| "unrecordable result".to_string());
            Vec::new()
        }
    };
    let outcome = recorder.payload_from_bytes(outcome_bytes);
    recorder.record(TapeRecordKind::ToolCall {
        call_id,
        tool,
        args_digest,
        after_calls,
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
    pub applied_file_effects: u64,
    /// Clock, random and env reads answered from the recording.
    pub served_reads: u64,
    pub live_calls: u64,
    /// Prefix cache reads of keys the recording never read, answered live.
    pub live_cache_reads: u64,
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
    outcome: Option<RecordedToolResult>,
    effects: Vec<(String, Option<Vec<u8>>)>,
    unservable: Option<String>,
}

struct PrefixTool {
    call_id: String,
    recorded: RecordedTool,
    used: bool,
}

type RecordedLookups =
    BTreeMap<(u64, String, String), std::collections::VecDeque<Option<serde_json::Value>>>;

struct BranchState {
    calls: Vec<RecordedCall>,
    /// Prefix tool calls by how many model calls preceded them.
    tools: BTreeMap<u64, Vec<PrefixTool>>,
    /// Prefix cache lookups and recorded reads per (model calls before,
    /// namespace, key), in recorded order.
    cache_lookups: RecordedLookups,
    /// Every (namespace, key) the recording read anywhere in the run.
    recorded_keys: BTreeSet<(String, String)>,
    /// Each runtime store's contents when the recorded run opened it.
    store_seeds: BTreeMap<String, Option<serde_json::Value>>,
    /// Set by the first live model call; nothing is served after it.
    live: bool,
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

    /// The final report. A run that ended before consuming its whole prefix
    /// (model calls, tool calls or recorded reads) diverged: it did not
    /// replay the recording, however far it got.
    pub fn finish(&self) -> BranchReport {
        let mut guard = PROCESS_BRANCH.lock().expect("branch lock poisoned");
        let Some(state) = guard.as_mut() else {
            return BranchReport::default();
        };
        if state.report.divergence.is_none() {
            state.report.divergence = under_run(state);
        }
        state.report.clone()
    }
}

fn under_run(state: &BranchState) -> Option<BranchDivergence> {
    let served = state.report.served_calls;
    let k = state.report.live_after_calls;
    if served < k {
        return Some(BranchDivergence {
            call: served + 1,
            what: "under-run prefix".to_string(),
            detail: format!("the run ended after {served} of {k} recorded model calls"),
        });
    }
    if let Some((after, tool)) = state.tools.iter().find_map(|(after, tools)| {
        tools
            .iter()
            .find(|tool| !tool.used)
            .map(|tool| (*after, tool))
    }) {
        return Some(BranchDivergence {
            call: after,
            what: "under-run prefix".to_string(),
            detail: format!(
                "recorded `{}` call {} was never made",
                tool.recorded.tool, tool.call_id
            ),
        });
    }
    state
        .cache_lookups
        .iter()
        .find(|(_, queue)| !queue.is_empty())
        .map(|((after, namespace, key), _)| BranchDivergence {
            call: *after,
            what: "under-run prefix".to_string(),
            detail: format!("recorded read of `{key}` in `{namespace}` was never made"),
        })
}

impl Drop for BranchGuard {
    fn drop(&mut self) {
        *PROCESS_BRANCH.lock().expect("branch lock poisoned") = None;
        PREFIX_ACTIVE.store(false, Ordering::Release);
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
    let k = plan.live_after_calls;
    match &tape.header.run {
        Some(RunTapeHeader {
            recording: RecordingLevel::Full,
            effects_observed: true,
        }) => {}
        Some(RunTapeHeader {
            recording: RecordingLevel::Full,
            effects_observed: false,
        }) => {
            return Err(
                "this tape was recorded without a workspace effect observer, so its tool calls \
                 cannot restore the workspace"
                    .to_string(),
            )
        }
        _ => {
            return Err(
                "this tape was not recorded with `recording: full`, so it can be read but not \
                 branched"
                    .to_string(),
            )
        }
    }
    let mut calls = Vec::new();
    let mut tools: BTreeMap<u64, Vec<PrefixTool>> = BTreeMap::new();
    let mut cache_lookups: RecordedLookups = BTreeMap::new();
    let mut recorded_keys = BTreeSet::new();
    let mut store_seeds = BTreeMap::new();
    let decode = |payload: &Option<TapePayload>| -> Result<Option<serde_json::Value>, String> {
        payload
            .as_ref()
            .map(|payload| {
                serde_json::from_slice(&tape.resolve_payload(payload)?)
                    .map_err(|err| format!("tape recorded value: {err}"))
            })
            .transpose()
    };
    for record in &tape.records {
        match &record.kind {
            TapeRecordKind::CacheLookup {
                namespace,
                key,
                value,
                ..
            } if namespace == prefix::STORE_SEED_NAMESPACE => {
                let seed = decode(value)?;
                store_seeds.entry(key.clone()).or_insert(seed);
            }
            TapeRecordKind::CacheLookup {
                namespace,
                key,
                after_calls,
                value,
            } => {
                recorded_keys.insert((namespace.clone(), key.clone()));
                if *after_calls <= k {
                    cache_lookups
                        .entry((*after_calls, namespace.clone(), key.clone()))
                        .or_default()
                        .push_back(decode(value)?);
                }
            }
            TapeRecordKind::LlmCall {
                request_digest,
                response,
                call_index: Some(_),
                request,
                messages,
                ..
            } => {
                let digest = request_digest.clone();
                let result: LlmResult = serde_json::from_slice(&tape.resolve_payload(response)?)
                    .map_err(|err| format!("tape model response: {err}"))?;
                calls.push(RecordedCall {
                    digest,
                    surface: recorded_surface(&tape, request.as_ref(), messages),
                    result,
                });
            }
            TapeRecordKind::ToolCall {
                call_id,
                tool,
                args_digest,
                after_calls,
                outcome,
                effects,
                unservable,
            } if *after_calls <= k => {
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
                tools.entry(*after_calls).or_default().push(PrefixTool {
                    call_id: call_id.clone(),
                    recorded: RecordedTool {
                        tool: tool.clone(),
                        args_digest: args_digest.clone(),
                        outcome,
                        effects: resolved,
                        unservable: unservable.clone(),
                    },
                    used: false,
                });
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
        cache_lookups,
        recorded_keys,
        store_seeds,
        live: false,
        workspace_root: plan.workspace_root,
        report: BranchReport {
            live_after_calls: plan.live_after_calls,
            ..BranchReport::default()
        },
    });
    PREFIX_ACTIVE.store(true, Ordering::Release);
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

const DIVERGED_FIX: &str = "the replayed prefix did something the recording did not, so the \
branch cannot prove it; record a fresh tape with this build, or branch from an earlier call";

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
        state.live = true;
        PREFIX_ACTIVE.store(false, Ordering::Release);
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
    // Keep the recorded cost and usage, flagged as served, so a branch's
    // accounting says what its prefix cost without counting it as new spend.
    let mut result = recorded.result.clone();
    result.telemetry.source = crate::llm::api::telemetry_source::BRANCH_SERVED.to_string();
    state.report.served_calls += 1;
    Some(Ok(result))
}

/// One persistent cache lookup. In a branch prefix the recorded answer at
/// the same point in the run is returned without reading the cache. A key
/// the recording read only elsewhere diverges. A key it never read is
/// run-scoped state, such as a key built from this run's session id, so it
/// is read live. Either way a recording tape keeps the answer.
pub(crate) fn cache_lookup(
    namespace: &str,
    key: &str,
    live: impl FnOnce() -> Result<Option<serde_json::Value>, crate::value::VmError>,
) -> Result<Option<serde_json::Value>, crate::value::VmError> {
    recorded_lookup(namespace, key, true, live)
}

/// Serve `key` from the recording at this point of a branch prefix, or read
/// it live and record it. `unknown_is_live` reads a key the recording never
/// read live instead of diverging.
fn recorded_lookup(
    namespace: &str,
    key: &str,
    unknown_is_live: bool,
    live: impl FnOnce() -> Result<Option<serde_json::Value>, crate::value::VmError>,
) -> Result<Option<serde_json::Value>, crate::value::VmError> {
    let served = {
        let mut guard = PROCESS_BRANCH.lock().expect("branch lock poisoned");
        match guard.as_mut() {
            Some(state) if state.report.divergence.is_some() => {
                return Err(divergence_error(
                    state.report.divergence.as_ref().expect("checked"),
                ));
            }
            Some(state) if !state.live => {
                let slot = (
                    state.report.served_calls,
                    namespace.to_string(),
                    key.to_string(),
                );
                let recorded = state
                    .cache_lookups
                    .get_mut(&slot)
                    .and_then(|q| q.pop_front());
                let known = state.recorded_keys.contains(&(slot.1, slot.2));
                match recorded {
                    Some(value) => {
                        if !unknown_is_live {
                            state.report.served_reads += 1;
                        }
                        Some(value)
                    }
                    None if !known && unknown_is_live => {
                        state.report.live_cache_reads += 1;
                        None
                    }
                    None => {
                        let divergence = BranchDivergence {
                            call: state.report.served_calls + 1,
                            what: "cache_lookup".to_string(),
                            detail: format!(
                            "the program read `{key}` in `{namespace}` where the recording did not"
                        ),
                        };
                        let error = divergence_error(&divergence);
                        state.report.divergence = Some(divergence);
                        return Err(error);
                    }
                }
            }
            _ => None,
        }
    };
    let value = match served {
        Some(value) => value,
        None => live()?,
    };
    if let Some(tape) = process_tape().filter(|tape| tape.level == RecordingLevel::Full) {
        let payload = value.as_ref().map(|value| {
            tape.recorder
                .payload_from_bytes(crate::canonical_json::to_vec(value))
        });
        tape.recorder.record(TapeRecordKind::CacheLookup {
            namespace: namespace.to_string(),
            key: key.to_string(),
            after_calls: tape.next_llm_call.load(Ordering::SeqCst),
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
    Served(RecordedToolResult),
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
    if state.live {
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
    // The same tool and arguments at the same point in the run, preferring
    // the same provider id: text-format calls get fresh ids on every parse.
    let digest = args_digest(args);
    let candidates = state.tools.get(&call).map_or(&[][..], Vec::as_slice);
    let same_call = |entry: &PrefixTool| {
        !entry.used && entry.recorded.tool == tool && entry.recorded.args_digest == digest
    };
    let index = candidates
        .iter()
        .position(|entry| same_call(entry) && entry.call_id == call_id)
        .or_else(|| candidates.iter().position(same_call));
    let Some(index) = index else {
        return diverge(
            state,
            "tool call",
            format!(
                "the recording made no `{tool}` call with these arguments after model call {call}"
            ),
        );
    };
    let entry = &mut state.tools.get_mut(&call).expect("matched above")[index];
    entry.used = true;
    let recorded = &entry.recorded;
    // Running it instead would repeat its effects; nothing in a prefix runs.
    let outcome = match (&recorded.outcome, &recorded.unservable) {
        (Some(outcome), None) => outcome.clone(),
        (_, reason) => {
            let reason = reason
                .clone()
                .unwrap_or_else(|| "no recorded result".to_string());
            return diverge(
                state,
                "unservable tool call",
                format!("the recording cannot serve `{tool}` call {call_id}: {reason}"),
            );
        }
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

fn apply_effect(root: &Path, path: &str, content: Option<&[u8]>) -> Result<(), String> {
    let relative = Path::new(path);
    if !relative
        .components()
        .all(|part| matches!(part, std::path::Component::Normal(_)))
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

#[path = "run_tape_prefix.rs"]
mod prefix;
pub(crate) use prefix::{
    prefix_active, record_read, records_reads, refuse_in_prefix, serve_read, store_seed,
    taped_random,
};

#[cfg(test)]
#[path = "run_tape_tests.rs"]
mod tests;
