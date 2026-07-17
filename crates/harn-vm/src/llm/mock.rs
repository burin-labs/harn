use crate::value::VmDictExt;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex, MutexGuard};

use super::api::{LlmResult, ProviderTelemetry, RawProviderToolCall};
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
    mocks: Vec<LlmMock>,
    recordings: Vec<LlmMock>,
    /// When set (v1 header `strictScopes: true`), a call whose scope has no
    /// matching entry is a hard miss — it never falls through to `default`.
    strict_scopes: bool,
}

static CLI_LLM_MOCK_NEXT_SCOPE: AtomicU64 = AtomicU64::new(1);
static CLI_LLM_MOCK_SCOPES: LazyLock<Mutex<BTreeMap<u64, CliLlmMockState>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));

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

/// A typed consumption receipt emitted once per mock-provider dispatch when a
/// fixture set is active, so tests can assert scope-level consumption without
/// reading engine internals. On a hit, `scope` is the bucket the entry was
/// drawn from (which distinguishes a `default` fall-through from a `main`
/// consumption); on a miss it is the scope the call requested.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MockConsumptionReceipt {
    pub scope: String,
    pub matched: bool,
    pub entry_id: String,
    /// `"once"` | `"sticky"` on a hit; empty on a miss.
    pub consume: String,
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
    /// Scope bucket this entry serves. `"main"` is the primary agent turn;
    /// any other name (`judge`, `critic`, `plan`, …) is its own scope. Open
    /// strings: an unknown name is simply its own scope. Defaults to
    /// [`DEFAULT_MOCK_SCOPE`].
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
}

/// The highest fixture contract version this build understands.
pub const MAX_MOCK_SCHEMA_VERSION: u32 = 1;

#[derive(Clone)]
pub(crate) struct LlmMockCall {
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
}

type LlmMockScope = (
    Vec<LlmMock>,
    Vec<LlmMockCall>,
    BTreeSet<String>,
    Vec<MockConsumptionReceipt>,
);

thread_local! {
    static LLM_REPLAY_MODE: RefCell<LlmReplayMode> = const { RefCell::new(LlmReplayMode::Off) };
    static LLM_FIXTURE_DIR: RefCell<String> = const { RefCell::new(String::new()) };
    static TOOL_RECORDINGS: RefCell<Vec<ToolCallRecord>> = const { RefCell::new(Vec::new()) };
    static LLM_MOCKS: RefCell<Vec<LlmMock>> = const { RefCell::new(Vec::new()) };
    static CLI_LLM_MOCK_SCOPE: RefCell<Option<u64>> = const { RefCell::new(None) };
    static LLM_MOCK_CALLS: RefCell<Vec<LlmMockCall>> = const { RefCell::new(Vec::new()) };
    static LLM_PROMPT_CACHE: RefCell<BTreeSet<String>> = const { RefCell::new(BTreeSet::new()) };
    static LLM_MOCK_SCOPES: RefCell<Vec<LlmMockScope>> = const { RefCell::new(Vec::new()) };
    // Consumption receipts emitted by the scope matcher, one per dispatch that
    // reached an active fixture set. Drained by `get_llm_mock_receipts`.
    static LLM_MOCK_RECEIPTS: RefCell<Vec<MockConsumptionReceipt>> = const { RefCell::new(Vec::new()) };
    // Scripted streaming chunks for the most recently matched builtin mock,
    // stashed by `build_mock_result` and drained by the streaming delta pump in
    // `api.rs`. Per-call and same-thread: set during `mock_llm_response` and
    // taken immediately after in the same synchronous call on the LocalSet.
    static LLM_MOCK_STREAM_CHUNKS: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
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
    CLI_LLM_MOCK_SCOPE.with(|scope| *scope.borrow())
}

fn install_cli_llm_mock_scope(state: CliLlmMockState) {
    clear_cli_llm_mock_mode();
    let scope = next_cli_llm_mock_scope_id();
    cli_llm_mock_scopes().insert(scope, state);
    CLI_LLM_MOCK_SCOPE.with(|slot| *slot.borrow_mut() = Some(scope));
}

pub(crate) fn push_llm_mock(mock: LlmMock) {
    LLM_MOCKS.with(|v| v.borrow_mut().push(mock));
}

pub(crate) fn get_llm_mock_calls() -> Vec<LlmMockCall> {
    LLM_MOCK_CALLS.with(|v| v.borrow().clone())
}

/// Return the consumption receipts recorded since the last reset/scope swap.
pub(crate) fn get_llm_mock_receipts() -> Vec<MockConsumptionReceipt> {
    LLM_MOCK_RECEIPTS.with(|v| v.borrow().clone())
}

fn record_mock_receipt(receipt: MockConsumptionReceipt) {
    LLM_MOCK_RECEIPTS.with(|v| v.borrow_mut().push(receipt));
}

pub(crate) fn builtin_llm_mock_active() -> bool {
    LLM_MOCKS.with(|v| !v.borrow().is_empty())
}

pub(crate) fn reset_llm_mock_state() {
    LLM_MOCKS.with(|v| v.borrow_mut().clear());
    clear_cli_llm_mock_mode();
    LLM_MOCK_CALLS.with(|v| v.borrow_mut().clear());
    LLM_PROMPT_CACHE.with(|v| v.borrow_mut().clear());
    LLM_MOCK_SCOPES.with(|v| v.borrow_mut().clear());
    LLM_MOCK_RECEIPTS.with(|v| v.borrow_mut().clear());
}

/// Save the current builtin LLM mock queue and recorded-calls list, then
/// start a fresh empty scope. Paired with `pop_llm_mock_scope`. Backs
/// the `with_llm_mocks` helper in `std/testing` so tests reliably
/// roll back to the prior state, including when the body throws.
pub(crate) fn push_llm_mock_scope() {
    let mocks = LLM_MOCKS.with(|v| std::mem::take(&mut *v.borrow_mut()));
    let calls = LLM_MOCK_CALLS.with(|v| std::mem::take(&mut *v.borrow_mut()));
    let cache = LLM_PROMPT_CACHE.with(|v| std::mem::take(&mut *v.borrow_mut()));
    let receipts = LLM_MOCK_RECEIPTS.with(|v| std::mem::take(&mut *v.borrow_mut()));
    LLM_MOCK_SCOPES.with(|v| v.borrow_mut().push((mocks, calls, cache, receipts)));
}

/// Restore the most recently pushed builtin LLM mock scope. Returns
/// `false` when there is nothing to pop, so the builtin can surface a
/// clear "imbalanced scope" error rather than silently corrupting
/// state. CLI-installed mocks are intentionally untouched: they are an
/// outer harness and should not flicker on each per-test scope swap.
pub(crate) fn pop_llm_mock_scope() -> bool {
    let entry = LLM_MOCK_SCOPES.with(|v| v.borrow_mut().pop());
    match entry {
        Some((mocks, calls, cache, receipts)) => {
            LLM_MOCKS.with(|v| *v.borrow_mut() = mocks);
            LLM_MOCK_CALLS.with(|v| *v.borrow_mut() = calls);
            LLM_PROMPT_CACHE.with(|v| *v.borrow_mut() = cache);
            LLM_MOCK_RECEIPTS.with(|v| *v.borrow_mut() = receipts);
            true
        }
        None => false,
    }
}

pub fn clear_cli_llm_mock_mode() {
    let scope = CLI_LLM_MOCK_SCOPE.with(|slot| slot.borrow_mut().take());
    if let Some(scope) = scope {
        cli_llm_mock_scopes().remove(&scope);
    }
}

pub fn install_cli_llm_mocks(mocks: Vec<LlmMock>) {
    install_cli_llm_mock_scope(CliLlmMockState {
        mode: CliLlmMockMode::Replay,
        mocks,
        recordings: Vec::new(),
        strict_scopes: false,
    });
}

/// Install a parsed fixture, honoring its file-level `strictScopes` header.
pub fn install_cli_llm_mock_fixture(fixture: LlmMockFixture) {
    install_cli_llm_mock_scope(CliLlmMockState {
        mode: CliLlmMockMode::Replay,
        mocks: fixture.mocks,
        recordings: Vec::new(),
        strict_scopes: fixture.strict_scopes,
    });
}

pub fn enable_cli_llm_mock_recording() {
    install_cli_llm_mock_scope(CliLlmMockState {
        mode: CliLlmMockMode::Record,
        mocks: Vec::new(),
        recordings: Vec::new(),
        strict_scopes: false,
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
    LLM_MOCK_CALLS.with(|v| {
        v.borrow_mut().push(LlmMockCall {
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
use harn_glob::match_prose as mock_glob_match;

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
) -> String {
    serde_json::to_string(&serde_json::json!({
        "model": model,
        "system": system,
        "messages": messages,
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
    let cache_hit = LLM_PROMPT_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.contains(cache_key) {
            true
        } else {
            cache.insert(cache_key.to_string());
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
/// receipt describing which scope bucket it came from.
struct ScopedMatch {
    outcome: Result<LlmResult, VmError>,
    receipt: MockConsumptionReceipt,
}

/// Serve the entry at `idx`, consuming it unless it is `sticky`, and build its
/// consumption receipt. FIFO and glob entries share this path so the
/// `once`/`sticky` policy is honored uniformly.
fn serve_scope_entry(mocks: &mut Vec<LlmMock>, idx: usize, match_text: &str) -> ScopedMatch {
    let receipt = MockConsumptionReceipt {
        scope: mocks[idx].scope.clone(),
        matched: true,
        entry_id: mocks[idx].entry_id.clone(),
        consume: consume_label(mocks[idx].sticky).to_string(),
    };
    let outcome = if mocks[idx].sticky {
        let mock = &mocks[idx];
        match &mock.error {
            Some(err) => Err(mock_error_to_vm_error(err)),
            None => Ok(build_mock_result(mock, match_text.len())),
        }
    } else {
        let mock = mocks.remove(idx);
        match &mock.error {
            Some(err) => Err(mock_error_to_vm_error(err)),
            None => Ok(build_mock_result(&mock, match_text.len())),
        }
    };
    ScopedMatch { outcome, receipt }
}

/// Match within a single scope bucket, preserving the historical
/// FIFO-then-glob priority (unpatterned entries consumed in order first, then
/// glob patterns) but only over entries whose `scope` equals `scope`.
fn match_in_scope(mocks: &mut Vec<LlmMock>, scope: &str, match_text: &str) -> Option<ScopedMatch> {
    if let Some(idx) = mocks
        .iter()
        .position(|m| m.match_pattern.is_none() && m.scope == scope)
    {
        return Some(serve_scope_entry(mocks, idx, match_text));
    }

    for idx in 0..mocks.len() {
        if mocks[idx].scope != scope {
            continue;
        }
        let matches = mocks[idx]
            .match_pattern
            .as_ref()
            .is_some_and(|pattern| mock_glob_match(pattern, match_text));
        if matches {
            return Some(serve_scope_entry(mocks, idx, match_text));
        }
    }

    None
}

/// Resolve a call with scope `scope` against a fixture queue: match the scope's
/// own bucket first, then (unless `strict_scopes`) fall through to the shared
/// `default` bucket — and never to `main` or any other scope. `None` means no
/// entry matched anywhere the call is allowed to reach.
fn try_match_scoped(
    mocks: &mut Vec<LlmMock>,
    scope: &str,
    strict_scopes: bool,
    match_text: &str,
) -> Option<ScopedMatch> {
    if let Some(matched) = match_in_scope(mocks, scope, match_text) {
        return Some(matched);
    }
    if scope != DEFAULT_MOCK_SCOPE && !strict_scopes {
        if let Some(matched) = match_in_scope(mocks, DEFAULT_MOCK_SCOPE, match_text) {
            return Some(matched);
        }
    }
    None
}

fn try_match_builtin_mock(scope: &str, match_text: &str) -> Option<ScopedMatch> {
    LLM_MOCKS.with(|mocks| try_match_scoped(&mut mocks.borrow_mut(), scope, false, match_text))
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
    let strict_scopes = state.strict_scopes;
    try_match_scoped(&mut state.mocks, scope, strict_scopes, match_text)
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
        scope: DEFAULT_MOCK_SCOPE.to_string(),
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
    let request_digest = LLM_MOCK_CALLS
        .with(|calls| calls.borrow().last().cloned())
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
) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    model.hash(&mut hasher);
    // Canonical JSON hashing is stable across Debug-format changes.
    serde_json::to_string(messages)
        .unwrap_or_default()
        .hash(&mut hasher);
    system.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
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
        "cache_creation_input_tokens": result.cache_write_tokens,
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
    let cache_key = mock_prompt_cache_key(&request.model, messages, system);
    let requested_scope = request.mock_scope.as_deref().unwrap_or(DEFAULT_MOCK_SCOPE);

    if let Some(matched) =
        try_match_cli_mock(request.cli_llm_mock_scope, requested_scope, &match_text)
    {
        record_mock_receipt(matched.receipt);
        return matched.outcome.map(|mut result| {
            if request.cache {
                apply_mock_prompt_cache(&mut result, &cache_key);
            }
            result
        });
    }

    if let Some(matched) = try_match_builtin_mock(requested_scope, &match_text) {
        record_mock_receipt(matched.receipt);
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
        record_mock_receipt(MockConsumptionReceipt {
            scope: requested_scope.to_string(),
            matched: false,
            entry_id: String::new(),
            consume: String::new(),
        });
    }

    if cli_llm_mock_replay_active_for_scope(request.cli_llm_mock_scope) {
        return Err(unmatched_cli_prompt_error(&match_text));
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
mod tests {
    use super::*;
    use crate::llm::api::LlmRequestPayload;

    fn text_mock(text: &str) -> LlmMock {
        LlmMock {
            text: text.to_string(),
            tool_calls: Vec::new(),
            raw_tool_calls: Vec::new(),
            match_pattern: None,
            scope: DEFAULT_MOCK_SCOPE.to_string(),
            entry_id: String::new(),
            sticky: false,
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
            thinking: None,
            thinking_summary: None,
            stop_reason: None,
            model: "fixture-model".to_string(),
            provider: None,
            blocks: None,
            logprobs: Vec::new(),
            error: None,
            stream_chunks: Vec::new(),
        }
    }

    #[test]
    fn build_mock_result_surfaces_fixture_tool_calls() {
        // The CLI-mock fixture shape a downstream native-tool test uses.
        let mock = crate::llm::jsonl::parse_llm_mock_value(&serde_json::json!({
            "match": "*",
            "consume_match": true,
            "tool_calls": [{"name": "ask_user", "arguments": {"question": "Which?"}}]
        }))
        .expect("parse mock");
        assert!(
            !mock.tool_calls.is_empty(),
            "fixture tool_calls must parse into the mock: {:?}",
            mock.tool_calls
        );
        let result = build_mock_result(&mock, 10);
        assert!(
            !result.tool_calls.is_empty(),
            "build_mock_result must surface tool_calls: {:?}",
            result.tool_calls
        );
        assert_eq!(result.tool_calls[0]["name"], "ask_user");
    }

    #[test]
    fn cli_llm_mock_replay_scope_survives_provider_worker_thread() {
        reset_llm_mock_state();
        install_cli_llm_mocks(vec![text_mock("cross-thread replay")]);
        let request = LlmRequestPayload::from(&crate::llm::api::options::base_opts("anthropic"));

        assert!(request.cli_llm_mock_scope.is_some());
        assert!(crate::llm::providers::MockProvider::should_intercept_request(&request));

        let result = std::thread::spawn(move || {
            assert!(crate::llm::providers::MockProvider::should_intercept_request(&request));
            mock_llm_response(&request)
        })
        .join()
        .expect("provider worker thread")
        .expect("mock response");

        assert_eq!(result.text, "cross-thread replay");
        clear_cli_llm_mock_mode();
    }

    #[test]
    fn cli_llm_mock_record_scope_collects_provider_worker_thread_results() {
        reset_llm_mock_state();
        enable_cli_llm_mock_recording();
        let request = LlmRequestPayload::from(&crate::llm::api::options::base_opts("anthropic"));
        let result = build_mock_result(&text_mock("cross-thread record"), 7);

        assert!(request.cli_llm_mock_scope.is_some());
        std::thread::spawn(move || record_cli_llm_result(&request, &result))
            .join()
            .expect("provider worker thread");

        let recordings = take_cli_llm_recordings();
        assert_eq!(recordings.len(), 1);
        assert_eq!(recordings[0].text, "cross-thread record");
        clear_cli_llm_mock_mode();
    }

    #[test]
    fn cli_mock_native_tool_calls_reach_the_live_result() {
        // Exercises the full CLI `--llm-mock` path (install scope -> match ->
        // build) the burin native-tool fixture uses, which the isolated
        // parse/build/message tests skip. If this yields an empty result, the
        // downstream native-tool mock test sees zero tool-call events.
        reset_llm_mock_state();
        let mocks = vec![crate::llm::jsonl::parse_llm_mock_value(&serde_json::json!({
            "match": "*",
            "consume_match": true,
            "tool_calls": [{"name": "ask_user", "arguments": {"question": "Which?"}}]
        }))
        .expect("parse mock")];
        install_cli_llm_mocks(mocks);
        let request = LlmRequestPayload::from(&crate::llm::api::options::base_opts("fixture"));
        assert!(
            request.cli_llm_mock_scope.is_some(),
            "cli mock scope must be active"
        );
        let result = mock_llm_response(&request).expect("mock response");
        clear_cli_llm_mock_mode();
        assert!(
            !result.tool_calls.is_empty(),
            "CLI mock native tool_calls must reach the live result: text={:?} tool_calls={:?}",
            result.text,
            result.tool_calls
        );
    }

    // --- Versioned mock-fixture contract (bc#4969) ---

    /// Build a request that draws from `scope`, carrying `prompt` as the sole
    /// user message. Installing the fixture first means the `From` impl captures
    /// the live CLI mock scope handle.
    fn request_with_scope(prompt: &str, scope: Option<&str>) -> LlmRequestPayload {
        let mut opts = crate::llm::api::options::base_opts("fixture");
        opts.messages = vec![serde_json::json!({"role": "user", "content": prompt})];
        opts.mock_scope = scope.map(str::to_string);
        LlmRequestPayload::from(&opts)
    }

    /// Assemble a v1 fixture from JSON entries, assigning stable entry ids by
    /// position exactly as the file loader would.
    fn v1_fixture(strict_scopes: bool, entries: &[serde_json::Value]) -> LlmMockFixture {
        let mocks = entries
            .iter()
            .enumerate()
            .map(|(idx, value)| {
                crate::llm::jsonl::parse_llm_mock_value_versioned(value, 1, idx)
                    .expect("parse v1 fixture entry")
            })
            .collect();
        LlmMockFixture {
            schema_version: 1,
            strict_scopes,
            mocks,
        }
    }

    #[test]
    fn scoped_fixture_serves_main_and_judge_from_their_own_buckets() {
        // bc#4969: with a shared first-match-wins queue this is unwritable — the
        // judge call would cannibalize the main entry. Scoped buckets keep them
        // apart.
        reset_llm_mock_state();
        install_cli_llm_mock_fixture(v1_fixture(
            false,
            &[
                serde_json::json!({"scope": "main", "text": "MAIN"}),
                serde_json::json!({"scope": "judge", "text": "JUDGE"}),
            ],
        ));

        let main = mock_llm_response(&request_with_scope("turn", Some("main"))).expect("main");
        assert_eq!(main.text, "MAIN");
        let judge = mock_llm_response(&request_with_scope("verify", Some("judge"))).expect("judge");
        assert_eq!(judge.text, "JUDGE");

        let receipts = get_llm_mock_receipts();
        assert_eq!(receipts.len(), 2);
        assert_eq!(receipts[0].scope, "main");
        assert!(receipts[0].matched);
        assert_eq!(receipts[1].scope, "judge");
        clear_cli_llm_mock_mode();
    }

    #[test]
    fn aux_call_falls_through_to_default_never_to_main() {
        // The core fix: a judge call with no judge entry reaches the shared
        // `default` bucket but must NEVER consume the `main` entry.
        reset_llm_mock_state();
        install_cli_llm_mock_fixture(v1_fixture(
            false,
            &[
                serde_json::json!({"scope": "main", "text": "MAIN"}),
                serde_json::json!({"scope": "default", "text": "DEFAULT"}),
            ],
        ));

        let judge = mock_llm_response(&request_with_scope("verify", Some("judge"))).expect("judge");
        assert_eq!(
            judge.text, "DEFAULT",
            "unscoped-aux call must reach default"
        );

        // The main entry is untouched: a real main call still gets it.
        let main = mock_llm_response(&request_with_scope("turn", Some("main"))).expect("main");
        assert_eq!(main.text, "MAIN");

        let receipts = get_llm_mock_receipts();
        assert_eq!(
            receipts[0].scope, "default",
            "judge drew from default bucket"
        );
        assert!(receipts[0].matched);
        clear_cli_llm_mock_mode();
    }

    #[test]
    fn v0_unscoped_fixture_replays_in_fifo_order() {
        // Back-compat golden: an unscoped v0 fixture keeps first-match-wins FIFO
        // consumption, unchanged by the scope machinery.
        reset_llm_mock_state();
        let mocks = vec![
            crate::llm::jsonl::parse_llm_mock_value(&serde_json::json!({"text": "first"}))
                .expect("parse first"),
            crate::llm::jsonl::parse_llm_mock_value(&serde_json::json!({"text": "second"}))
                .expect("parse second"),
        ];
        install_cli_llm_mocks(mocks);

        assert_eq!(
            mock_llm_response(&request_with_scope("a", None))
                .expect("first")
                .text,
            "first"
        );
        assert_eq!(
            mock_llm_response(&request_with_scope("b", None))
                .expect("second")
                .text,
            "second"
        );
        clear_cli_llm_mock_mode();
    }

    #[test]
    fn sticky_entry_reused_while_once_entry_is_consumed() {
        reset_llm_mock_state();
        install_cli_llm_mock_fixture(v1_fixture(
            false,
            &[
                serde_json::json!({"scope": "judge", "match": "*", "consume": "sticky", "text": "STICKY"}),
                serde_json::json!({"scope": "main", "text": "ONCE"}),
            ],
        ));

        for _ in 0..3 {
            assert_eq!(
                mock_llm_response(&request_with_scope("q", Some("judge")))
                    .expect("sticky")
                    .text,
                "STICKY"
            );
        }

        assert_eq!(
            mock_llm_response(&request_with_scope("t", Some("main")))
                .expect("once")
                .text,
            "ONCE"
        );
        // The one-shot main entry is gone: a second main call misses (no default
        // bucket to fall to) and, under replay, errors.
        assert!(
            mock_llm_response(&request_with_scope("t2", Some("main"))).is_err(),
            "a consumed once-entry must not replay"
        );
        clear_cli_llm_mock_mode();
    }

    #[test]
    fn strict_scopes_makes_unscoped_aux_a_hard_miss() {
        reset_llm_mock_state();
        install_cli_llm_mock_fixture(v1_fixture(
            true,
            &[serde_json::json!({"scope": "default", "text": "DEFAULT"})],
        ));

        // strictScopes forbids the default fall-through, so a judge call misses.
        assert!(
            mock_llm_response(&request_with_scope("verify", Some("judge"))).is_err(),
            "strict scopes must make an unscoped-aux call a hard miss"
        );
        let receipts = get_llm_mock_receipts();
        assert!(
            receipts.iter().any(|r| r.scope == "judge" && !r.matched),
            "the hard miss must be recorded as an unmatched receipt: {receipts:?}"
        );

        // The default entry was never touched — an explicit default call hits it.
        let def = mock_llm_response(&request_with_scope("x", None)).expect("default");
        assert_eq!(def.text, "DEFAULT");
        clear_cli_llm_mock_mode();
    }
}
