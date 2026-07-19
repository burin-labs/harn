use crate::value::VmDictExt;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};

use super::api::{LlmResult, ProviderTelemetry, RawProviderToolCall};
use super::mock_store::{MockQueue, QueueMatch};
use crate::orchestration::ToolCallRecord;
use crate::value::{ErrorCategory, VmError, VmValue};

/// LLM replay mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmReplayMode {
    Off,
    Record,
    Replay,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum CliLlmMockMode {
    #[default]
    Off,
    Replay,
    Record,
}

#[derive(Default)]
struct CliLlmMockState {
    mode: CliLlmMockMode,
    queue: MockQueue,
    recordings: Vec<LlmMock>,
}

static CLI_LLM_MOCK_NEXT_SCOPE: AtomicU64 = AtomicU64::new(1);
static CLI_LLM_MOCK_SCOPES: LazyLock<Mutex<BTreeMap<u64, CliLlmMockState>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));

struct CliLlmMockLease(u64);

impl Drop for CliLlmMockLease {
    fn drop(&mut self) {
        cli_llm_mock_scopes().remove(&self.0);
    }
}

/// Categorized error injected by a mock. When present, the mock
/// short-circuits the provider call and surfaces as
/// `VmError::CategorizedError`, so `llm_call` throws and
/// `llm_call_safe` populates its `error` envelope.
#[derive(Clone, Debug)]
pub struct MockError {
    pub category: ErrorCategory,
    pub message: String,
    pub status: Option<u16>,
    pub kind: Option<String>,
    pub reason: Option<String>,
    /// Optional retry hint. Provider-envelope mocks put this directly
    /// on the thrown dict; legacy category-only mocks embed it in the
    /// message so the live-provider parser path still exercises the
    /// same extraction code.
    pub retry_after_ms: Option<u64>,
}

impl MockError {
    fn has_provider_envelope(&self) -> bool {
        self.status.is_some() || self.kind.is_some() || self.reason.is_some()
    }
}

pub(crate) fn build_mock_error(
    category: Option<String>,
    message: Option<String>,
    status: Option<u16>,
    kind: Option<String>,
    reason: Option<String>,
    retry_after_ms: Option<u64>,
) -> Result<MockError, String> {
    if retry_after_ms.is_some_and(|ms| ms > i64::MAX as u64) {
        return Err("error.retry_after_ms must fit in a signed 64-bit integer".to_string());
    }
    let kind = match kind {
        Some(value) if value.trim().is_empty() => None,
        Some(value) => {
            let normalized = value.trim().to_ascii_lowercase();
            if super::api::LlmErrorKind::parse(&normalized).is_none() {
                return Err(format!("unknown error kind `{value}`"));
            }
            Some(normalized)
        }
        None => None,
    };
    let reason = reason.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    });
    let category_was_provided = category.is_some();
    let category = match category {
        Some(value) if value.trim().is_empty() => {
            return Err("error.category must not be empty".to_string());
        }
        Some(value) => {
            let normalized = value.trim().to_ascii_lowercase();
            let category = ErrorCategory::parse(&normalized);
            if category.as_str() != normalized {
                return Err(format!("unknown error category `{value}`"));
            }
            category
        }
        None => infer_mock_error_category(status, kind.as_deref(), reason.as_deref()),
    };
    if !category_was_provided && kind.is_none() && status.is_none() && reason.is_none() {
        return Err(
            "error.category is required unless error.status, error.kind, or error.reason is set"
                .to_string(),
        );
    }
    Ok(MockError {
        category,
        message: message.unwrap_or_else(|| {
            default_mock_error_message(status, kind.as_deref(), reason.as_deref())
        }),
        status,
        kind,
        reason,
        retry_after_ms,
    })
}

pub(crate) fn validate_mock_error_status(status: i64) -> Result<u16, String> {
    let status = u16::try_from(status)
        .map_err(|_| "error.status must be an HTTP status code".to_string())?;
    reqwest::StatusCode::from_u16(status)
        .map_err(|_| "error.status must be an HTTP status code".to_string())?;
    Ok(status)
}

fn infer_mock_error_category(
    status: Option<u16>,
    kind: Option<&str>,
    reason: Option<&str>,
) -> ErrorCategory {
    if let Some(status) = status {
        match status {
            401 | 403 => return ErrorCategory::Auth,
            404 | 410 => return ErrorCategory::NotFound,
            408 | 504 | 522 | 524 => return ErrorCategory::Timeout,
            429 => return ErrorCategory::RateLimit,
            503 | 529 => return ErrorCategory::Overloaded,
            500 | 502 => return ErrorCategory::ServerError,
            _ => {}
        }
    }
    if let Some(reason) = reason {
        match reason {
            "rate_limit" => return ErrorCategory::RateLimit,
            "timeout" => return ErrorCategory::Timeout,
            "network_error" | "transient_network" => return ErrorCategory::TransientNetwork,
            "server_error" | "provider_error" | "provider_5xx" | "upstream_unavailable" => {
                return ErrorCategory::ServerError;
            }
            "auth_failure" => return ErrorCategory::Auth,
            "model_unavailable" => return ErrorCategory::NotFound,
            _ => {}
        }
    }
    if kind == Some("transient") {
        return ErrorCategory::ServerError;
    }
    ErrorCategory::Generic
}

fn default_mock_error_message(
    status: Option<u16>,
    kind: Option<&str>,
    reason: Option<&str>,
) -> String {
    match (status, kind, reason) {
        (Some(status), Some(kind), Some(reason)) => {
            format!("HTTP {status} mock LLM error ({kind}/{reason})")
        }
        (Some(status), _, Some(reason)) => format!("HTTP {status} mock LLM error ({reason})"),
        (Some(status), _, _) => format!("HTTP {status} mock LLM error"),
        (None, Some(kind), Some(reason)) => format!("mock LLM error ({kind}/{reason})"),
        (None, Some(kind), None) => format!("mock LLM error ({kind})"),
        (None, None, Some(reason)) => format!("mock LLM error ({reason})"),
        (None, None, None) => String::new(),
    }
}

/// The scope a fixture entry belongs to when no `scope` is authored, and the
/// scope a call requests when it sets no `mock_scope`. The shared bucket every
/// unscoped-aux call may fall through to.
pub const DEFAULT_MOCK_SCOPE: &str = "default";

/// Advisory vocabulary for the purposes Harn itself assigns to LLM calls.
/// Fixture scopes remain open strings so applications can add their own
/// purposes without changing the runtime, but spelling a Harn purpose in a
/// fixture gets linted against this producer-owned list.
pub const KNOWN_MOCK_SCOPES: &[&str] = &[
    DEFAULT_MOCK_SCOPE,
    "agent.main",
    "agent.input_guardrail",
    "agent.scope_classifier",
    "completion.judge",
    "step.judge",
];

/// A typed consumption receipt emitted once per mock-provider dispatch when a
/// fixture set is active. It records both sides of default fallback so a
/// caller can prove that a response came from the requested purpose or the
/// shared default bucket without inspecting queue internals.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MockConsumptionReceipt {
    pub requested_scope: String,
    pub resolved_scope: String,
    pub matched: bool,
    pub id: String,
    /// `"once"` | `"sticky"` on a hit; empty on a miss.
    pub consume: String,
    pub fell_through: bool,
    pub remaining: usize,
}

impl MockConsumptionReceipt {
    pub(crate) fn hit(
        requested_scope: &str,
        resolved_scope: &str,
        mock: &LlmMock,
        fell_through: bool,
        remaining: usize,
    ) -> Self {
        Self {
            requested_scope: requested_scope.to_string(),
            resolved_scope: resolved_scope.to_string(),
            matched: true,
            id: mock.entry_id.clone(),
            consume: consume_label(mock.sticky).to_string(),
            fell_through,
            remaining,
        }
    }

    pub(crate) fn miss(requested_scope: &str, remaining: usize) -> Self {
        Self {
            requested_scope: requested_scope.to_string(),
            resolved_scope: String::new(),
            matched: false,
            id: String::new(),
            consume: String::new(),
            fell_through: false,
            remaining,
        }
    }
}

/// Consumption policy label for a matched entry.
fn consume_label(sticky: bool) -> &'static str {
    if sticky {
        "sticky"
    } else {
        "once"
    }
}

#[derive(Clone, Debug)]
pub struct LlmMock {
    pub text: String,
    pub tool_calls: Vec<serde_json::Value>,
    pub raw_tool_calls: Vec<RawProviderToolCall>,
    pub match_pattern: Option<String>, // None = FIFO, Some = glob
    /// Scope bucket this entry serves. Scope strings are open and unknown
    /// values remain valid isolated buckets; the parser only advises on
    /// scopes outside [`KNOWN_MOCK_SCOPES`]. Defaults to [`DEFAULT_MOCK_SCOPE`].
    pub scope: String,
    /// Stable per-entry identifier, surfaced on the consumption receipt.
    /// Authored `id` when present, else the load-time entry index. Assigned
    /// once at load and never shifts as sibling entries are consumed.
    pub entry_id: String,
    /// Reusable when `true`: the entry matches repeatedly and is never
    /// consumed (a `0..N`-times classifier). When `false` it is a one-shot
    /// queue slot, removed after it matches.
    pub sticky: bool,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
    pub cache_write_tokens: Option<i64>,
    pub thinking: Option<String>,
    pub thinking_summary: Option<String>,
    pub stop_reason: Option<String>,
    pub model: String,
    pub provider: Option<String>,
    pub blocks: Option<Vec<serde_json::Value>>,
    pub logprobs: Vec<serde_json::Value>,
    /// When `Some`, this mock synthesizes an error instead of an
    /// `LlmResult`. `text`/`tool_calls` are ignored for error mocks.
    pub error: Option<MockError>,
    /// Ordered visible-text chunks emitted as separate streaming deltas when
    /// this mock is consumed through a streaming caller (e.g. `agent_loop`'s
    /// `on_delta` seam). Empty = the default single-delta behavior, where the
    /// full `text` is delivered as one delta. When non-empty and `text` is
    /// empty, `text` is derived from the concatenation of the chunks so the
    /// non-streaming result and the streamed transcript agree exactly.
    pub stream_chunks: Vec<String>,
}

/// A parsed JSONL fixture: the contract version pinned by the optional header,
/// the file-level `strictScopes` opt-in, and the entries. A file with no
/// header is contract v0 (`schema_version == 0`), replayed byte-identically to
/// the pre-contract single-scope, first-match-wins queue.
#[derive(Clone, Debug, Default)]
pub struct LlmMockFixture {
    pub schema_version: u32,
    pub strict_scopes: bool,
    pub mocks: Vec<LlmMock>,
    pub warnings: Vec<String>,
}

/// Producer-owned facts returned after atomically installing a fixture
/// document. Consumers use this instead of inspecting mutable queue state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LlmMockFixtureReceipt {
    pub schema_version: u32,
    pub strict_scopes: bool,
    pub count: usize,
    pub scopes: Vec<String>,
    pub warnings: Vec<String>,
}

/// The highest fixture contract version this build understands.
pub const MAX_MOCK_SCHEMA_VERSION: u32 = 1;

#[derive(Clone)]
pub(crate) struct LlmMockCall {
    pub mock_scope: String,
    pub api_mode: String,
    pub messages: Vec<serde_json::Value>,
    pub system: Option<String>,
    pub tools: Option<Vec<serde_json::Value>>,
    pub provider_tools: Option<Vec<serde_json::Value>>,
    pub tool_choice: Option<serde_json::Value>,
    pub output_format: serde_json::Value,
    pub thinking: serde_json::Value,
    pub previous_response_id: Option<String>,
    pub store: Option<bool>,
    pub background: Option<bool>,
    pub truncation: Option<String>,
    pub compact: Option<bool>,
    pub include: Option<Vec<String>>,
    pub max_tool_calls: Option<i64>,
    pub prefill: Option<String>,
}

type LlmMockScope = (
    MockQueue,
    Vec<LlmMockCall>,
    BTreeSet<String>,
    Vec<MockConsumptionReceipt>,
);

#[derive(Default)]
struct LlmMockState {
    builtin_queue: MockQueue,
    calls: Vec<LlmMockCall>,
    prompt_cache: BTreeSet<String>,
    scopes: Vec<LlmMockScope>,
    receipts: Vec<MockConsumptionReceipt>,
    cli_scope: Option<Arc<CliLlmMockLease>>,
}

/// Shared mutable mock state for one VM execution tree.
///
/// Child VMs and inline async tasks clone this handle, while independently
/// constructed VMs receive distinct handles. `AmbientExecutionScope` swaps the
/// handle on every future poll, so builtin calls always resolve through the
/// logical VM even when the executor moves that future to another thread.
#[derive(Clone, Default)]
pub(crate) struct LlmMockContext(Arc<Mutex<LlmMockState>>);

impl LlmMockContext {
    pub(crate) fn for_new_vm() -> Self {
        let context = Self::default();
        context.lock().cli_scope = current_cli_llm_mock_lease();
        context
    }

    fn lock(&self) -> MutexGuard<'_, LlmMockState> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

thread_local! {
    static LLM_REPLAY_MODE: RefCell<LlmReplayMode> = const { RefCell::new(LlmReplayMode::Off) };
    static LLM_FIXTURE_DIR: RefCell<String> = const { RefCell::new(String::new()) };
    static TOOL_RECORDINGS: RefCell<Vec<ToolCallRecord>> = const { RefCell::new(Vec::new()) };
    static LLM_MOCK_CONTEXT: RefCell<LlmMockContext> = RefCell::new(LlmMockContext::default());
    // Scripted streaming chunks for the most recently matched builtin mock,
    // stashed by `build_mock_result` and drained by the streaming delta pump in
    // `api.rs`. Per-call and same-thread: set during `mock_llm_response` and
    // taken immediately after in the same synchronous call on the LocalSet.
    static LLM_MOCK_STREAM_CHUNKS: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
}

pub(crate) fn swap_llm_mock_context(next: LlmMockContext) -> LlmMockContext {
    LLM_MOCK_CONTEXT.with(|slot| std::mem::replace(&mut *slot.borrow_mut(), next))
}

pub(crate) fn current_llm_mock_context() -> LlmMockContext {
    LLM_MOCK_CONTEXT.with(|slot| slot.borrow().clone())
}

fn with_mock_state<T>(f: impl FnOnce(&LlmMockState) -> T) -> T {
    let context = current_llm_mock_context();
    let state = context.lock();
    f(&state)
}

fn with_mock_state_mut<T>(f: impl FnOnce(&mut LlmMockState) -> T) -> T {
    let context = current_llm_mock_context();
    let mut state = context.lock();
    f(&mut state)
}

/// Record the scripted streaming chunks for the mock response currently being
/// assembled. Overwrites any prior value; cleared at the top of each
/// `mock_llm_response` so a chunk-less response never inherits stale chunks.
pub(crate) fn set_mock_stream_chunks(chunks: Option<Vec<String>>) {
    LLM_MOCK_STREAM_CHUNKS.with(|slot| *slot.borrow_mut() = chunks);
}

/// Take (and clear) the scripted streaming chunks for the just-produced mock
/// response. Returns `None` when the response scripted no chunks, in which case
/// the streaming pump falls back to a single full-text delta.
pub(crate) fn take_mock_stream_chunks() -> Option<Vec<String>> {
    LLM_MOCK_STREAM_CHUNKS.with(|slot| slot.borrow_mut().take())
}

fn cli_llm_mock_scopes() -> MutexGuard<'static, BTreeMap<u64, CliLlmMockState>> {
    CLI_LLM_MOCK_SCOPES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn next_cli_llm_mock_scope_id() -> u64 {
    CLI_LLM_MOCK_NEXT_SCOPE.fetch_add(1, Ordering::Relaxed)
}

pub(crate) fn current_cli_llm_mock_scope() -> Option<u64> {
    current_cli_llm_mock_lease().map(|scope| scope.0)
}

fn current_cli_llm_mock_lease() -> Option<Arc<CliLlmMockLease>> {
    with_mock_state(|state| state.cli_scope.clone())
}

fn install_cli_llm_mock_scope(state: CliLlmMockState) {
    clear_cli_llm_mock_mode();
    let scope = next_cli_llm_mock_scope_id();
    cli_llm_mock_scopes().insert(scope, state);
    with_mock_state_mut(|state| state.cli_scope = Some(Arc::new(CliLlmMockLease(scope))));
}

/// Test-only escape hatch for Rust unit tests that seed the legacy queue
/// directly. Runtime callers use the typed fixture install or inline builtin.
#[cfg(test)]
pub(crate) fn push_llm_mock(mock: LlmMock) {
    with_mock_state_mut(|state| state.builtin_queue.push_v0(mock));
}

/// Append a legacy inline v0 entry. A whole-document v1 fixture owns its
/// queue shape, so mixing inline entries into it would silently change the
/// document's declared contract.
pub(crate) fn push_inline_llm_mock(mock: LlmMock) -> Result<(), String> {
    with_mock_state_mut(|state| {
        let queue = &mut state.builtin_queue;
        if queue.schema_version() > 0 {
            return Err(
                "cannot append llm_mock() entries to an active versioned fixture; clear or load one complete document"
                    .to_string(),
            );
        }
        queue.push_v0(mock);
        Ok(())
    })
}

/// Atomically replace the builtin fixture store after the whole document has
/// already parsed. A parse failure never reaches this function, preserving the
/// active fixture exactly as it was.
pub(crate) fn install_builtin_llm_mock_fixture(fixture: LlmMockFixture) -> LlmMockFixtureReceipt {
    let queue = MockQueue::from_fixture(fixture);
    let receipt = LlmMockFixtureReceipt {
        schema_version: queue.schema_version(),
        strict_scopes: queue.strict_scopes(),
        count: queue.count(),
        scopes: queue.scopes(),
        warnings: queue.warnings().to_vec(),
    };
    with_mock_state_mut(|state| state.builtin_queue = queue);
    receipt
}

pub(crate) fn get_llm_mock_calls() -> Vec<LlmMockCall> {
    with_mock_state(|state| state.calls.clone())
}

/// Return the consumption receipts recorded since the last reset/scope swap.
pub(crate) fn get_llm_mock_receipts() -> Vec<MockConsumptionReceipt> {
    with_mock_state(|state| state.receipts.clone())
}

fn record_mock_receipt(session_id: Option<&str>, receipt: MockConsumptionReceipt) {
    if receipt.matched {
        if let Some(session_id) = session_id.filter(|id| !id.is_empty()) {
            super::agent_runtime::emit_agent_event_sync(
                &crate::agent_events::AgentEvent::TypedCheckpoint {
                    session_id: session_id.to_string(),
                    checkpoint: serde_json::json!({
                        "kind": "llm_mock_fixture_consumption",
                        "schema": "harn.llm_mock_fixture_consumption.v1",
                        "id": receipt.id,
                        "requested_scope": receipt.requested_scope,
                        "resolved_scope": receipt.resolved_scope,
                        "consume": receipt.consume,
                        "fell_through": receipt.fell_through,
                        "remaining": receipt.remaining,
                    }),
                },
            );
        }
    }
    with_mock_state_mut(|state| state.receipts.push(receipt));
}

pub(crate) fn builtin_llm_mock_snapshot() -> serde_json::Value {
    with_mock_state(|state| {
        let queue = &state.builtin_queue;
        serde_json::json!({
            "schema": "harn.llm_mock_fixture_queue.v1",
            "schema_version": queue.schema_version(),
            "strict_scopes": queue.strict_scopes(),
            "queue_remaining": queue.queue_remaining(),
            "warnings": queue.warnings(),
        })
    })
}

pub(crate) fn builtin_llm_mock_active() -> bool {
    with_mock_state(|state| state.builtin_queue.is_active())
}

pub(crate) fn builtin_llm_mock_strict_scopes() -> bool {
    with_mock_state(|state| state.builtin_queue.strict_scopes())
}

pub(crate) fn reset_llm_mock_state() {
    with_mock_state_mut(|state| {
        state.cli_scope = None;
        state.builtin_queue = MockQueue::default();
        state.calls.clear();
        state.prompt_cache.clear();
        state.scopes.clear();
        state.receipts.clear();
    });
}

/// Save the current builtin LLM mock queue and recorded-calls list, then
/// start a fresh empty scope. Paired with `pop_llm_mock_scope`. Backs
/// the `with_llm_mocks` helper in `std/testing` so tests reliably
/// roll back to the prior state, including when the body throws.
pub(crate) fn push_llm_mock_scope() {
    with_mock_state_mut(|state| {
        let fixture = std::mem::take(&mut state.builtin_queue);
        let calls = std::mem::take(&mut state.calls);
        let cache = std::mem::take(&mut state.prompt_cache);
        let receipts = std::mem::take(&mut state.receipts);
        state.scopes.push((fixture, calls, cache, receipts));
    });
}

/// Restore the most recently pushed builtin LLM mock scope. Returns
/// `false` when there is nothing to pop, so the builtin can surface a
/// clear "imbalanced scope" error rather than silently corrupting
/// state. CLI-installed mocks are intentionally untouched: they are an
/// outer harness and should not flicker on each per-test scope swap.
pub(crate) fn pop_llm_mock_scope() -> bool {
    with_mock_state_mut(|state| match state.scopes.pop() {
        Some((fixture, calls, cache, receipts)) => {
            state.builtin_queue = fixture;
            state.calls = calls;
            state.prompt_cache = cache;
            state.receipts = receipts;
            true
        }
        None => false,
    })
}

pub fn clear_cli_llm_mock_mode() {
    with_mock_state_mut(|state| state.cli_scope = None);
}

pub fn install_cli_llm_mocks(mocks: Vec<LlmMock>) {
    install_cli_llm_mock_scope(CliLlmMockState {
        mode: CliLlmMockMode::Replay,
        queue: MockQueue::from_fixture(LlmMockFixture {
            schema_version: 0,
            strict_scopes: false,
            mocks,
            warnings: Vec::new(),
        }),
        recordings: Vec::new(),
    });
}

/// Install a parsed fixture, honoring its file-level `strictScopes` header.
pub fn install_cli_llm_mock_fixture(fixture: LlmMockFixture) {
    install_cli_llm_mock_scope(CliLlmMockState {
        mode: CliLlmMockMode::Replay,
        queue: MockQueue::from_fixture(fixture),
        recordings: Vec::new(),
    });
}

pub fn enable_cli_llm_mock_recording() {
    install_cli_llm_mock_scope(CliLlmMockState {
        mode: CliLlmMockMode::Record,
        queue: MockQueue::default(),
        recordings: Vec::new(),
    });
}

pub fn take_cli_llm_recordings() -> Vec<LlmMock> {
    let Some(scope) = current_cli_llm_mock_scope() else {
        return Vec::new();
    };
    cli_llm_mock_scopes()
        .get_mut(&scope)
        .map(|state| std::mem::take(&mut state.recordings))
        .unwrap_or_default()
}

pub(crate) fn cli_llm_mock_replay_active() -> bool {
    cli_llm_mock_replay_active_for_scope(current_cli_llm_mock_scope())
}

pub(crate) fn cli_llm_mock_replay_active_for_scope(scope: Option<u64>) -> bool {
    let Some(scope) = scope else {
        return false;
    };
    cli_llm_mock_scopes()
        .get(&scope)
        .is_some_and(|state| state.mode == CliLlmMockMode::Replay)
}

fn record_llm_mock_call(request: &super::api::LlmRequestPayload) {
    with_mock_state_mut(|state| {
        state.calls.push(LlmMockCall {
            mock_scope: request
                .mock_scope
                .as_deref()
                .unwrap_or(DEFAULT_MOCK_SCOPE)
                .to_string(),
            api_mode: request.api_mode.as_str().to_string(),
            messages: request.messages.clone(),
            system: request.system.clone(),
            tools: request.native_tools.clone(),
            provider_tools: if request.provider_tools.is_empty() {
                None
            } else {
                Some(request.provider_tools.clone())
            },
            tool_choice: request.tool_choice.clone(),
            output_format: serde_json::to_value(&request.output_format).unwrap_or_else(|_| {
                serde_json::json!({
                    "kind": "text"
                })
            }),
            thinking: serde_json::to_value(&request.thinking).unwrap_or_else(|_| {
                serde_json::json!({
                    "mode": "disabled"
                })
            }),
            previous_response_id: request.previous_response_id.clone(),
            store: request.store,
            background: request.background,
            truncation: request.truncation.clone(),
            compact: request.compact,
            include: request.include.clone(),
            max_tool_calls: request.max_tool_calls,
            prefill: request.prefill.clone(),
        });
    });
}

/// Build an LlmResult from a matched mock.
fn build_mock_result(mock: &LlmMock, last_msg_len: usize) -> LlmResult {
    // A mock may script an ordered list of visible-text chunks for streaming
    // callers. Derive the flat `text` from their concatenation when `text` was
    // left empty, so the non-streaming result and the streamed transcript agree
    // byte-for-byte, and stash the chunks for the streaming delta pump.
    let effective_text = if !mock.stream_chunks.is_empty() && mock.text.is_empty() {
        mock.stream_chunks.concat()
    } else {
        mock.text.clone()
    };
    set_mock_stream_chunks(if mock.stream_chunks.is_empty() {
        None
    } else {
        Some(mock.stream_chunks.clone())
    });
    let mock = &LlmMock {
        text: effective_text,
        ..mock.clone()
    };
    let (tool_calls, blocks) = if let Some(blocks) = &mock.blocks {
        (mock.tool_calls.clone(), blocks.clone())
    } else {
        let mut blocks = Vec::new();

        if !mock.text.is_empty() {
            blocks.push(serde_json::json!({
                "type": "output_text",
                "text": mock.text,
                "visibility": "public",
            }));
        }

        let mut tool_calls = Vec::new();
        for (i, tc) in mock.tool_calls.iter().enumerate() {
            let id = format!("mock_call_{}", i + 1);
            let name = tc.get("name").and_then(|n| n.as_str()).unwrap_or("unknown");
            let arguments = tc
                .get("arguments")
                .cloned()
                .unwrap_or(serde_json::json!({}));
            tool_calls.push(serde_json::json!({
                "id": id,
                "type": "tool_call",
                "name": name,
                "arguments": arguments,
            }));
            blocks.push(serde_json::json!({
                "type": "tool_call",
                "id": id,
                "name": name,
                "arguments": arguments,
                "visibility": "internal",
            }));
        }

        (tool_calls, blocks)
    };

    LlmResult {
        served_fast: false,
        text: mock.text.clone(),
        raw_tool_calls: if mock.raw_tool_calls.is_empty() {
            Vec::new()
        } else {
            mock.raw_tool_calls.clone()
        },
        tool_calls,
        input_tokens: mock.input_tokens.unwrap_or(last_msg_len as i64),
        output_tokens: mock.output_tokens.unwrap_or(30),
        cache_read_tokens: mock.cache_read_tokens.unwrap_or(0),
        cache_write_tokens: mock.cache_write_tokens.unwrap_or(0),
        cache_supported: true,
        model: mock.model.clone(),
        provider: mock.provider.clone().unwrap_or_else(|| "mock".to_string()),
        thinking: mock.thinking.clone(),
        thinking_summary: mock.thinking_summary.clone(),
        stop_reason: mock.stop_reason.clone(),
        blocks,
        logprobs: mock.logprobs.clone(),
        telemetry: ProviderTelemetry::default(),
    }
}

// Mock prompt patterns match free prose, where `?`/`[`/`{` are ordinary
// characters — only `*` is a wildcard. The shared prose matcher keeps that
// contract (`*`-only ordered segments).
fn collect_mock_match_strings(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(text) if !text.is_empty() => out.push(text.clone()),
        serde_json::Value::String(_) => {}
        serde_json::Value::Array(items) => {
            for item in items {
                collect_mock_match_strings(item, out);
            }
        }
        serde_json::Value::Object(map) => {
            for value in map.values() {
                collect_mock_match_strings(value, out);
            }
        }
        _ => {}
    }
}

fn mock_match_text(messages: &[serde_json::Value]) -> String {
    let mut parts = Vec::new();
    for message in messages {
        collect_mock_match_strings(message, &mut parts);
    }
    parts.join("\n")
}

fn mock_last_prompt_text(messages: &[serde_json::Value]) -> String {
    for message in messages.iter().rev() {
        let Some(content) = message.get("content") else {
            continue;
        };
        let mut parts = Vec::new();
        collect_mock_match_strings(content, &mut parts);
        let text = parts.join("\n");
        if !text.trim().is_empty() {
            return text;
        }
    }
    String::new()
}

fn mock_prompt_cache_key(
    model: &str,
    messages: &[serde_json::Value],
    system: Option<&str>,
    mock_scope: &str,
) -> String {
    serde_json::to_string(&serde_json::json!({
        "model": model,
        "system": system,
        "messages": messages,
        "mock_scope": mock_scope,
    }))
    .unwrap_or_default()
}

fn apply_mock_prompt_cache(result: &mut LlmResult, cache_key: &str) {
    if result.cache_read_tokens > 0 || result.cache_write_tokens > 0 {
        return;
    }
    let cache_tokens = result.input_tokens.max(0);
    if cache_tokens == 0 {
        return;
    }
    let cache_hit = with_mock_state_mut(|state| {
        if state.prompt_cache.contains(cache_key) {
            true
        } else {
            state.prompt_cache.insert(cache_key.to_string());
            false
        }
    });
    if cache_hit {
        result.cache_read_tokens = cache_tokens;
    } else {
        result.cache_write_tokens = cache_tokens;
    }
}

/// Convert a mock's `error` payload into the `VmError` that the
/// provider path would have raised, so classification, retry, and
/// `error_category` all behave identically to a real failure.
fn mock_error_to_vm_error(err: &MockError) -> VmError {
    let message = mock_error_message(err);
    if err.has_provider_envelope() {
        let classified = super::api::classify_llm_error(err.category.clone(), &message);
        let mut dict = BTreeMap::new();
        dict.put_str("category", err.category.as_str());
        dict.put_str(
            "kind",
            err.kind
                .as_deref()
                .unwrap_or_else(|| classified.kind.as_str()),
        );
        dict.put_str(
            "reason",
            err.reason
                .as_deref()
                .unwrap_or_else(|| classified.reason.as_str()),
        );
        dict.put_str("message", message);
        if let Some(status) = err.status {
            dict.insert("status".to_string(), VmValue::Int(i64::from(status)));
        }
        if let Some(retry_after_ms) = err.retry_after_ms {
            dict.insert(
                "retry_after_ms".to_string(),
                VmValue::Int(retry_after_ms as i64),
            );
        }
        return VmError::Thrown(VmValue::dict(dict));
    }

    VmError::CategorizedError {
        message,
        category: err.category.clone(),
    }
}

fn mock_error_message(err: &MockError) -> String {
    // Embed legacy category-only retry hints into the message so the
    // same parser that handles live provider headers populates
    // `retry_after_ms` on the final thrown dict.
    let Some(ms) = err.retry_after_ms else {
        return err.message.clone();
    };
    if err.has_provider_envelope() {
        return err.message.clone();
    }
    let secs = (ms as f64 / 1000.0).max(0.0);
    let sep = if err.message.is_empty() || err.message.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    format!("{}{sep}retry-after: {secs}\n", err.message)
}

/// A matched entry: the response (or error) to serve plus the consumption
/// receipt produced by the shared queue store.
struct ScopedMatch {
    outcome: Result<LlmResult, VmError>,
    receipt: MockConsumptionReceipt,
}

fn build_scoped_match(selected: QueueMatch, match_text: &str) -> ScopedMatch {
    let QueueMatch { mock, receipt } = selected;
    let outcome = match &mock.error {
        Some(err) => Err(mock_error_to_vm_error(err)),
        None => Ok(build_mock_result(&mock, match_text.len())),
    };
    ScopedMatch { outcome, receipt }
}

fn try_match_builtin_mock(scope: &str, match_text: &str) -> Option<ScopedMatch> {
    with_mock_state_mut(|state| {
        state
            .builtin_queue
            .match_request(scope, match_text)
            .map(|selected| build_scoped_match(selected, match_text))
    })
}

fn try_match_cli_mock(
    cli_scope: Option<u64>,
    scope: &str,
    match_text: &str,
) -> Option<ScopedMatch> {
    let cli_scope = cli_scope?;
    let mut scopes = cli_llm_mock_scopes();
    let state = scopes.get_mut(&cli_scope)?;
    if state.mode != CliLlmMockMode::Replay {
        return None;
    }
    state
        .queue
        .match_request(scope, match_text)
        .map(|selected| build_scoped_match(selected, match_text))
}

pub(crate) fn record_cli_llm_result(request: &super::api::LlmRequestPayload, result: &LlmResult) {
    record_unified_tape_llm_call(result);
    let Some(scope) = request.cli_llm_mock_scope else {
        return;
    };
    let mut scopes = cli_llm_mock_scopes();
    let Some(state) = scopes.get_mut(&scope) else {
        return;
    };
    if state.mode != CliLlmMockMode::Record {
        return;
    }
    state.recordings.push(LlmMock {
        text: result.text.clone(),
        tool_calls: result.tool_calls.clone(),
        raw_tool_calls: result.raw_tool_calls.clone(),
        match_pattern: None,
        scope: request
            .mock_scope
            .clone()
            .unwrap_or_else(|| DEFAULT_MOCK_SCOPE.to_string()),
        entry_id: String::new(),
        sticky: false,
        input_tokens: Some(result.input_tokens),
        output_tokens: Some(result.output_tokens),
        cache_read_tokens: Some(result.cache_read_tokens),
        cache_write_tokens: Some(result.cache_write_tokens),
        thinking: result.thinking.clone(),
        thinking_summary: result.thinking_summary.clone(),
        stop_reason: result.stop_reason.clone(),
        model: result.model.clone(),
        provider: Some(result.provider.clone()),
        blocks: Some(result.blocks.clone()),
        logprobs: result.logprobs.clone(),
        error: None,
        stream_chunks: Vec::new(),
    });
}

/// Append an `LlmCall` record to the unified-tape recorder when one is
/// active. The request digest is built from the most recently recorded
/// `LlmMockCall` so the same hashing surface used for fixture matching
/// drives the fidelity oracle's request comparison; falls back to a
/// hash of the response text alone when no matching call is on record
/// (e.g. when `record_llm_mock_call` was bypassed).
fn record_unified_tape_llm_call(result: &LlmResult) {
    if crate::testbench::tape::active_recorder().is_none() {
        return;
    }
    let response_json = serde_json::to_vec(result).unwrap_or_else(|_| Vec::new());
    let request_digest = with_mock_state(|state| state.calls.last().cloned())
        .map(|call| {
            let mut request = serde_json::Map::new();
            request.insert("messages".to_string(), serde_json::json!(call.messages));
            request.insert("system".to_string(), serde_json::json!(call.system));
            request.insert("tools".to_string(), serde_json::json!(call.tools));
            request.insert(
                "tool_choice".to_string(),
                serde_json::json!(call.tool_choice),
            );
            request.insert("thinking".to_string(), serde_json::json!(call.thinking));
            if call.mock_scope != DEFAULT_MOCK_SCOPE {
                request.insert("mock_scope".to_string(), serde_json::json!(call.mock_scope));
            }
            request.insert("model".to_string(), serde_json::json!(result.model));
            if call.api_mode != "chat_completions" {
                request.insert("api_mode".to_string(), serde_json::json!(call.api_mode));
            }
            if call.provider_tools.is_some() {
                request.insert(
                    "provider_tools".to_string(),
                    serde_json::json!(call.provider_tools),
                );
            }
            if call
                .output_format
                .get("kind")
                .and_then(|value| value.as_str())
                != Some("text")
            {
                request.insert(
                    "output_format".to_string(),
                    serde_json::json!(call.output_format),
                );
            }
            if call.previous_response_id.is_some() {
                request.insert(
                    "previous_response_id".to_string(),
                    serde_json::json!(call.previous_response_id),
                );
            }
            if call.store.is_some() {
                request.insert("store".to_string(), serde_json::json!(call.store));
            }
            if call.background.is_some() {
                request.insert("background".to_string(), serde_json::json!(call.background));
            }
            if call.truncation.is_some() {
                request.insert("truncation".to_string(), serde_json::json!(call.truncation));
            }
            if call.compact.is_some() {
                request.insert("compact".to_string(), serde_json::json!(call.compact));
            }
            if call.include.is_some() {
                request.insert("include".to_string(), serde_json::json!(call.include));
            }
            if call.max_tool_calls.is_some() {
                request.insert(
                    "max_tool_calls".to_string(),
                    serde_json::json!(call.max_tool_calls),
                );
            }
            if call.prefill.is_some() {
                request.insert("prefill".to_string(), serde_json::json!(call.prefill));
            }
            let serialized =
                serde_json::to_vec(&serde_json::Value::Object(request)).unwrap_or_default();
            crate::testbench::tape::content_hash(&serialized)
        })
        .unwrap_or_else(|| {
            // Fall back to hashing the response — keeps fidelity comparable
            // across runs even when the request surface wasn't captured.
            crate::testbench::tape::content_hash(result.text.as_bytes())
        });
    crate::testbench::tape::with_active_recorder(|recorder| {
        let response = recorder.payload_from_bytes(response_json);
        Some(crate::testbench::tape::TapeRecordKind::LlmCall {
            request_digest,
            response,
        })
    });
}

fn unmatched_cli_prompt_error(match_text: &str) -> VmError {
    let mut snippet: String = match_text.chars().take(200).collect();
    if match_text.chars().count() > 200 {
        snippet.push_str("...");
    }
    VmError::Runtime(format!("No --llm-mock fixture matched prompt: {snippet:?}"))
}

fn unmatched_builtin_prompt_error(match_text: &str) -> VmError {
    let mut snippet: String = match_text.chars().take(200).collect();
    if match_text.chars().count() > 200 {
        snippet.push_str("...");
    }
    VmError::Runtime(format!(
        "No llm_mock fixture matched prompt in a strict scope: {snippet:?}"
    ))
}

/// Set LLM replay mode (record/replay) and fixture directory.
pub fn set_replay_mode(mode: LlmReplayMode, fixture_dir: &str) {
    LLM_REPLAY_MODE.with(|v| *v.borrow_mut() = mode);
    LLM_FIXTURE_DIR.with(|v| *v.borrow_mut() = fixture_dir.to_string());
}

pub(crate) fn get_replay_mode() -> LlmReplayMode {
    LLM_REPLAY_MODE.with(|v| *v.borrow())
}

pub(crate) fn get_fixture_dir() -> String {
    LLM_FIXTURE_DIR.with(|v| v.borrow().clone())
}

/// Hash a request for fixture file naming using canonical JSON serialization.
pub(crate) fn fixture_hash(
    model: &str,
    messages: &[serde_json::Value],
    system: Option<&str>,
    mock_scope: Option<&str>,
) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    model.hash(&mut hasher);
    // Canonical JSON hashing is stable across Debug-format changes.
    serde_json::to_string(messages)
        .unwrap_or_default()
        .hash(&mut hasher);
    system.hash(&mut hasher);
    if mock_scope.is_some_and(|scope| scope != DEFAULT_MOCK_SCOPE) {
        mock_scope.hash(&mut hasher);
    }
    format!("{:016x}", hasher.finish())
}

pub(crate) fn fixture_hash_for_request(request: &super::api::LlmRequestPayload) -> String {
    fixture_hash(
        &request.model,
        &request.messages,
        request.system.as_deref(),
        request.mock_scope.as_deref(),
    )
}

pub(crate) fn save_fixture(hash: &str, result: &LlmResult) {
    let dir = get_fixture_dir();
    if dir.is_empty() {
        return;
    }
    let _ = std::fs::create_dir_all(&dir);
    let path = format!("{dir}/{hash}.json");
    let json = serde_json::json!({
        "text": result.text,
        "tool_calls": result.tool_calls,
        "raw_tool_calls": result.raw_tool_calls,
        "input_tokens": result.input_tokens,
        "output_tokens": result.output_tokens,
        "cache_read_tokens": result.cache_read_tokens,
        "cache_write_tokens": result.cache_write_tokens,
        "model": result.model,
        "provider": result.provider,
        "thinking": result.thinking,
        "thinking_summary": result.thinking_summary,
        "stop_reason": result.stop_reason,
        "blocks": result.blocks,
        "logprobs": result.logprobs,
    });
    let _ = std::fs::write(
        &path,
        serde_json::to_string_pretty(&json).unwrap_or_default(),
    );
}

pub(crate) fn load_fixture(hash: &str) -> Option<LlmResult> {
    let dir = get_fixture_dir();
    if dir.is_empty() {
        return None;
    }
    let path = format!("{dir}/{hash}.json");
    let content = std::fs::read_to_string(&path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;
    Some(LlmResult {
        served_fast: false,
        text: json["text"].as_str().unwrap_or("").to_string(),
        tool_calls: json["tool_calls"].as_array().cloned().unwrap_or_default(),
        raw_tool_calls: RawProviderToolCall::array_from_value(&json["raw_tool_calls"]).ok()?,
        input_tokens: json["input_tokens"].as_i64().unwrap_or(0),
        output_tokens: json["output_tokens"].as_i64().unwrap_or(0),
        cache_read_tokens: json["cache_read_tokens"].as_i64().unwrap_or(0),
        cache_write_tokens: json["cache_write_tokens"]
            .as_i64()
            .or_else(|| json["cache_creation_input_tokens"].as_i64())
            .unwrap_or(0),
        cache_supported: json["cache_supported"].as_bool().unwrap_or(true),
        model: json["model"].as_str().unwrap_or("").to_string(),
        provider: json["provider"].as_str().unwrap_or("mock").to_string(),
        thinking: json["thinking"].as_str().map(|s| s.to_string()),
        thinking_summary: json["thinking_summary"].as_str().map(|s| s.to_string()),
        stop_reason: json["stop_reason"].as_str().map(|s| s.to_string()),
        blocks: json["blocks"].as_array().cloned().unwrap_or_default(),
        logprobs: json["logprobs"].as_array().cloned().unwrap_or_default(),
        telemetry: serde_json::from_value(json["telemetry"].clone()).unwrap_or_default(),
    })
}

/// Generate stub argument values for required parameters in a tool schema.
/// This makes mock tool calls realistic — a real model would always fill
/// required fields, so the mock should too.
fn mock_required_args(tool_schema: &serde_json::Value) -> serde_json::Value {
    let mut args = serde_json::Map::new();
    // Anthropic: {name, input_schema: {properties, required}}
    // OpenAI:    {function: {name, parameters: {properties, required}}}
    // Harn VM:   {parameters: {name: {type, required}}}  (from tool_define)
    let input_schema = tool_schema
        .get("input_schema")
        .or_else(|| tool_schema.get("inputSchema"))
        .or_else(|| {
            tool_schema
                .get("function")
                .and_then(|f| f.get("parameters"))
        })
        .or_else(|| tool_schema.get("parameters"));
    let Some(schema) = input_schema else {
        return serde_json::Value::Object(args);
    };
    let required: std::collections::BTreeSet<String> = schema
        .get("required")
        .and_then(|r| r.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    if let Some(props) = schema.get("properties").and_then(|p| p.as_object()) {
        for (name, prop) in props {
            if !required.contains(name) {
                continue;
            }
            let ty = prop
                .get("type")
                .and_then(|t| t.as_str())
                .unwrap_or("string");
            let placeholder = match ty {
                "integer" => serde_json::json!(0),
                "number" => serde_json::json!(0.0),
                "boolean" => serde_json::json!(false),
                "array" => serde_json::json!([]),
                "object" => serde_json::json!({}),
                _ => serde_json::json!(""),
            };
            args.insert(name.clone(), placeholder);
        }
    }
    serde_json::Value::Object(args)
}

fn mock_tool_name(tool: &serde_json::Value) -> Option<&str> {
    tool.get("name")
        .or_else(|| {
            tool.get("function")
                .and_then(|function| function.get("name"))
        })
        .and_then(|name| name.as_str())
}

fn mock_auto_tool_candidate(tools: &[serde_json::Value]) -> Option<&serde_json::Value> {
    tools
        .iter()
        .find(|tool| mock_tool_name(tool) != Some("agent_await_resumption"))
}

/// Mock LLM provider -- deterministic responses for testing without API keys.
/// When configurable mocks have been registered via `llm_mock()`, those are
/// checked first (FIFO queue, then pattern matching). Falls through to the
/// default deterministic behavior when no mocks match.
pub(crate) fn mock_llm_response(
    request: &super::api::LlmRequestPayload,
) -> Result<LlmResult, VmError> {
    record_llm_mock_call(request);
    // Reset per-call so a response that scripts no chunks (auto tool calls,
    // generated prose, error mocks) never inherits a prior mock's chunks.
    set_mock_stream_chunks(None);

    let messages = &request.messages;
    let system = request.system.as_deref();
    let match_text = mock_match_text(messages);
    let prompt_text = mock_last_prompt_text(messages);
    let requested_scope = request.mock_scope.as_deref().unwrap_or(DEFAULT_MOCK_SCOPE);
    let cache_key = mock_prompt_cache_key(&request.model, messages, system, requested_scope);

    if let Some(matched) =
        try_match_cli_mock(request.cli_llm_mock_scope, requested_scope, &match_text)
    {
        record_mock_receipt(request.session_id.as_deref(), matched.receipt);
        return matched.outcome.map(|mut result| {
            if request.cache {
                apply_mock_prompt_cache(&mut result, &cache_key);
            }
            result
        });
    }

    if let Some(matched) = try_match_builtin_mock(requested_scope, &match_text) {
        record_mock_receipt(request.session_id.as_deref(), matched.receipt);
        return matched.outcome.map(|mut result| {
            if request.cache {
                apply_mock_prompt_cache(&mut result, &cache_key);
            }
            result
        });
    }

    // No entry matched anywhere this call could reach. Record one miss receipt
    // when a fixture set is active so a strict-scope hard miss stays assertable.
    if cli_llm_mock_replay_active_for_scope(request.cli_llm_mock_scope) || builtin_llm_mock_active()
    {
        let receipt = if cli_llm_mock_replay_active_for_scope(request.cli_llm_mock_scope) {
            let scopes = cli_llm_mock_scopes();
            scopes
                .get(&request.cli_llm_mock_scope.unwrap())
                .map(|state| state.queue.miss_receipt(requested_scope))
                .unwrap_or_else(|| MockConsumptionReceipt::miss(requested_scope, 0))
        } else {
            with_mock_state(|state| state.builtin_queue.miss_receipt(requested_scope))
        };
        record_mock_receipt(request.session_id.as_deref(), receipt);
    }

    if cli_llm_mock_replay_active_for_scope(request.cli_llm_mock_scope) {
        return Err(unmatched_cli_prompt_error(&match_text));
    }
    if builtin_llm_mock_strict_scopes() {
        return Err(unmatched_builtin_prompt_error(&match_text));
    }

    // Generate a mock tool call for the first tool, filling required
    // params with placeholders so the call passes schema validation.
    if let Some(tools) = request.native_tools.as_deref() {
        if let Some(first_tool) = mock_auto_tool_candidate(tools) {
            let tool_name = mock_tool_name(first_tool).unwrap_or("unknown");
            let mock_args = mock_required_args(first_tool);
            let mut result = LlmResult {
                served_fast: false,
                text: String::new(),
                tool_calls: vec![serde_json::json!({
                        "id": "mock_call_1",
                        "type": "tool_call",
                        "name": tool_name,
                "arguments": mock_args
                })],
                raw_tool_calls: Vec::new(),
                input_tokens: prompt_text.len() as i64,
                output_tokens: 20,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                cache_supported: true,
                model: request.model.clone(),
                provider: "mock".to_string(),
                thinking: None,
                thinking_summary: None,
                stop_reason: None,
                blocks: vec![serde_json::json!({
                    "type": "tool_call",
                    "id": "mock_call_1",
                    "name": tool_name,
                    "arguments": mock_args,
                    "visibility": "internal",
                })],
                logprobs: Vec::new(),
                telemetry: ProviderTelemetry::default(),
            };
            if request.cache {
                apply_mock_prompt_cache(&mut result, &cache_key);
            }
            return Ok(result);
        }
    }

    // Preserve the historical auto-complete behavior for tagged text-tool
    // prompts only. Bare `##DONE##` in no-tool/native prompts changes
    // loop semantics by completing runs that used to exhaust budget unless
    // a fixture explicitly returned the sentinel.
    let tagged_done = system.is_some_and(|s| s.contains("<done>"));

    let prose_body = if prompt_text.is_empty() {
        "Mock LLM response".to_string()
    } else {
        let word_count = prompt_text.split_whitespace().count();
        format!(
            "Mock response to {word_count}-word prompt: {}",
            prompt_text.chars().take(100).collect::<String>()
        )
    };
    let response = if tagged_done {
        format!("<assistant_prose>{prose_body}</assistant_prose>\n<done>##DONE##</done>")
    } else {
        prose_body
    };

    let mut result = LlmResult {
        served_fast: false,
        text: response.clone(),
        tool_calls: vec![],
        raw_tool_calls: Vec::new(),
        input_tokens: prompt_text.len() as i64,
        output_tokens: 30,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        cache_supported: true,
        model: request.model.clone(),
        provider: "mock".to_string(),
        thinking: None,
        thinking_summary: None,
        stop_reason: None,
        blocks: vec![serde_json::json!({
            "type": "output_text",
            "text": response,
            "visibility": "public",
        })],
        logprobs: Vec::new(),
        telemetry: ProviderTelemetry::default(),
    };
    if request.cache {
        apply_mock_prompt_cache(&mut result, &cache_key);
    }
    Ok(result)
}

/// Take all recorded tool calls, leaving the buffer empty.
pub fn drain_tool_recordings() -> Vec<ToolCallRecord> {
    TOOL_RECORDINGS.with(|v| std::mem::take(&mut *v.borrow_mut()))
}

#[cfg(test)]
#[path = "mock_tests.rs"]
mod tests;
