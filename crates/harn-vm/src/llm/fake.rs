//! Deterministic fake LLM provider for streaming/tool-use/error tests.
//!
//! `FakeLlmProvider` plays back a pre-scripted sequence of streamed events
//! (text deltas, tool-call deltas, stop reasons), simulated provider errors,
//! and simulated latency — all without any network I/O. It is designed for
//! tests that need to exercise streaming behavior, tool-use loops,
//! retry-on-failure, and provider error classification deterministically.
//!
//! ## Why not the existing `MockProvider`?
//!
//! `mock::mock_llm_response` is a one-shot synchronous function: it returns
//! a fully-formed `LlmResult` and emits a single delta containing the entire
//! text. That is enough for non-streaming tests but loses the ability to
//! observe per-token streaming order, mid-stream tool-call deltas, and
//! provider-side latency.
//!
//! `FakeLlmProvider` keeps the deterministic, in-process spirit of the mock
//! provider but adds:
//!
//! - **Real streaming.** Each [`FakeLlmEvent::Token`] sends an individual
//!   delta, in script order, through the caller's `delta_tx`.
//! - **Tool-call deltas.** Tool calls become both top-level
//!   `tool_calls` entries and `tool_call` blocks in the result.
//! - **Synthetic errors.** [`FakeLlmTurn::Error`] surfaces as
//!   `VmError::CategorizedError`, byte-for-byte compatible with how a real
//!   provider failure is classified, so retry/backoff paths exercise the
//!   same code under test as in production.
//! - **Simulated latency.** [`FakeLlmTurn::Stalled`] / [`FakeLlmEvent::Stall`]
//!   call `tokio::time::sleep`, which is virtual under
//!   `tokio::time::pause` — tests advance the clock with
//!   `tokio::time::advance(...)` instead of waiting wall-clock time.
//!
//! ## Usage
//!
//! ```rust,ignore
//! # use harn_vm::llm::{FakeLlmEvent, FakeLlmScript, FakeLlmTurn, FakeStopReason, install_fake_llm_script};
//! let script = FakeLlmScript::new()
//!     .push(FakeLlmTurn::stream(vec![
//!         FakeLlmEvent::Token("hello ".into()),
//!         FakeLlmEvent::Token("world".into()),
//!         FakeLlmEvent::Done(FakeStopReason::EndTurn),
//!     ]));
//! let _guard = install_fake_llm_script(script);
//! // run llm_call with provider: "fake" — assertions follow.
//! // _guard, on drop, asserts every scripted turn was consumed.
//! ```

use std::cell::RefCell;
use std::time::Duration;

use crate::llm::api::{DeltaSender, LlmRequestPayload, LlmResult, ProviderTelemetry};
use crate::llm::provider::{LlmProvider, LlmProviderChat};
use crate::value::{ErrorCategory, VmError};

/// One step of a fake LLM script.
#[derive(Clone, Debug)]
pub enum FakeLlmTurn {
    /// Stream a sequence of events to the caller. Events fire in order;
    /// `Done` (or end of vec) finalizes the turn into an `LlmResult`.
    Stream(Vec<FakeLlmEvent>),
    /// Surface a synthetic provider error. Mirrors the way real providers
    /// fail so retry/classification logic exercises the same path.
    Error(FakeLlmError),
    /// Pause for `duration` before producing the next turn. Honors
    /// `tokio::time::pause` — tests advance virtual time with
    /// `tokio::time::advance(duration)`.
    Stalled(Duration),
}

impl FakeLlmTurn {
    /// Convenience constructor for a streaming turn.
    pub fn stream(events: impl IntoIterator<Item = FakeLlmEvent>) -> Self {
        Self::Stream(events.into_iter().collect())
    }

    /// Convenience constructor for an error turn.
    pub fn error(category: ErrorCategory, message: impl Into<String>) -> Self {
        Self::Error(FakeLlmError {
            category,
            message: message.into(),
            retry_after_ms: None,
        })
    }
}

/// One event within a streaming turn.
#[derive(Clone, Debug)]
pub enum FakeLlmEvent {
    /// Append `text` to the assistant's running text and emit it as a
    /// streaming delta.
    Token(String),
    /// Add a tool call to the result. The fake provider produces both a
    /// top-level `tool_calls` entry and a matching `tool_call` block.
    ToolCallDelta {
        id: String,
        name: String,
        arguments: serde_json::Value,
    },
    /// Pause mid-stream. Like `FakeLlmTurn::Stalled`, this honors
    /// `tokio::time::pause`.
    Stall(Duration),
    /// Finalize the turn with the given stop reason. Optional — when
    /// omitted, the turn ends with `EndTurn` after the last event.
    Done(FakeStopReason),
}

/// Stop reason for a streaming turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FakeStopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    StopSequence,
    Custom(String),
}

impl FakeStopReason {
    fn as_str(&self) -> &str {
        match self {
            Self::EndTurn => "end_turn",
            Self::ToolUse => "tool_use",
            Self::MaxTokens => "max_tokens",
            Self::StopSequence => "stop_sequence",
            Self::Custom(value) => value.as_str(),
        }
    }
}

/// Synthetic provider failure surfaced by [`FakeLlmTurn::Error`].
#[derive(Clone, Debug)]
pub struct FakeLlmError {
    pub category: ErrorCategory,
    pub message: String,
    /// Optional `retry-after` hint embedded in the error message so that
    /// `agent_observe::extract_retry_after_ms` — the same parser used for
    /// real HTTP 429s — surfaces the value on the caller's thrown dict.
    pub retry_after_ms: Option<u64>,
}

impl FakeLlmError {
    pub fn new(category: ErrorCategory, message: impl Into<String>) -> Self {
        Self {
            category,
            message: message.into(),
            retry_after_ms: None,
        }
    }

    pub fn with_retry_after_ms(mut self, ms: u64) -> Self {
        self.retry_after_ms = Some(ms);
        self
    }
}

/// A scripted sequence of fake LLM turns.
///
/// Turns are consumed FIFO: the first turn matches the first call, the
/// second turn matches the second call, etc. The drop guard returned by
/// [`install_fake_llm_script`] asserts on drop that every turn was
/// consumed, surfacing accidentally-mismatched test expectations.
#[derive(Clone, Debug, Default)]
pub struct FakeLlmScript {
    pub turns: Vec<FakeLlmTurn>,
}

impl FakeLlmScript {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a single-turn script that streams the given events.
    pub fn streaming(events: impl IntoIterator<Item = FakeLlmEvent>) -> Self {
        Self {
            turns: vec![FakeLlmTurn::stream(events)],
        }
    }

    /// Build a single-turn script that surfaces an error.
    pub fn erroring(category: ErrorCategory, message: impl Into<String>) -> Self {
        Self {
            turns: vec![FakeLlmTurn::error(category, message)],
        }
    }

    /// Append a turn to the script.
    pub fn push(mut self, turn: FakeLlmTurn) -> Self {
        self.turns.push(turn);
        self
    }
}

/// One captured request from a fake LLM call. Tests use these to assert
/// the provider was invoked with the expected prompt / tools / model.
#[derive(Clone, Debug)]
pub struct FakeLlmCall {
    pub provider: String,
    pub model: String,
    pub system: Option<String>,
    pub messages: Vec<serde_json::Value>,
    pub native_tools: Option<Vec<serde_json::Value>>,
    pub stream: bool,
}

impl FakeLlmCall {
    fn from_request(request: &LlmRequestPayload) -> Self {
        Self {
            provider: request.provider.clone(),
            model: request.model.clone(),
            system: request.system.clone(),
            messages: request.messages.clone(),
            native_tools: request.native_tools.clone(),
            stream: request.stream,
        }
    }
}

thread_local! {
    static FAKE_LLM_TURNS: RefCell<Vec<FakeLlmTurn>> = const { RefCell::new(Vec::new()) };
    static FAKE_LLM_CALLS: RefCell<Vec<FakeLlmCall>> = const { RefCell::new(Vec::new()) };
}

/// Install a script as the active fake-LLM playback queue and return a
/// drop guard. The guard asserts on drop that every turn was consumed
/// and resets the thread-local state.
///
/// Panics on drop (not during the test body) if turns remain or no script
/// was active to begin with — both signal a mismatch between the test's
/// expectations and what the code under test actually did.
#[must_use = "FakeLlmGuard asserts on drop; bind it to a `_guard` local"]
pub fn install_fake_llm_script(script: FakeLlmScript) -> FakeLlmGuard {
    FAKE_LLM_TURNS.with(|turns| {
        let mut turns = turns.borrow_mut();
        assert!(
            turns.is_empty(),
            "FakeLlmProvider: a script is already installed; drop the previous guard before installing a new one"
        );
        *turns = script.turns;
    });
    FAKE_LLM_CALLS.with(|calls| calls.borrow_mut().clear());
    FakeLlmGuard { _priv: () }
}

/// Captured fake-LLM calls observed since the active guard was installed.
pub fn fake_llm_captured_calls() -> Vec<FakeLlmCall> {
    FAKE_LLM_CALLS.with(|calls| calls.borrow().clone())
}

/// Drop guard returned by [`install_fake_llm_script`].
///
/// Asserts on drop that the script was fully consumed, then resets the
/// thread-local state. Drop runs even on panic, so a failed assertion in
/// the test body still cleans up.
#[must_use]
pub struct FakeLlmGuard {
    _priv: (),
}

impl Drop for FakeLlmGuard {
    fn drop(&mut self) {
        let remaining = FAKE_LLM_TURNS.with(|turns| std::mem::take(&mut *turns.borrow_mut()));
        FAKE_LLM_CALLS.with(|calls| calls.borrow_mut().clear());
        // If we are unwinding from a prior panic, don't double-panic —
        // the original failure is more informative.
        if std::thread::panicking() {
            return;
        }
        assert!(
            remaining.is_empty(),
            "FakeLlmProvider script had {} unconsumed turn(s); did the code under test make fewer LLM calls than expected?",
            remaining.len()
        );
    }
}

/// Take the next turn from the active script, or return an explanatory
/// error if no script is installed / the script is exhausted.
fn take_next_turn(request: &LlmRequestPayload) -> Result<FakeLlmTurn, VmError> {
    FAKE_LLM_CALLS.with(|calls| {
        calls.borrow_mut().push(FakeLlmCall::from_request(request));
    });
    FAKE_LLM_TURNS.with(|turns| {
        let mut turns = turns.borrow_mut();
        if turns.is_empty() {
            Err(VmError::Runtime(
                "FakeLlmProvider: no script installed (or script exhausted) — install_fake_llm_script() must precede llm_call(provider: \"fake\")".to_string()
            ))
        } else {
            Ok(turns.remove(0))
        }
    })
}

/// Build the synthetic `retry-after` hint that the agent layer parses out
/// of error messages. Mirrors `mock::mock_error_to_vm_error`.
fn fake_error_to_vm_error(err: &FakeLlmError) -> VmError {
    let message = match err.retry_after_ms {
        Some(ms) => {
            let secs = (ms as f64 / 1000.0).max(0.0);
            let sep = if err.message.is_empty() || err.message.ends_with('\n') {
                ""
            } else {
                "\n"
            };
            format!("{}{sep}retry-after: {secs}\n", err.message)
        }
        None => err.message.clone(),
    };
    VmError::CategorizedError {
        message,
        category: err.category.clone(),
    }
}

/// Zero-cost unit struct registered as the `"fake"` provider.
pub(crate) struct FakeLlmProvider;

impl LlmProvider for FakeLlmProvider {
    fn name(&self) -> &'static str {
        "fake"
    }

    fn requires_model(&self) -> bool {
        false
    }
}

impl LlmProviderChat for FakeLlmProvider {
    fn chat<'a>(
        &'a self,
        request: &'a LlmRequestPayload,
        delta_tx: Option<DeltaSender>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<LlmResult, VmError>> + 'a>> {
        Box::pin(self.chat_impl(request, delta_tx))
    }
}

impl FakeLlmProvider {
    /// Whether the request should route through the fake provider.
    pub(crate) fn should_intercept(provider: &str) -> bool {
        provider == "fake"
    }

    pub(crate) async fn chat_impl(
        &self,
        request: &LlmRequestPayload,
        delta_tx: Option<DeltaSender>,
    ) -> Result<LlmResult, VmError> {
        loop {
            let turn = take_next_turn(request)?;
            match turn {
                FakeLlmTurn::Stalled(duration) => {
                    if !duration.is_zero() {
                        tokio::time::sleep(duration).await;
                    }
                    // Loop to consume the next turn. `Stalled` is a delay
                    // marker, not a producer — it gates the *following*
                    // turn rather than ending the call on its own.
                }
                FakeLlmTurn::Error(err) => {
                    return Err(fake_error_to_vm_error(&err));
                }
                FakeLlmTurn::Stream(events) => {
                    return play_stream(request, events, delta_tx).await;
                }
            }
        }
    }
}

async fn play_stream(
    request: &LlmRequestPayload,
    events: Vec<FakeLlmEvent>,
    delta_tx: Option<DeltaSender>,
) -> Result<LlmResult, VmError> {
    let mut text = String::new();
    let mut tool_calls: Vec<serde_json::Value> = Vec::new();
    let mut blocks: Vec<serde_json::Value> = Vec::new();
    let mut stop_reason: Option<FakeStopReason> = None;
    let mut next_tool_index: usize = 1;
    // Mirror the real-transport mid-stream abort so tests can exercise
    // `schema_stream_abort` end-to-end without standing up a live SSE
    // server.
    let mut schema_watch = crate::llm::api::StreamSchemaWatch::from_payload(request);

    for event in events {
        match event {
            FakeLlmEvent::Token(chunk) => {
                if let Some(tx) = delta_tx.as_ref() {
                    let _ = tx.send(chunk.clone());
                }
                text.push_str(&chunk);
                if let Some(watch) = schema_watch.as_mut() {
                    if let Some(abort) = watch.observe(&chunk) {
                        return Err(abort.into_vm_error());
                    }
                }
            }
            FakeLlmEvent::ToolCallDelta {
                id,
                name,
                arguments,
            } => {
                let id = if id.is_empty() {
                    let auto = format!("fake_call_{next_tool_index}");
                    next_tool_index += 1;
                    auto
                } else {
                    id
                };
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
            FakeLlmEvent::Stall(duration) => {
                if !duration.is_zero() {
                    tokio::time::sleep(duration).await;
                }
            }
            FakeLlmEvent::Done(reason) => {
                stop_reason = Some(reason);
                break;
            }
        }
    }

    if !text.is_empty() {
        // Place the consolidated text block at the front so call sites that
        // peek at `blocks[0]` see prose before any tool-call blocks.
        let text_block = serde_json::json!({
            "type": "output_text",
            "text": text,
            "visibility": "public",
        });
        blocks.insert(0, text_block);
    }

    let stop_reason = stop_reason.unwrap_or(if tool_calls.is_empty() {
        FakeStopReason::EndTurn
    } else {
        FakeStopReason::ToolUse
    });

    Ok(LlmResult {
        text,
        tool_calls,
        input_tokens: count_input_tokens(&request.messages),
        output_tokens: 0,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        model: request.model.clone(),
        provider: "fake".to_string(),
        thinking: None,
        thinking_summary: None,
        stop_reason: Some(stop_reason.as_str().to_string()),
        blocks,
        logprobs: Vec::new(),
        telemetry: ProviderTelemetry::default(),
    })
}

/// Cheap deterministic input-token estimate based on the rendered prompt
/// length. Matches the spirit of `mock_llm_response`'s estimate without
/// pulling in the mock's pattern-matcher infrastructure.
fn count_input_tokens(messages: &[serde_json::Value]) -> i64 {
    fn collect(value: &serde_json::Value, out: &mut String) {
        match value {
            serde_json::Value::String(text) => {
                out.push_str(text);
                out.push('\n');
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    collect(item, out);
                }
            }
            serde_json::Value::Object(map) => {
                for value in map.values() {
                    collect(value, out);
                }
            }
            _ => {}
        }
    }
    let mut buf = String::new();
    for message in messages {
        collect(message, &mut buf);
    }
    buf.len() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::api::{LlmApiMode, ThinkingConfig};
    use crate::llm::api::{LlmRequestPayload, LlmRouteFallback, OutputFormat};

    fn fake_request() -> LlmRequestPayload {
        LlmRequestPayload {
            provider: "fake".to_string(),
            model: "fake-model".to_string(),
            api_key: String::new(),
            api_mode: LlmApiMode::ChatCompletions,
            fallback_chain: Vec::new(),
            route_fallbacks: Vec::<LlmRouteFallback>::new(),
            messages: vec![serde_json::json!({"role": "user", "content": "hello"})],
            system: None,
            max_tokens: 64,
            temperature: None,
            top_p: None,
            top_k: None,
            logprobs: false,
            top_logprobs: None,
            stop: None,
            seed: None,
            frequency_penalty: None,
            presence_penalty: None,
            output_format: OutputFormat::Text,
            response_format: None,
            json_schema: None,
            output_schema: None,
            schema_stream_abort: false,
            thinking: ThinkingConfig::Disabled,
            anthropic_beta_features: Vec::new(),
            vision: false,
            native_tools: None,
            provider_tools: Vec::new(),
            tool_choice: None,
            cache: false,
            timeout: None,
            stream: true,
            provider_overrides: None,
            previous_response_id: None,
            store: None,
            background: None,
            truncation: None,
            compact: None,
            include: None,
            max_tool_calls: None,
            prefill: None,
            session_id: None,
            reminder_lifecycle: Vec::new(),
        }
    }

    fn current_thread_runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .start_paused(false)
            .build()
            .expect("runtime")
    }

    #[test]
    fn streaming_turn_emits_deltas_in_order() {
        let runtime = current_thread_runtime();
        let _guard = install_fake_llm_script(FakeLlmScript::streaming(vec![
            FakeLlmEvent::Token("hello ".into()),
            FakeLlmEvent::Token("world".into()),
            FakeLlmEvent::Done(FakeStopReason::EndTurn),
        ]));

        runtime.block_on(async {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
            let result = FakeLlmProvider
                .chat_impl(&fake_request(), Some(tx))
                .await
                .expect("fake call should succeed");

            let mut deltas = Vec::new();
            while let Ok(delta) = rx.try_recv() {
                deltas.push(delta);
            }

            assert_eq!(deltas, vec!["hello ".to_string(), "world".to_string()]);
            assert_eq!(result.text, "hello world");
            assert_eq!(result.provider, "fake");
            assert_eq!(result.stop_reason.as_deref(), Some("end_turn"));
            assert_eq!(result.blocks.len(), 1);
            assert_eq!(result.blocks[0]["type"].as_str(), Some("output_text"));
            assert_eq!(result.blocks[0]["text"].as_str(), Some("hello world"));
        });
        assert_eq!(fake_llm_captured_calls().len(), 1);
    }

    #[test]
    fn tool_call_deltas_become_tool_calls_and_blocks() {
        let runtime = current_thread_runtime();
        let _guard = install_fake_llm_script(FakeLlmScript::streaming(vec![
            FakeLlmEvent::Token("calling tool".into()),
            FakeLlmEvent::ToolCallDelta {
                id: String::new(),
                name: "search".into(),
                arguments: serde_json::json!({"q": "harn"}),
            },
            FakeLlmEvent::Done(FakeStopReason::ToolUse),
        ]));

        runtime.block_on(async {
            let result = FakeLlmProvider
                .chat_impl(&fake_request(), None)
                .await
                .expect("fake call should succeed");

            assert_eq!(result.tool_calls.len(), 1);
            assert_eq!(result.tool_calls[0]["name"].as_str(), Some("search"));
            assert_eq!(result.tool_calls[0]["id"].as_str(), Some("fake_call_1"));
            assert_eq!(
                result.tool_calls[0]["arguments"]["q"].as_str(),
                Some("harn")
            );
            assert_eq!(result.stop_reason.as_deref(), Some("tool_use"));
            // First block is the consolidated text, second is the tool call.
            assert_eq!(result.blocks[0]["type"].as_str(), Some("output_text"));
            assert_eq!(result.blocks[1]["type"].as_str(), Some("tool_call"));
            assert_eq!(result.blocks[1]["name"].as_str(), Some("search"));
        });
    }

    #[test]
    fn error_turn_returns_categorized_error() {
        let runtime = current_thread_runtime();
        let _guard = install_fake_llm_script(FakeLlmScript::erroring(
            ErrorCategory::RateLimit,
            "throttled",
        ));

        runtime.block_on(async {
            let err = FakeLlmProvider
                .chat_impl(&fake_request(), None)
                .await
                .expect_err("fake error turn should fail");
            match err {
                VmError::CategorizedError { message, category } => {
                    assert_eq!(category, ErrorCategory::RateLimit);
                    assert!(
                        message.contains("throttled"),
                        "error message should pass through: {message}"
                    );
                }
                other => panic!("expected CategorizedError, got {other:?}"),
            }
        });
    }

    #[test]
    fn error_turn_embeds_retry_after_hint() {
        let runtime = current_thread_runtime();
        let _guard = install_fake_llm_script(FakeLlmScript::default().push(FakeLlmTurn::Error(
            FakeLlmError::new(ErrorCategory::RateLimit, "throttled").with_retry_after_ms(2_500),
        )));

        runtime.block_on(async {
            let err = FakeLlmProvider
                .chat_impl(&fake_request(), None)
                .await
                .expect_err("fake error turn should fail");
            let VmError::CategorizedError { message, .. } = err else {
                panic!("expected CategorizedError");
            };
            assert!(
                message.contains("retry-after: 2.5"),
                "retry-after hint should be present in synthetic message: {message}"
            );
        });
    }

    #[test]
    fn stalled_turn_advances_under_paused_clock() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .start_paused(true)
            .build()
            .expect("paused runtime");
        let _guard = install_fake_llm_script(
            FakeLlmScript::default()
                .push(FakeLlmTurn::Stalled(Duration::from_mins(1)))
                .push(FakeLlmTurn::stream(vec![
                    FakeLlmEvent::Token("done".into()),
                    FakeLlmEvent::Done(FakeStopReason::EndTurn),
                ])),
        );

        runtime.block_on(async {
            let request = fake_request();
            let chat = FakeLlmProvider.chat_impl(&request, None);
            tokio::pin!(chat);

            // The chat future is parked on `tokio::time::sleep(60s)`. With
            // a paused clock and no advance, polling once should leave it
            // pending — proving real wall-clock is not required.
            let polled = futures::poll!(&mut chat);
            assert!(
                matches!(polled, std::task::Poll::Pending),
                "fake provider should be parked on the stall"
            );

            tokio::time::advance(Duration::from_mins(1)).await;
            let result = chat.await.expect("after advance, fake call resolves");
            assert_eq!(result.text, "done");
        });
    }

    #[test]
    fn multiple_turns_consumed_in_fifo_order() {
        let runtime = current_thread_runtime();
        let _guard = install_fake_llm_script(
            FakeLlmScript::default()
                .push(FakeLlmTurn::stream(vec![
                    FakeLlmEvent::Token("first".into()),
                    FakeLlmEvent::Done(FakeStopReason::EndTurn),
                ]))
                .push(FakeLlmTurn::stream(vec![
                    FakeLlmEvent::Token("second".into()),
                    FakeLlmEvent::Done(FakeStopReason::EndTurn),
                ])),
        );

        runtime.block_on(async {
            let first = FakeLlmProvider
                .chat_impl(&fake_request(), None)
                .await
                .expect("first call");
            let second = FakeLlmProvider
                .chat_impl(&fake_request(), None)
                .await
                .expect("second call");
            assert_eq!(first.text, "first");
            assert_eq!(second.text, "second");
        });

        let calls = fake_llm_captured_calls();
        assert_eq!(calls.len(), 2);
        assert!(calls.iter().all(|c| c.provider == "fake"));
    }

    #[test]
    #[should_panic(expected = "no script installed")]
    fn calling_without_script_panics_with_explanatory_error() {
        let runtime = current_thread_runtime();
        // No guard — exercise the bare error path.
        runtime
            .block_on(async {
                FakeLlmProvider
                    .chat_impl(&fake_request(), None)
                    .await
                    .map_err(|e| e.to_string())
            })
            .unwrap();
    }

    #[test]
    #[should_panic(expected = "unconsumed turn")]
    fn drop_guard_asserts_on_unused_turns() {
        let guard =
            install_fake_llm_script(FakeLlmScript::default().push(FakeLlmTurn::stream(vec![
                FakeLlmEvent::Done(FakeStopReason::EndTurn),
            ])));
        // Without making the call, the guard should panic on drop.
        drop(guard);
    }
}
