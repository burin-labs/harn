//! LLM call observability: retry logic, transcript dumps, span annotation,
//! and the `observed_llm_call` wrapper extracted from `agent.rs`.
//!
//! # Transcript log shape
//!
//! Writes go to `$HARN_LLM_TRANSCRIPT_DIR/llm_transcript.jsonl`, one JSON
//! object per line, append-only. Consumers replay the events in order to
//! reconstruct the model's context at any iteration.
//!
//! Every event also carries a `type` discriminator, a `timestamp` (Unix
//! `secs.millis`), and a `span_id` (the current tracing span, may be
//! null) — these common fields are omitted from the per-event field lists
//! below.
//!
//! Event types:
//!
//! - `system_prompt` `{content, hash}` — emitted once when a new system
//!   prompt takes effect. Dedup'd via a rolling hash so consecutive
//!   identical prompts are not re-emitted.
//! - `tool_schemas` `{schemas, hash}` — same shape for the tool schema
//!   list; each request re-uses the last-emitted set.
//! - `message` `{role, content, iteration?}` — single message appended to
//!   the visible conversation. Emitted every time a message lands in the
//!   transcript (user task, nudge, assistant reply, tool result, host
//!   push).
//! - `routing_decision` `{call_id, iteration, policy, requested_quality,
//!   selected_provider, selected_model, fallback_chain, alternatives}` —
//!   emitted once before `provider_call_request` whenever a routing
//!   decision was attached to the call (model/provider selection,
//!   fallback chain, and the considered alternatives).
//! - `provider_call_request` core `{call_id, iteration, model, provider,
//!   max_tokens, temperature, tool_choice, tool_format}` — slim metadata
//!   for a single model call. No `messages`, `system`, or `tool_schemas`
//!   fields; those are reconstructable from prior events. Also carries
//!   diagnostics `{thinking, native_tool_count, message_count,
//!   structural_experiment, route_policy, fallback_chain,
//!   routing_decision}`.
//!   Set `HARN_LLM_TRANSCRIPT_VERBOSE=1` to include a `request_snapshot`
//!   object with the exact system prompt, message list, and tool schemas
//!   attached to each request for debugging provider-context issues.
//! - `provider_call_response` core `{call_id, iteration, model, provider,
//!   text, tool_calls, parsed_tool_calls, input_tokens, output_tokens,
//!   response_ms}`. `tool_calls` is the provider-native tool-call array
//!   (empty for text-format local models); `parsed_tool_calls` is the
//!   merged view (native when present, otherwise the calls parsed out of
//!   the inline tagged `<tool_call>` blocks in `text`) so the record is
//!   self-describing for text-format runs. Also carries diagnostics
//!   `{cost_usd, cache_* (cache_read_tokens, cache_write_tokens,
//!   cache_creation_input_tokens, cache_hit_ratio, cache_savings_usd,
//!   cache_hit), thinking, thinking_summary, provider_telemetry,
//!   structural_experiment}`.
//! - `interpreted_response` `{call_id, iteration, tool_format, prose,
//!   tool_calls, tool_parse_errors}` — post-parse view of the last
//!   assistant turn.
//!
//! To reconstruct the prompt sent at `call_id=X`, replay events in order
//! and track the last `system_prompt`, the last `tool_schemas`, and every
//! `message` up to (but not including) the matching `provider_call_request`.

use std::cell::RefCell;
use std::sync::Arc;

use crate::event_log::EventLog;
use crate::value::VmError;

use super::api::{vm_call_llm_full_streaming, vm_call_llm_full_streaming_offthread, DeltaSender};
use super::trace::{trace_llm_call, LlmTraceEntry};

use super::agent_tools::next_call_id;

thread_local! {
    /// Last-emitted hash for the current transcript's system prompt and
    /// tool schemas. Used to dedup identical payloads across turns so we
    /// write them once per stage instead of once per request.
    static LAST_SYSTEM_PROMPT_HASH: RefCell<Option<u64>> = const { RefCell::new(None) };
    static LAST_TOOL_SCHEMAS_HASH: RefCell<Option<u64>> = const { RefCell::new(None) };
    static TRANSCRIPT_DIR_STACK: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

fn reset_transcript_dedup() {
    LAST_SYSTEM_PROMPT_HASH.with(|hash| *hash.borrow_mut() = None);
    LAST_TOOL_SCHEMAS_HASH.with(|hash| *hash.borrow_mut() = None);
}

pub(super) fn push_llm_transcript_dir(dir: &str) {
    if dir.trim().is_empty() {
        return;
    }
    TRANSCRIPT_DIR_STACK.with(|stack| stack.borrow_mut().push(dir.to_string()));
    reset_transcript_dedup();
}

pub(super) fn pop_llm_transcript_dir() {
    TRANSCRIPT_DIR_STACK.with(|stack| {
        stack.borrow_mut().pop();
    });
    reset_transcript_dedup();
}

fn current_transcript_dir() -> Option<String> {
    let stacked = TRANSCRIPT_DIR_STACK.with(|stack| stack.borrow().last().cloned());
    if stacked.is_some() {
        return stacked;
    }
    std::env::var("HARN_LLM_TRANSCRIPT_DIR")
        .ok()
        .filter(|d| !d.is_empty())
}

fn hash_str(value: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

fn hash_json(value: &serde_json::Value) -> u64 {
    // Dedup only needs intra-process stability; built-in key ordering is fine.
    let encoded = serde_json::to_string(value).unwrap_or_default();
    hash_str(&encoded)
}

fn verbose_llm_transcript_enabled() -> bool {
    std::env::var("HARN_LLM_TRANSCRIPT_VERBOSE")
        .ok()
        .map(|value| {
            let normalized = value.trim().to_ascii_lowercase();
            matches!(normalized.as_str(), "1" | "true" | "yes" | "on" | "full")
        })
        .unwrap_or(false)
}

/// Classify whether a VmError from an LLM call is transient and worth
/// retrying.
///
/// Priority:
/// 1. `CategorizedError` → consult `ErrorCategory::is_transient()` for the
///    authoritative, structured answer.
/// 2. `Thrown(String)` / `Runtime(String)` → first try to *derive* a
///    category via the shared `classify_error_message` machinery (so
///    HTTP-status patterns and well-known provider identifiers stay in
///    one place), then fall back to a small substring list for error
///    shapes that don't carry a status code (network failure phrases).
pub(super) fn is_retryable_llm_error(err: &VmError) -> bool {
    use crate::value::{classify_error_message, ErrorCategory};
    let msg = match err {
        VmError::CategorizedError { category, message } => {
            let llm_info = crate::llm::api::classify_llm_error(category.clone(), message);
            return if llm_info.reason == crate::llm::api::LlmErrorReason::Unknown {
                category.is_transient()
            } else {
                llm_info.kind == crate::llm::api::LlmErrorKind::Transient
            };
        }
        VmError::Thrown(crate::value::VmValue::Dict(d)) => {
            if let Some(kind) = d.get("kind").map(|v| v.display()) {
                return kind == "transient";
            }
            if let Some(category) = d.get("category").map(|v| v.display()) {
                return ErrorCategory::parse(&category).is_transient();
            }
            return false;
        }
        VmError::Thrown(crate::value::VmValue::String(s)) => s.as_ref(),
        VmError::Runtime(s) => s.as_str(),
        _ => return false,
    };
    let category = classify_error_message(msg);
    let llm_info = crate::llm::api::classify_llm_error(category, msg);
    if llm_info.kind == crate::llm::api::LlmErrorKind::Transient {
        return true;
    }
    if llm_info.reason != crate::llm::api::LlmErrorReason::Unknown {
        return false;
    }
    let derived = classify_error_message(msg);
    if derived != ErrorCategory::Generic {
        return derived.is_transient();
    }
    // Fallback for retryable shapes that don't carry a status code.
    let lower = msg.to_lowercase();
    lower.contains("too many requests")
        || lower.contains("rate limit")
        || lower.contains("overloaded")
        || lower.contains("service unavailable")
        || lower.contains("bad gateway")
        || lower.contains("gateway timeout")
        || lower.contains("timed out")
        || lower.contains("timeout")
        || lower.contains("delivered no content")
        || lower.contains("eof")
}

/// Whether an LLM-call failure is a transport-level *network* failure
/// (connection refused/reset, DNS failure, dropped link, request timeout).
/// Feeds the per-route circuit breaker together with
/// [`is_overloaded_llm_error`].
///
/// Deliberately excludes `RateLimit` (429): a 429 means the link is healthy and
/// the provider is throttling us; that is handled by the rate limiter's cooldown
/// and Retry-After, not by the breaker. Generic `ServerError` (500/502) is
/// likewise the provider's fault on a reachable link and must not trip the
/// breaker either; provider *overload* (529/503) is the one server-side class
/// that does, via the separate overload predicate.
pub(super) fn is_network_failure_llm_error(err: &VmError) -> bool {
    let (category, message) = match err {
        VmError::CategorizedError { category, message } => (category.clone(), message.clone()),
        VmError::Thrown(crate::value::VmValue::String(s)) => {
            (crate::value::classify_error_message(s), s.to_string())
        }
        VmError::Runtime(s) => (crate::value::classify_error_message(s), s.clone()),
        _ => return false,
    };
    let reason = crate::llm::api::classify_llm_error(category, &message).reason;
    matches!(
        reason,
        crate::llm::api::LlmErrorReason::NetworkError | crate::llm::api::LlmErrorReason::Timeout
    )
}

/// Whether an LLM-call failure says the provider itself is shedding load
/// (HTTP 529 / 503, Anthropic `overloaded_error`). Distinct from a 429 — the
/// client hasn't exceeded a quota — and from a generic 500/502, which is a
/// single-request server fault. Overload is a provider-wide condition, so it
/// feeds BOTH the per-route breaker (fail fast instead of burning the retry
/// budget) and the shared cooldown (N parallel agents back off together
/// instead of stampeding the overloaded provider).
pub(super) fn is_overloaded_llm_error(err: &VmError) -> bool {
    crate::value::error_to_category(err) == crate::value::ErrorCategory::Overloaded
}

/// L0 detection: classify an LLM-call failure into a rate-governor throttle
/// signal from the STRUCTURED error category (never a raw log string), reusing
/// the same category logic the cooldown/breaker seams above use so the governor
/// agrees with the runtime's own routing. Returns `None` for non-throttle
/// failures (network/auth/context/generic 5xx). A 429 → `RateLimit429`; a
/// provider overload (529/503/`overloaded_error`) → `Overloaded`. The
/// empty-under-load signal is detected separately (on the `Ok` empty path)
/// because it is not a thrown error.
fn governor_throttle_signal_for_error(
    err: &VmError,
) -> Option<crate::llm::rate_governor::ThrottleSignal> {
    use crate::llm::rate_governor::ThrottleSignal;
    let category = crate::value::error_to_category(err);
    if category == crate::value::ErrorCategory::Overloaded {
        return Some(ThrottleSignal::Overloaded);
    }
    let rate_limited = crate::llm::api::classify_llm_error(category, &err.to_string()).reason
        == crate::llm::api::LlmErrorReason::RateLimit;
    if rate_limited {
        return Some(ThrottleSignal::RateLimit429);
    }
    None
}

/// Estimated (input + output) tokens for a call, for the governor's TPM bucket.
/// Reuses the same gross-token projection the route limiter charges, so the two
/// agree. `0` when unprojectable.
fn governor_estimated_tokens(opts: &super::api::LlmCallOptions) -> u64 {
    let projection = super::cost::project_llm_call_cost(opts, 0.0);
    (projection.projected_input_tokens.max(0) + projection.projected_output_tokens.max(0)) as u64
}

/// Wait behind the rate governor until it admits this call. `Wait`/`CircuitOpen`
/// resolve to a bounded back-off (honoring the mock clock) so retries pace
/// themselves instead of blind-firing at a throttled provider.
///
/// Returns `true` when the governor RESERVED an in-flight slot (the outcome path
/// MUST then release it exactly once) and `false` when it did not — either the
/// flag is off, or the admission cap was hit while the circuit was still OPEN.
/// The cap exists for the same reason the durable rate limiter caps its backoff:
/// a governor should pace, not hang. On a cap-hit-while-OPEN we proceed WITHOUT a
/// reservation, so the outcome path skips the release and the in-flight count
/// stays balanced; a real 429/overload then re-feeds the governor and the normal
/// retry/escalation path, exactly as an uncapped limiter would after its clamp.
async fn await_governor_admission(provider: &str, org_key: &str, est_tokens: u64) -> bool {
    use crate::llm::rate_governor::{gate, GateOutcome};
    if !crate::llm::rate_governor::enabled() {
        return false;
    }
    const GOVERNOR_MAX_ADMISSION_WAITS: usize = 256;
    for _ in 0..GOVERNOR_MAX_ADMISSION_WAITS {
        match gate(provider, org_key, est_tokens) {
            GateOutcome::Proceed => return true,
            GateOutcome::Wait(d) | GateOutcome::CircuitOpen(d) => {
                crate::clock_mock::sleep(d).await;
            }
        }
    }
    // Cap hit: one last gate. Proceed reserves a slot; a persisting OPEN does
    // not — we fall through unreserved rather than hammer or hang.
    matches!(gate(provider, org_key, est_tokens), GateOutcome::Proceed)
}

/// Release the governor slot reserved by [`await_governor_admission`] and record
/// the call's outcome (AIMD + circuit + L0 `provider_throttle` emission). No-op
/// when the flag is off. Runs exactly once per gated attempt.
fn record_governor_call_outcome(
    provider: &str,
    org_key: &str,
    reserved: bool,
    llm_result: &Result<super::api::LlmResult, VmError>,
) {
    use crate::llm::rate_governor::{self, GovernorOutcome, ThrottleSignal};
    // No reservation → nothing to release. This happens when the flag is off, or
    // the admission cap was hit while the circuit stayed OPEN. Skipping keeps the
    // in-flight count balanced.
    if !reserved {
        return;
    }
    // Classify the outcome (and the throttle signal, if any) BEFORE recording,
    // so the empty-under-load heuristic reads the circuit state as it was during
    // the call — not after this outcome mutates it.
    let (outcome, throttle) = match llm_result {
        Ok(result) => {
            // Empty-under-load: billed output but committed nothing, WHILE this
            // provider is already throttled → soft-throttle, not capability.
            let committed_nothing = result.text.is_empty()
                && result.tool_calls.is_empty()
                && result.thinking.as_deref().map(str::is_empty).unwrap_or(true);
            let empty_billed = committed_nothing && result.output_tokens > 0;
            if empty_billed && rate_governor::provider_already_throttled(provider, org_key) {
                (
                    GovernorOutcome::Throttled {
                        signal: ThrottleSignal::EmptyUnderLoad,
                        retry_after_ms: None,
                    },
                    Some((ThrottleSignal::EmptyUnderLoad, None)),
                )
            } else {
                (GovernorOutcome::Served, None)
            }
        }
        Err(err) => match governor_throttle_signal_for_error(err) {
            Some(signal) => {
                let retry_after_ms = extract_retry_after_ms(err);
                (
                    GovernorOutcome::Throttled {
                        signal,
                        retry_after_ms,
                    },
                    Some((signal, retry_after_ms)),
                )
            }
            None => (GovernorOutcome::Neutral, None),
        },
    };
    // Record the outcome first so the emitted `governor_state` snapshot reflects
    // the governor's REACTION (shrunk concurrency, opened circuit).
    rate_governor::record_outcome(provider, org_key, outcome);
    if let Some((signal, retry_after_ms)) = throttle {
        emit_provider_throttle(provider, org_key, signal, retry_after_ms);
    }
}

/// Emit the L0 `provider_throttle` transcript record + a `governor_state`
/// snapshot record, following the `resolved_dispatch` emit pattern
/// ([`append_llm_transcript_entry`]).
fn emit_provider_throttle(
    provider: &str,
    org_key: &str,
    signal: crate::llm::rate_governor::ThrottleSignal,
    retry_after_ms: Option<u64>,
) {
    let ts = chrono_now();
    append_llm_transcript_entry(&crate::llm::rate_governor::build_throttle_record(
        provider,
        org_key,
        signal,
        None,
        retry_after_ms,
        ts.clone(),
    ));
    if let Some(snapshot) = crate::llm::rate_governor::snapshot(provider, org_key) {
        append_llm_transcript_entry(&crate::llm::rate_governor::build_state_record(
            provider, org_key, &snapshot, ts,
        ));
    }
}

/// Shared-cooldown duration to record for a failed call, or 0 for "no
/// cooldown". Rate-limit (429) failures cool down for the provider's
/// Retry-After when one was sent (there is no meaningful default — catalog
/// rpm limits already pace the route). Overload (529/503) failures cool down
/// for Retry-After too, but fall back to a fixed default because overload
/// responses rarely carry the header and sibling agents must still stop
/// hammering the provider.
pub(super) fn shared_cooldown_ms_for_llm_error(err: &VmError) -> u64 {
    let category = crate::value::error_to_category(err);
    let overloaded = category == crate::value::ErrorCategory::Overloaded;
    let rate_limited = crate::llm::api::classify_llm_error(category, &err.to_string()).reason
        == crate::llm::api::LlmErrorReason::RateLimit;
    if !overloaded && !rate_limited {
        return 0;
    }
    extract_retry_after_ms(err).unwrap_or(if overloaded {
        super::rate_limit::OVERLOAD_COOLDOWN_MS
    } else {
        0
    })
}

/// A *thrown* provider response the agent loop should retry within the
/// empty-completion budget rather than terminate on. Two shapes, both surfaced
/// as a thrown error by the response/transport parsers:
///
/// 1. **Zero-token empty completion** — `completion_tokens=N ... delivered no
///    content` (the transport's billed-but-empty 200 guard). A provider stall,
///    not an answer.
/// 2. **Billed-noncommittal completion** — `... returned billed output
///    (completion_tokens=N) with no dispatchable tool call or answer (upstream
///    contract violation)` (the [`super::api::is_billed_noncommittal_completion`]
///    backstop in `response.rs`/`transport.rs`). The upstream serialized the
///    tool call onto the reasoning channel only and finished *clean*
///    (`stop_reason == "stop"`), so it is THROWN rather than returned as an
///    `Ok` with `stop_reason == "error"`; the `Ok`-arm
///    [`is_errored_actionless_completion`] retry therefore never sees it, and
///    the generic terminal-error classifier ([`is_retryable_llm_error`]) does
///    not match its signature. Routing it through the same empty-completion
///    budget unifies all three unproductive-completion shapes onto one bounded
///    retry path instead of hard-breaking the loop as a silent `provider_error`.
///
/// Matches the message regardless of the `VmError` carrier (`Thrown(String)`
/// from the parsers, or `CategorizedError`/`Runtime` should a future caller
/// re-wrap it).
fn is_empty_completion_retry_error(err: &VmError) -> bool {
    let msg = match err {
        VmError::Thrown(crate::value::VmValue::String(s)) => s.as_ref(),
        VmError::CategorizedError { message, .. } => message.as_str(),
        VmError::Runtime(s) => s.as_str(),
        _ => return false,
    };
    let lower = msg.to_lowercase();
    if !lower.contains("completion_tokens=") {
        return false;
    }
    // (1) zero-token empty completion, or (2) billed-noncommittal completion.
    lower.contains("delivered no content")
        || (lower.contains("no dispatchable tool call or answer")
            && lower.contains("upstream contract violation"))
}

/// A *thrown* failure whose signature says the provider's **native tool-call
/// channel vanished or refused a call for this route** — distinct from the
/// generic provider stall (`delivered no content`), which is a link hiccup that
/// retrying native is the right move for. Two shapes qualify:
///
/// 1. **Billed-noncommittal** — the upstream finished cleanly, billed output, and
///    committed neither a tool call nor visible text: the action was serialized
///    only onto a private reasoning channel. Surfaced by
///    [`super::api::is_billed_noncommittal_completion`] /
///    `billed_noncommittal_completion_error` (`response.rs`). This is the
///    canonical cheap-model "vanishing call" signature; the 2026-06 transcript
///    sweep observed it 13× on `deepinfra/openai/gpt-oss-120b` (tf=native), and
///    the error message itself prescribes a "Harn text/json tool format".
/// 2. **Native function-call protocol refusal** — the provider rejects a native
///    tool request with a 4xx whose body says the function call did not complete
///    (the observed SambaNova shape: HTTP 400 `Model started a function call but
///    did not complete it`). This is a 4xx, so [`is_native_tool_channel_failure`]
///    (5xx/EOF only) deliberately does not match it — but it is unambiguously a
///    broken NATIVE tool channel for the route, not a malformed request from us,
///    so it earns the same one-shot degrade.
///
/// Unlike [`is_empty_completion_retry_error`], this predicate intentionally does
/// NOT match the bare `delivered no content` stall: a stall has no tool-channel
/// fingerprint and is correctly handled by a same-channel retry.
fn is_billed_noncommittal_throw(err: &VmError) -> bool {
    let msg = match err {
        VmError::Thrown(crate::value::VmValue::String(s)) => s.as_ref(),
        VmError::CategorizedError { message, .. } => message.as_str(),
        VmError::Runtime(s) => s.as_str(),
        VmError::Thrown(crate::value::VmValue::Dict(d)) => {
            return d
                .get("message")
                .map(|v| v.display())
                .map(|m| message_is_billed_noncommittal_throw(&m))
                .unwrap_or(false);
        }
        _ => return false,
    };
    message_is_billed_noncommittal_throw(msg)
}

/// String-level half of [`is_billed_noncommittal_throw`], shared by the
/// string-carrier and dict-carrier paths.
fn message_is_billed_noncommittal_throw(msg: &str) -> bool {
    let lower = msg.to_lowercase();
    // (1) billed-noncommittal contract violation (the reasoning-channel-only
    // vanish). Requires the billed-output marker so a generic "contract
    // violation" phrase elsewhere cannot trip it.
    let billed_noncommittal = lower.contains("completion_tokens=")
        && lower.contains("no dispatchable tool call or answer")
        && lower.contains("upstream contract violation");
    // (2) native function-call protocol refusal (SambaNova 400 shape). Keyed on
    // the "function call ... did not complete" fingerprint, not a model name.
    let function_call_refusal = lower.contains("function call")
        && (lower.contains("did not complete") || lower.contains("not complete it"));
    billed_noncommittal || function_call_refusal
}

/// A failure that looks like the *provider's native tool-call channel itself*
/// is broken for this route — not a generic transient hiccup. The marquee case
/// is the documented Ollama leak: the embedded qwen3-family tool-call extractor
/// runs server-side on `/v1/chat/completions`, fails its EOF/parse on the
/// model's output, and Ollama returns an HTTP 500 instead of degrading to raw
/// content (ollama/ollama#14986, #14570 — no opt-out flag). The same shape
/// appears on any serving stack that parses tool calls server-side and 500s /
/// EOFs when the native assumption is wrong, so this is keyed on the failure
/// SIGNATURE, never on a model or provider name.
///
/// Deliberately conservative: only a 5xx `ServerError` OR an EOF/stream-cut
/// transport error carrying a tool-call-parser fingerprint qualifies. A plain
/// 503 "service unavailable" with no parser fingerprint stays an ordinary
/// transient retry (retrying native is the right move — the link, not the
/// channel, hiccuped). This keeps the degrade a genuine last-resort safety net.
fn is_native_tool_channel_failure(err: &VmError) -> bool {
    let msg = match err {
        VmError::Thrown(crate::value::VmValue::String(s)) => s.as_ref(),
        VmError::CategorizedError { message, .. } => message.as_str(),
        VmError::Runtime(s) => s.as_str(),
        VmError::Thrown(crate::value::VmValue::Dict(d)) => {
            return d
                .get("message")
                .map(|v| v.display())
                .map(|m| message_is_native_tool_channel_failure(&m))
                .unwrap_or(false);
        }
        _ => return false,
    };
    message_is_native_tool_channel_failure(msg)
}

/// The string-level half of [`is_native_tool_channel_failure`], split out so the
/// dict-carrier and string-carrier paths share one fingerprint definition.
fn message_is_native_tool_channel_failure(msg: &str) -> bool {
    let lower = msg.to_lowercase();

    // The failure must classify as a server error (5xx) or an EOF / stream cut.
    // A tool-call parser that chokes server-side surfaces as one of these; a
    // 4xx (bad request / auth) or a context-overflow is a different problem the
    // degrade would not fix.
    let server_error = lower.contains("[http_error]")
        || lower.contains("server_error")
        || lower.contains(" 500")
        || lower.contains("status 500")
        || lower.contains("status: 500")
        || lower.contains("502")
        || lower.contains("api_error");
    let stream_cut = lower.contains("unexpected eof")
        || lower.contains("eof while parsing")
        || lower.contains("error decoding stream");
    if !server_error && !stream_cut {
        return false;
    }

    // ...AND it must carry a tool-call-parser fingerprint. This is what
    // distinguishes "the native tool channel is broken for this route" from a
    // generic 500 (a generic 500 stays an ordinary transient native retry).
    lower.contains("tool")
        && (lower.contains("parse")
            || lower.contains("parser")
            || lower.contains("extract")
            || lower.contains("eof"))
}

/// A streaming transport body/read failure after the provider accepted the
/// request. Retrying the identical streaming request can re-hit the same
/// HTTP/SSE body path forever; when the route does not require streaming, the
/// productive fallback is to retry once as a normal request/response call.
fn is_stream_transport_failure(err: &VmError) -> bool {
    let msg = match err {
        VmError::Thrown(crate::value::VmValue::String(s)) => s.as_ref(),
        VmError::CategorizedError { message, .. } => message.as_str(),
        VmError::Runtime(s) => s.as_str(),
        VmError::Thrown(crate::value::VmValue::Dict(d)) => {
            return d
                .get("message")
                .map(|v| v.display())
                .map(|m| message_is_stream_transport_failure(&m))
                .unwrap_or(false);
        }
        _ => return false,
    };
    message_is_stream_transport_failure(msg)
}

fn message_is_stream_transport_failure(msg: &str) -> bool {
    let lower = msg.to_lowercase();
    lower.contains("stream error")
        && (lower.contains("mid-stream")
            || lower.contains("response body")
            || lower.contains("body")
            || lower.contains("error decoding stream")
            || lower.contains("connection reset"))
}

fn can_degrade_stream_transport(opts: &super::api::LlmCallOptions) -> bool {
    opts.stream
        && !crate::llm::capabilities::lookup(&opts.provider, &opts.model).requires_streaming
        && !crate::llm::provider::provider_uses_ollama_messages(&opts.provider, &opts.model)
}

/// A wire-level "success" that carries nothing at all: zero output tokens, no
/// text, no thinking, no tool calls, and no server-side tool-search activity.
/// Observed live (OpenRouter): a provider stall that ends with an empty 200
/// flows back into the agent loop as an empty assistant turn the loop has to
/// burn an iteration recovering from. Treated as a transient provider hiccup
/// and retried in [`observed_llm_call`].
///
/// Token-cap truncations (`stop_reason` length/max_tokens) are excluded — a
/// deterministic cap would just re-truncate on every retry, mirroring the
/// `done_reason == "length"` carve-out on the Ollama NDJSON path.
fn is_zero_token_empty_completion(result: &super::api::LlmResult) -> bool {
    let truncated = matches!(
        result
            .stop_reason
            .as_deref()
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("length" | "max_tokens")
    );
    let has_tool_search_block = result.blocks.iter().any(|block| {
        matches!(
            block.get("type").and_then(|value| value.as_str()),
            Some("tool_search_query") | Some("tool_search_result")
        )
    });
    result.output_tokens == 0
        && result.text.is_empty()
        && result.tool_calls.is_empty()
        && result.thinking.as_deref().unwrap_or("").is_empty()
        && !truncated
        && !has_tool_search_block
}

/// A wire-level "success" whose `stop_reason` reports a provider *error* yet
/// carries no dispatchable action. Observed live (cheap-model eval meter):
/// a generation comes back with `stop_reason == "error"` after only narrating
/// an intended tool call in its text/thinking ("We need to make edit to create
/// tests/...test.cpp...") but with ZERO parsed tool calls. Unlike
/// [`is_zero_token_empty_completion`], the reasoning channel is non-empty, so
/// the zero-token predicate misses it — yet the loop still has no action to run
/// and would otherwise advance on a broken turn (and reply with a generic
/// no-progress nag that never tells the model its turn errored). Treated as a
/// transient provider hiccup and retried in [`observed_llm_call`].
///
/// Token-cap truncations are excluded for the same reason as the zero-token
/// path: a deterministic cap would just re-truncate on every retry. A turn that
/// errored but still dispatched a tool call, or carried a server-side
/// tool-search block, is NOT actionless — the loop has real work and is left
/// untouched.
fn is_errored_actionless_completion(result: &super::api::LlmResult) -> bool {
    let stop = result
        .stop_reason
        .as_deref()
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if stop != "error" {
        return false;
    }
    let has_tool_search_block = result.blocks.iter().any(|block| {
        matches!(
            block.get("type").and_then(|value| value.as_str()),
            Some("tool_search_query") | Some("tool_search_result")
        )
    });
    result.tool_calls.is_empty() && !has_tool_search_block
}

/// Unproductive `Ok` generations the agent loop should retry rather than
/// advance on: the zero-token empty completion (provider stall) and the
/// errored-but-actionless completion (`stop_reason == "error"` with no
/// dispatchable tool call). Both leave the loop with no action to run, so
/// advancing burns an iteration on a broken turn.
fn is_retryable_unproductive_completion(result: &super::api::LlmResult) -> bool {
    is_zero_token_empty_completion(result) || is_errored_actionless_completion(result)
}

/// Extract retry-after delay from an error message if present.
///
/// Supports both forms defined by RFC 7231 §7.1.3:
/// - delta-seconds (integer or fractional)
/// - HTTP-date (IMF-fixdate)
///
/// Returns `None` if no recognizable `retry-after:` header is embedded.
/// HTTP-date values in the past are normalized to 0 ms. Values above
/// `60_000` ms are clamped — callers combine the hint with their own
/// exponential backoff rather than honoring huge provider-requested
/// sleeps verbatim.
pub(super) fn extract_retry_after_ms(err: &VmError) -> Option<u64> {
    let msg = match err {
        VmError::Thrown(crate::value::VmValue::String(s)) => s.as_ref(),
        VmError::Thrown(crate::value::VmValue::Dict(d)) => {
            return d.get("retry_after_ms").and_then(|v| match v {
                crate::value::VmValue::Int(ms) if *ms >= 0 => Some(*ms as u64),
                _ => None,
            });
        }
        VmError::CategorizedError { message, .. } => message.as_str(),
        VmError::Runtime(s) => s.as_str(),
        _ => return None,
    };
    parse_retry_after(msg)
}

/// Parse the value of a `retry-after:` header embedded anywhere in `msg`.
///
/// Exposed for unit tests; the public entry point is
/// `extract_retry_after_ms`.
pub(crate) fn parse_retry_after(msg: &str) -> Option<u64> {
    const MAX_MS: u64 = 60_000;
    let lower = msg.to_lowercase();
    let pos = lower.find("retry-after:")?;
    let after = &msg[pos + "retry-after:".len()..];
    // End at CRLF so we don't grab a neighboring header.
    let end = after.find(['\r', '\n']).unwrap_or(after.len());
    let value = after[..end].trim();
    if value.is_empty() {
        return None;
    }
    let numeric_prefix = value
        .chars()
        .take_while(|ch| ch.is_ascii_digit() || *ch == '.')
        .collect::<String>();
    if !numeric_prefix.is_empty() {
        if let Ok(secs) = numeric_prefix.parse::<f64>() {
            if !secs.is_finite() || secs < 0.0 {
                return Some(0);
            }
            let ms = (secs * 1000.0) as u64;
            return Some(ms.min(MAX_MS));
        }
    }
    if let Ok(target) = httpdate::parse_http_date(value) {
        let now = std::time::SystemTime::now();
        let delta = target
            .duration_since(now)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        return Some(delta.min(MAX_MS));
    }
    None
}

/// Write the full LLM request payload to a JSONL transcript file.
///
/// Holds a process-wide mutex around the open + write so concurrent
/// transcript emitters (parallel tests, multi-tenant agent loops on the
/// same VM) never produce a torn line. POSIX `O_APPEND` only guarantees
/// atomicity for writes ≤ `PIPE_BUF` (512 bytes on macOS), and
/// `provider_call_request` events comfortably exceed that — without
/// this lock, two simultaneous `writeln!` calls on different `File`
/// handles for the same path can interleave their bytes mid-line and
/// produce invalid JSON that downstream readers (and tests) silently
/// drop.
pub(super) fn append_llm_transcript_entry(entry: &serde_json::Value) {
    let mut redacted = entry.clone();
    crate::redact::current_policy().redact_json_in_place(&mut redacted);
    forward_transcript_run_events(&redacted);
    append_llm_transcript_event_log(&redacted);
    let Some(dir) = current_transcript_dir() else {
        return;
    };
    let _ = std::fs::create_dir_all(&dir);
    let path = format!("{dir}/llm_transcript.jsonl");
    let Ok(line) = serde_json::to_string(&redacted) else {
        return;
    };
    static WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = WRITE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        use std::io::Write;
        let _ = f.write_all(line.as_bytes());
        let _ = f.write_all(b"\n");
    }
}

/// Fan transcript entries out to the run-events sink (`harn run
/// --json`). [`RunEvent::Transcript`] mirrors the raw entry; tool
/// calls and tool results carried inside the transcript stream are
/// also surfaced as their own [`RunEvent::ToolCall`] /
/// [`RunEvent::ToolResult`] variants so consumers don't have to
/// re-parse the transcript shape.
///
/// `tool_call` events are emitted once per logical call, keyed off
/// `interpreted_response` (the post-parse view that resolves the final
/// tool selection). Earlier-stage entries (`provider_call_response`)
/// still appear as `transcript` events for replay, but their
/// `tool_calls` arrays are not promoted to avoid duplicate
/// `tool_call` events for the same `call_id`.
fn forward_transcript_run_events(entry: &serde_json::Value) {
    if !crate::run_events::sink_active() {
        return;
    }
    let kind = entry
        .get("type")
        .and_then(|value| value.as_str())
        .unwrap_or("transcript_event")
        .to_string();
    let agent_id = entry
        .get("agent_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);

    if kind == "interpreted_response" {
        if let Some(calls) = entry.get("tool_calls").and_then(|value| value.as_array()) {
            for call in calls {
                let name = call
                    .get("name")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_string();
                let id = call
                    .get("id")
                    .or_else(|| call.get("call_id"))
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_string();
                let args = call
                    .get("arguments")
                    .or_else(|| call.get("args"))
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                crate::run_events::emit(crate::run_events::RunEvent::ToolCall {
                    call_id: id,
                    name,
                    args,
                    started_at: chrono_now(),
                });
            }
        }
    }

    if kind == "tool_result" {
        let call_id = entry
            .get("call_id")
            .or_else(|| entry.get("id"))
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string();
        let ok = entry
            .get("ok")
            .and_then(|value| value.as_bool())
            .unwrap_or(true);
        let result = entry
            .get("result")
            .or_else(|| entry.get("content"))
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        crate::run_events::emit(crate::run_events::RunEvent::ToolResult {
            call_id,
            ok,
            result,
        });
    }

    crate::run_events::emit(crate::run_events::RunEvent::Transcript {
        agent_id,
        kind,
        payload: entry.clone(),
    });
}

fn append_llm_transcript_event_log(entry: &serde_json::Value) {
    let Some(log) = crate::event_log::active_event_log() else {
        return;
    };
    let topic = crate::event_log::Topic::new(crate::event_log::HARN_LLM_TRANSCRIPT_TOPIC)
        .expect("static transcript topic should be valid");
    let kind = entry
        .get("type")
        .and_then(|value| value.as_str())
        .unwrap_or("transcript_event")
        .to_string();
    let mut headers = std::collections::BTreeMap::new();
    if let Some(span_id) = entry.get("span_id").and_then(|value| value.as_u64()) {
        headers.insert("span_id".to_string(), span_id.to_string());
    }
    if let Some(context) = crate::triggers::dispatcher::current_dispatch_context() {
        headers.insert("trigger_id".to_string(), context.binding_id.clone());
        headers.insert(
            "binding_key".to_string(),
            format!("{}@v{}", context.binding_id, context.binding_version),
        );
        headers.insert("event_id".to_string(), context.trigger_event.id.0.clone());
        headers.insert(
            "trace_id".to_string(),
            context.trigger_event.trace_id.0.clone(),
        );
        headers.insert("pipeline".to_string(), context.binding_id);
        headers.insert("action".to_string(), context.action);
        if let Some(tenant_id) = context.trigger_event.tenant_id {
            headers.insert("tenant_id".to_string(), tenant_id.0);
        }
    }
    let event = crate::event_log::LogEvent::new(kind, entry.clone()).with_headers(headers);
    // Append synchronously. Earlier this fire-and-forget `handle.spawn`ed the
    // append on the ambient tokio runtime, but the agent loop and the test
    // runner drive their runtime with `LocalSet::run_until`, which stops
    // polling once the driving future resolves. Detached append tasks were
    // therefore never polled to completion: each stranded task pinned its
    // transcript-sized `LogEvent` payload plus an `Arc<AnyEventLog>` clone for
    // the lifetime of the runtime — across an entire `harn test --parallel`
    // worker, that accumulated ~one transcript per test and OOM'd CI (#2660).
    //
    // None of the event-log backends actually yield to the tokio reactor on
    // `append` (memory = `Mutex`, sqlite = blocking `Mutex<Connection>`, file =
    // blocking fs), so a private `futures::executor::block_on` runs the append
    // to completion on this thread without touching the ambient runtime. This
    // is the same path the non-runtime branch already used.
    let _ = futures::executor::block_on(log.append(&topic, event));
}

/// Record a `template.render` transcript event for a `render()` /
/// `render_prompt()` call that resolved under an LLM-aware frame.
/// Captures the active LLM identity + capability snapshot plus the
/// branch trace produced during rendering. Replay determinism is
/// guaranteed by the renderer itself; this function is purely a
/// serializer.
pub fn record_template_render(
    template_uri: &str,
    template_revision_hash: &str,
    ctx: &crate::stdlib::template::LlmRenderContext,
    trace: &[crate::stdlib::template::BranchDecision],
    rendered_bytes: usize,
) {
    let branches = trace
        .iter()
        .map(|decision| {
            let mut entry = serde_json::Map::new();
            entry.insert(
                "kind".to_string(),
                serde_json::Value::String(decision.kind.as_str().to_string()),
            );
            entry.insert(
                "template_uri".to_string(),
                serde_json::Value::String(decision.template_uri.clone()),
            );
            entry.insert("line".to_string(), serde_json::json!(decision.line));
            entry.insert("col".to_string(), serde_json::json!(decision.col));
            entry.insert(
                "branch_id".to_string(),
                serde_json::Value::String(decision.branch_id.clone()),
            );
            if let Some(label) = decision.branch_label.as_ref() {
                entry.insert(
                    "branch_label".to_string(),
                    serde_json::Value::String(label.clone()),
                );
            }
            serde_json::Value::Object(entry)
        })
        .collect::<Vec<_>>();
    let llm = serde_json::json!({
        "provider": ctx.provider,
        "model": ctx.model,
        "family": ctx.family,
        "capabilities": vm_value_to_json(&ctx.capabilities),
    });
    let mut fields = serde_json::Map::new();
    fields.insert(
        "template_uri".to_string(),
        serde_json::Value::String(template_uri.to_string()),
    );
    fields.insert(
        "template_revision_hash".to_string(),
        serde_json::Value::String(template_revision_hash.to_string()),
    );
    fields.insert("llm".to_string(), llm);
    fields.insert("branches".to_string(), serde_json::Value::Array(branches));
    fields.insert(
        "rendered_bytes".to_string(),
        serde_json::json!(rendered_bytes),
    );
    append_llm_observability_entry("template.render", fields);
}

fn vm_value_to_json(value: &crate::value::VmValue) -> serde_json::Value {
    use crate::value::VmValue;
    match value {
        VmValue::Nil => serde_json::Value::Null,
        VmValue::Bool(b) => serde_json::Value::Bool(*b),
        VmValue::Int(n) => serde_json::json!(*n),
        VmValue::Float(f) => serde_json::json!(*f),
        VmValue::String(s) => serde_json::Value::String(s.to_string()),
        VmValue::List(items) => {
            serde_json::Value::Array(items.iter().map(vm_value_to_json).collect())
        }
        VmValue::Dict(d) => serde_json::Value::Object(
            d.iter()
                .map(|(k, v)| (k.to_string(), vm_value_to_json(v)))
                .collect(),
        ),
        other => serde_json::Value::String(other.display()),
    }
}

pub(crate) fn append_llm_observability_entry(
    event_type: &str,
    mut fields: serde_json::Map<String, serde_json::Value>,
) {
    fields.insert("type".to_string(), serde_json::json!(event_type));
    fields
        .entry("timestamp".to_string())
        .or_insert_with(|| serde_json::json!(chrono_now()));
    fields
        .entry("span_id".to_string())
        .or_insert_with(|| serde_json::json!(crate::tracing::current_span_id()));
    append_llm_transcript_entry(&serde_json::Value::Object(fields));
}

fn emit_system_prompt_if_changed(system: Option<&str>) {
    let content = system.unwrap_or("");
    let current = hash_str(content);
    let changed = LAST_SYSTEM_PROMPT_HASH.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.as_ref() == Some(&current) {
            false
        } else {
            *slot = Some(current);
            true
        }
    });
    if !changed {
        return;
    }
    append_llm_transcript_entry(&serde_json::json!({
        "type": "system_prompt",
        "timestamp": chrono_now(),
        "span_id": crate::tracing::current_span_id(),
        "hash": current,
        "content": content,
    }));
}

fn emit_tool_schemas_if_changed(schemas: &[crate::llm::tools::ToolSchema]) {
    let value = serde_json::to_value(schemas).unwrap_or(serde_json::Value::Null);
    let current = hash_json(&value);
    let changed = LAST_TOOL_SCHEMAS_HASH.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.as_ref() == Some(&current) {
            false
        } else {
            *slot = Some(current);
            true
        }
    });
    if !changed {
        return;
    }
    append_llm_transcript_entry(&serde_json::json!({
        "type": "tool_schemas",
        "timestamp": chrono_now(),
        "span_id": crate::tracing::current_span_id(),
        "hash": current,
        "schemas": value,
    }));
}

pub(super) fn dump_llm_request(
    iteration: usize,
    call_id: &str,
    tool_format: &str,
    opts: &super::api::LlmCallOptions,
) {
    // Emit system prompt + schemas as dedup'd events so they don't
    // repeat on every request.
    emit_system_prompt_if_changed(opts.system.as_deref());
    let tool_schemas =
        crate::llm::tools::collect_tool_schemas(opts.tools.as_ref(), opts.native_tools.as_deref());
    emit_tool_schemas_if_changed(&tool_schemas);

    let structural_experiment = opts
        .applied_structural_experiment
        .as_ref()
        .map(serde_json::to_value)
        .transpose()
        .unwrap_or(None)
        .unwrap_or(serde_json::Value::Null);
    if let Some(decision) = opts.routing_decision.as_ref() {
        append_llm_transcript_entry(&serde_json::json!({
            "type": "routing_decision",
            "iteration": iteration,
            "call_id": call_id,
            "span_id": crate::tracing::current_span_id(),
            "timestamp": chrono_now(),
            "policy": decision.policy.clone(),
            "requested_quality": decision.requested_quality.clone(),
            "selected_provider": decision.selected_provider.clone(),
            "selected_model": decision.selected_model.clone(),
            "fallback_chain": opts.fallback_chain.clone(),
            "alternatives": decision.alternatives.clone(),
        }));
    }
    let mut request_event = serde_json::json!({
        "type": "provider_call_request",
        "iteration": iteration,
        "call_id": call_id,
        "span_id": crate::tracing::current_span_id(),
        "timestamp": chrono_now(),
        "model": opts.model,
        "provider": opts.provider,
        "max_tokens": opts.max_tokens,
        "temperature": opts.temperature,
        "thinking": match &opts.thinking {
            super::api::ThinkingConfig::Disabled => serde_json::json!({
                "mode": "disabled",
                "enabled": false,
                "budget_tokens": serde_json::Value::Null,
            }),
            super::api::ThinkingConfig::Enabled { budget_tokens } => serde_json::json!({
                "mode": "enabled",
                "enabled": true,
                "budget_tokens": budget_tokens,
            }),
            super::api::ThinkingConfig::Adaptive => serde_json::json!({
                "mode": "adaptive",
                "enabled": true,
                "budget_tokens": serde_json::Value::Null,
            }),
            super::api::ThinkingConfig::Effort { level } => serde_json::json!({
                "mode": "effort",
                "level": level.as_str(),
                "enabled": *level != super::api::ReasoningEffort::None,
                "budget_tokens": serde_json::Value::Null,
            }),
        },
        "tool_choice": opts.tool_choice,
        "tool_format": tool_format,
        "native_tool_count": opts.native_tools.as_ref().map(|tools| tools.len()).unwrap_or(0),
        "message_count": opts.messages.len(),
        "structural_experiment": structural_experiment,
        "route_policy": opts.route_policy.as_label(),
        "fallback_chain": opts.fallback_chain.clone(),
        "routing_decision": opts.routing_decision.clone(),
    });
    if verbose_llm_transcript_enabled() {
        request_event["request_snapshot"] = serde_json::json!({
            "system": opts.system,
            "messages": opts.messages,
            "tool_schemas": tool_schemas,
            "native_tools": opts.native_tools,
        });
    }
    append_llm_transcript_entry(&request_event);
}

/// Compute the merged (native OR text-parsed) tool calls for the
/// observability response record. Mirrors the merge in
/// `crate::llm::api::result::vm_build_llm_result` (provider-native calls
/// take precedence; otherwise fall back to the calls parsed out of the
/// inline tagged `<tool_call>` blocks in `result.text`, resolved against
/// the same `tools` registry the request used so unknown-name calls are
/// not dropped). By the time the result reaches this function `text` has
/// already been canonicalized from any `[[CALL]]` wire form back to
/// `<tool_call>`, so the tagged parser sees the calls.
///
/// Observability-only: this is read off the existing `result` + `tools`
/// and does not flow back into the request-construction / history path, so
/// the model's next-turn payload is byte-identical with or without this
/// call.
fn merged_tool_calls_for_observability(
    result: &super::api::LlmResult,
    tools: Option<&crate::value::VmValue>,
) -> Vec<serde_json::Value> {
    if !result.tool_calls.is_empty() {
        return result.tool_calls.clone();
    }
    crate::llm::tools::parse_text_tool_calls_with_tools(&result.text, tools).calls
}

pub(super) fn dump_llm_response(
    iteration: usize,
    call_id: &str,
    result: &super::api::LlmResult,
    response_ms: u64,
    structural_experiment: Option<&crate::llm::structural_experiments::AppliedStructuralExperiment>,
    tools: Option<&crate::value::VmValue>,
) {
    let structural_experiment = structural_experiment
        .map(serde_json::to_value)
        .transpose()
        .unwrap_or(None)
        .unwrap_or(serde_json::Value::Null);
    let telemetry = serde_json::to_value(&result.telemetry).unwrap_or(serde_json::Value::Null);
    let parsed_tool_calls = merged_tool_calls_for_observability(result, tools);
    append_llm_transcript_entry(&serde_json::json!({
        "type": "provider_call_response",
        "iteration": iteration,
        "call_id": call_id,
        "span_id": crate::tracing::current_span_id(),
        "timestamp": chrono_now(),
        "provider": result.provider,
        "model": result.model,
        "text": result.text,
        "tool_calls": result.tool_calls,
        // Observability-only merged view: provider-native calls when present,
        // otherwise the calls parsed out of the inline tagged `<tool_call>`
        // blocks in `text`. Text-format local models (llamacpp/qwen3.6) carry
        // their calls only inline, so `tool_calls` (native) is empty for them;
        // this sidecar makes the response record self-describing. Distinct from
        // `tool_calls` so consumers can tell native vs. text-parsed apart. This
        // does NOT touch the request-construction / history path — the model's
        // next-turn payload is unchanged.
        "parsed_tool_calls": parsed_tool_calls,
        "input_tokens": result.input_tokens,
        "output_tokens": result.output_tokens,
        "cost_usd": crate::llm::cost::calculate_cost_for_provider(
            &result.provider,
            &result.model,
            result.input_tokens,
            result.output_tokens,
        ),
        "cache_read_tokens": result.cache_read_tokens,
        "cache_write_tokens": result.cache_write_tokens,
        "cache_creation_input_tokens": result.cache_write_tokens,
        "cache_hit_ratio": crate::llm::cost::cache_hit_ratio(
            result.input_tokens,
            result.cache_read_tokens,
            result.cache_write_tokens,
        ),
        "cache_savings_usd": crate::llm::cost::cache_savings_usd_for_provider(
            &result.provider,
            &result.model,
            result.cache_read_tokens,
            result.cache_write_tokens,
        ),
        // Explicit bool for easy cache-regression spotting in tailed logs.
        "cache_hit": result.cache_read_tokens > 0,
        "thinking": result.thinking,
        "thinking_summary": result.thinking_summary,
        // Provider-reported finish/stop reason (`stop` / `length` /
        // `tool_calls` for OpenAI-compatibles, `end_turn` / `max_tokens` /
        // `tool_use` for Anthropic, `done_reason` for Ollama). The transport
        // layer has always captured this onto `LlmResult.stop_reason`, but the
        // observability record dropped it — so transcript mining saw
        // stop_reason=None on every provider response and truncation analysis
        // (an IDE host bug report) was blind to output-cap cuts. `null` when the
        // provider reported nothing.
        "stop_reason": result.stop_reason,
        "response_ms": response_ms,
        // Server-side runtime telemetry (Ollama timings, llama.cpp prefill /
        // decode breakdown, etc.). Empty for providers that report nothing.
        "provider_telemetry": telemetry,
        "structural_experiment": structural_experiment,
    }));
}

/// Emit the self-contained `resolved_dispatch` transcript record for one LLM
/// call: the final resolved provider/model/wire_format/thinking/tool_format +
/// where each came from (`provenance`) + the normalized outcome. This is the
/// one-call answer to "what did this LLM call actually dispatch, and what did
/// it return" that used to require joining request+response events and
/// cross-referencing the capability catalog by hand.
pub(super) fn dump_resolved_dispatch(
    iteration: usize,
    call_id: &str,
    opts: &super::api::LlmCallOptions,
    effective_tool_format: &str,
    outcome: &super::resolved_dispatch::DispatchOutcome,
) {
    append_llm_transcript_entry(&super::resolved_dispatch::build_record(
        iteration,
        call_id,
        crate::tracing::current_span_id(),
        chrono_now(),
        opts,
        effective_tool_format,
        outcome,
    ));
}

pub(super) fn annotate_current_span(metadata: &[(&str, serde_json::Value)]) {
    let Some(span_id) = crate::tracing::current_span_id() else {
        return;
    };
    for (key, value) in metadata {
        crate::tracing::span_set_metadata(span_id, key, value.clone());
    }
}

pub(super) fn chrono_now() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}.{:03}", now.as_secs(), now.subsec_millis())
}

/// Inputs required to wire a streaming candidate detector (harn#692)
/// into a delta-forwarding task. When supplied, the detector consumes
/// each text delta in parallel with the bridge progress notifier and
/// emits `AgentEvent::ToolCall { parsing: true, .. }` /
/// `AgentEvent::ToolCallUpdate { parsing: false, .. }` events through
/// the global session sink registry so ACP clients render an in-flight
/// chip while the model is still writing the args.
pub(crate) struct StreamingDetectorContext {
    pub session_id: String,
    pub known_tools: std::collections::BTreeSet<String>,
}

/// Create an unbounded channel and spawn a local task that forwards text
/// deltas to `bridge.send_call_progress()`. When `detector_ctx` is
/// `Some`, the same task also drives a streaming text-tool-call
/// candidate detector — emitting candidate-start / promoted / aborted
/// events via the global session sink registry as the buffer grows
/// (harn#692).
pub(super) fn spawn_progress_forwarder(
    bridge: &Arc<crate::bridge::HostBridge>,
    call_id: String,
    user_visible: bool,
    detector_ctx: Option<StreamingDetectorContext>,
    mut first_token: super::first_token::FirstTokenTimer,
) -> DeltaSender {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let bridge = bridge.clone();
    let mut detector = detector_ctx.map(|ctx| {
        crate::llm::tools::StreamingToolCallDetector::new(ctx.session_id, ctx.known_tools)
    });
    tokio::task::spawn_local(async move {
        let mut token_count: u64 = 0;
        while let Some(delta) = rx.recv().await {
            first_token.observe_delta();
            token_count += 1;
            bridge.send_call_progress(&call_id, &delta, token_count, user_visible);
            if let Some(d) = detector.as_mut() {
                for event in d.push(&delta) {
                    crate::agent_events::emit_event(&event);
                }
            }
        }
        if let Some(mut d) = detector {
            for event in d.finalize() {
                crate::agent_events::emit_event(&event);
            }
        }
    });
    tx
}

/// No-bridge twin of `spawn_progress_forwarder`. Drives only the
/// streaming candidate detector — the deltas are otherwise discarded
/// (the bridge progress channel is the only consumer, and we don't have
/// one). Used so non-bridge callers (offthread VM, CLI loops without an
/// attached host) still see candidate events when they have a
/// `StreamingDetectorContext`.
pub(super) fn spawn_detector_only_forwarder(
    detector_ctx: StreamingDetectorContext,
    first_token: super::first_token::FirstTokenTimer,
) -> DeltaSender {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    tokio::task::spawn_local(run_detector_loop(detector_ctx, rx, first_token));
    tx
}

/// Inner loop driving a [`StreamingToolCallDetector`] from a delta
/// channel. Pulled out of `spawn_detector_only_forwarder` so tests can
/// drive the same logic deterministically (await directly) without
/// depending on `spawn_local` task scheduling.
///
/// `sink` is the function each emitted event flows through. Production
/// passes `crate::agent_events::emit_event` so events fan out through
/// the global session-keyed sink registry. Tests pass a closure that
/// captures into a local buffer — sidestepping the global registry,
/// which other tests in this binary mutate via `reset_all_sinks` and
/// can race the per-session install.
#[cfg(test)]
async fn run_detector_loop_with_sink(
    detector_ctx: StreamingDetectorContext,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<String>,
    mut sink: impl FnMut(&crate::agent_events::AgentEvent),
) {
    let mut detector = crate::llm::tools::StreamingToolCallDetector::new(
        detector_ctx.session_id,
        detector_ctx.known_tools,
    );
    while let Some(delta) = rx.recv().await {
        for event in detector.push(&delta) {
            sink(&event);
        }
    }
    for event in detector.finalize() {
        sink(&event);
    }
}

/// Production wrapper: forwards every detector event through the global
/// session sink registry so ACP / external sinks see them.
async fn run_detector_loop(
    detector_ctx: StreamingDetectorContext,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<String>,
    mut first_token: super::first_token::FirstTokenTimer,
) {
    let mut detector = crate::llm::tools::StreamingToolCallDetector::new(
        detector_ctx.session_id,
        detector_ctx.known_tools,
    );
    while let Some(delta) = rx.recv().await {
        first_token.observe_delta();
        for event in detector.push(&delta) {
            crate::agent_events::emit_event(&event);
        }
    }
    for event in detector.finalize() {
        crate::agent_events::emit_event(&event);
    }
}

/// Configuration for LLM call retries.
///
/// Default 0: a raw `llm_call` is fail-fast by default (documented contract;
/// see the quickref), and the `llm_retries` option is deprecated in favor of
/// `with_retry` (removed in v0.9.0). Resilience for the agent/eval path comes
/// from the agent options presets (`agent/options.harn` sets `llm_retries: 2`)
/// and the proactive per-route rate limiter (sliding-window + cooldown), NOT
/// from a global default — flipping this to >=2 silently retried transient
/// errors for EVERY caller (one-shot library calls, conformance scenarios, the
/// CLI playground replay), changing the documented fail-fast semantics. Callers
/// that want retries opt in via `llm_retries`/`with_retry`; only *retryable*
/// errors would consume the budget (`is_retryable_llm_error`).
pub(crate) const DEFAULT_LLM_CALL_RETRIES: usize = 0;
pub(crate) const DEFAULT_LLM_CALL_BACKOFF_MS: u64 = 250;

/// Built-in retry budget for zero-token empty completions. Applies even when
/// the caller's transient-retry budget is 0 (the fail-fast `llm_call`
/// default), mirroring the transport's unconditional single retry for the
/// Ollama empty-content parser bug: an empty 200 is clearly a provider
/// hiccup, and most live callers (e.g. a host agent loop) retry only on
/// *errors*, so an empty Ok would otherwise sail through untouched.
const EMPTY_COMPLETION_BUILTIN_RETRIES: usize = 1;

/// Effective retry budget for zero-token empty completions: the caller's
/// transient budget, floored at [`EMPTY_COMPLETION_BUILTIN_RETRIES`] for real
/// providers. Deterministic in-process providers (mock/fake) replay scripted
/// turns — a built-in silent retry would consume turns tests rely on — so
/// they only honor an explicit `llm_retries` opt-in.
fn empty_completion_retry_budget(retry_config: &LlmRetryConfig, provider: &str) -> usize {
    if crate::llm::providers::MockProvider::should_intercept(provider)
        || crate::llm::fake::FakeLlmProvider::should_intercept(provider)
    {
        retry_config.retries
    } else {
        retry_config.retries.max(EMPTY_COMPLETION_BUILTIN_RETRIES)
    }
}

pub(crate) struct LlmRetryConfig {
    /// Maximum number of retries for transient errors (429, 5xx, connection).
    pub retries: usize,
    /// Base backoff in milliseconds between retries.
    pub backoff_ms: u64,
}

impl Default for LlmRetryConfig {
    fn default() -> Self {
        Self {
            retries: DEFAULT_LLM_CALL_RETRIES,
            backoff_ms: DEFAULT_LLM_CALL_BACKOFF_MS,
        }
    }
}

fn llm_retry_backoff_ms(
    error: &VmError,
    retry_config: &LlmRetryConfig,
    attempt: usize,
    provider: &str,
) -> u64 {
    if crate::llm::providers::MockProvider::should_intercept(provider) {
        return 0;
    }
    // Honor an explicit provider Retry-After, but still add a small jitter on
    // top so concurrent same-key callers (eval --concurrency K plus a coexisting
    // session) that all received the same Retry-After do not resume in lockstep
    // and re-stampede the provider the instant the window opens.
    match extract_retry_after_ms(error) {
        Some(retry_after_ms) => retry_after_ms.saturating_add(retry_after_jitter_ms()),
        None => base_retry_backoff_ms(retry_config, attempt),
    }
}

/// Equal-jitter exponential backoff base shared by the error-retry and
/// empty-completion retry paths (no `retry-after` hint available on the latter).
///
/// AWS "equal jitter": `wait = ceil/2 + rand(0, ceil/2)`, where
/// `ceil = backoff_ms * 2^min(attempt, 4)`. Keeping the lower half fixed avoids
/// the near-zero waits that pure "full jitter" can produce, while randomizing
/// the upper half desynchronizes retries across concurrent same-key processes
/// (avoids the thundering herd that the old zero-jitter `ceil` produced).
fn base_retry_backoff_ms(retry_config: &LlmRetryConfig, attempt: usize) -> u64 {
    let ceil = retry_config.backoff_ms.saturating_mul(1 << attempt.min(4));
    equal_jitter_ms(ceil, &mut rand::rng())
}

/// Pure equal-jitter computation, seamed on an injectable RNG so tests can
/// assert the `[ceil/2, ceil]` bounds and desynchronization deterministically.
fn equal_jitter_ms<R: rand::RngExt>(ceil: u64, rng: &mut R) -> u64 {
    let half = ceil / 2;
    if half == 0 {
        return ceil;
    }
    // `ceil/2` fixed lower half + `rand(0, ceil/2)` upper half → `[ceil/2, ceil]`.
    half + rand_range_inclusive(half, rng)
}

/// Small additive jitter (0..=backoff base) layered on top of a provider
/// Retry-After so identical Retry-After values do not resume in lockstep.
fn retry_after_jitter_ms() -> u64 {
    rand_range_inclusive(DEFAULT_LLM_CALL_BACKOFF_MS, &mut rand::rng())
}

/// `rand(0, max)` inclusive, seamed on an injectable RNG. Centralizes the
/// half-open-to-inclusive conversion (`random_range` is exclusive of its end).
fn rand_range_inclusive<R: rand::RngExt>(max: u64, rng: &mut R) -> u64 {
    rng.random_range(0..max.saturating_add(1))
}

/// Rewrite a native-tool-format request onto the text channel for a retry,
/// without rebuilding the whole request from scratch. Mirrors the established
/// "text-channel request" shape (see the Ollama raw-generate test in `api.rs`):
/// drop the provider-native tool payload, force `Text` output, and clear the
/// structured-output mirrors so the transport serves a plain chat completion
/// the model answers in content. The agent loop's text-tool parser then reads
/// the calls back out of the assistant text.
///
/// This is the wire-level half of the runtime tool_format fallback. It does NOT
/// re-render the system prompt's tool exemplar (that lives in the pipeline), so
/// the goal is strictly to stop a native-channel failure from hard-failing or
/// parse-looping the call — letting the model produce *parseable* output on a
/// working channel — not to guarantee identical guidance to a text-pinned run.
fn degrade_options_to_text_channel(
    opts: &super::api::LlmCallOptions,
) -> super::api::LlmCallOptions {
    let mut degraded = opts.clone();
    degraded.native_tools = None;
    degraded.output_format = super::api::OutputFormat::Text;
    degraded.response_format = None;
    degraded.json_schema = None;
    degraded
}

fn degrade_options_to_non_streaming_transport(
    opts: &super::api::LlmCallOptions,
) -> super::api::LlmCallOptions {
    let mut degraded = opts.clone();
    degraded.stream = false;
    degraded
}

// ---------------------------------------------------------------------------
// observed_llm_call — shared single-LLM-call wrapper with full observability
// ---------------------------------------------------------------------------

/// Make one LLM call with full observability: call-id generation, bridge
/// notifications (call_start / call_progress / call_end), span annotation,
/// retry with exponential backoff, and tracing.
pub(crate) async fn observed_llm_call(
    opts: &super::api::LlmCallOptions,
    tool_format: Option<&str>,
    bridge: Option<&Arc<crate::bridge::HostBridge>>,
    retry_config: &LlmRetryConfig,
    iteration: Option<usize>,
    user_visible: bool,
    offthread: bool,
    streaming_detector: Option<StreamingDetectorContext>,
) -> Result<super::api::LlmResult, VmError> {
    let _in_flight_guard = super::call::InFlightLlmCallGuard::enter(opts);
    let mut effective_tool_format = tool_format
        .map(str::to_string)
        .or_else(|| {
            std::env::var("HARN_AGENT_TOOL_FORMAT")
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
        .unwrap_or_else(|| crate::llm_config::default_tool_format(&opts.model, &opts.provider));
    // Working request. Starts as the caller's `opts` (zero-copy) and is only
    // cloned when the runtime tool_format fallback degrades a native-channel
    // request to text mid-retry (see the `Err` arm below). Once degraded, the
    // degraded copy persists for every remaining attempt this call makes.
    let mut working: std::borrow::Cow<'_, super::api::LlmCallOptions> =
        std::borrow::Cow::Borrowed(opts);
    let mut degraded_to_text = false;
    let mut degraded_stream_transport = false;
    let mut attempt = 0usize;
    // How many empty-completion flakes this call retried through. Distinct from
    // `attempt` (which also counts transient-error and tool-format-degrade
    // retries) so the `resolved_dispatch` record can tell a clean serve from a
    // serve that RECOVERED from an empty flake — the exact recovered-vs-terminal
    // distinction the escalation guard hinges on.
    let mut empty_completion_retries = 0usize;
    loop {
        let opts: &super::api::LlmCallOptions = working.as_ref();
        // Network-only circuit breaker: if this route has seen sustained
        // NetworkError/Timeout failures, fail fast instead of burning the retry
        // budget against a dead link (laptop disconnect / DNS failure). 429s do
        // NOT trip this — they are handled by the rate-limiter cooldown below.
        super::rate_limit::check_network_breaker_for_llm_call(opts)?;

        let rate_limit_permit = super::rate_limit::acquire_permit_for_llm_call(opts).await?;

        // Rate governor (Layer 1, behind the `llm.rate_governor` flag; a byte-
        // identical no-op when off). Retries WAIT behind the governor instead of
        // blind-firing: an AIMD-shrunk concurrency limit, a full token bucket, or
        // an OPEN circuit all resolve to a bounded back-off here rather than
        // another hammer at a throttled provider. `gate` reserves an in-flight
        // slot on `Proceed`; the outcome below releases it exactly once.
        let governor_org_key = crate::llm::rate_governor::org_key_id(&opts.api_key);
        let governor_est_tokens = governor_estimated_tokens(opts);
        let governor_reserved =
            await_governor_admission(&opts.provider, &governor_org_key, governor_est_tokens).await;

        let call_id = next_call_id();
        let prompt_chars: usize = opts
            .messages
            .iter()
            .filter_map(|m| m.get("content").and_then(|c| c.as_str()))
            .map(|s| s.len())
            .sum();

        let mut span_meta = vec![
            ("call_id", serde_json::json!(call_id.clone())),
            ("model", serde_json::json!(opts.model.clone())),
            ("provider", serde_json::json!(opts.provider.clone())),
            ("prompt_chars", serde_json::json!(prompt_chars)),
            (
                "route_policy",
                serde_json::json!(opts.route_policy.as_label()),
            ),
            (
                "fallback_chain",
                serde_json::json!(opts.fallback_chain.clone()),
            ),
        ];
        if let Some(decision) = opts.routing_decision.as_ref() {
            span_meta.push(("routing_decision", serde_json::json!(decision)));
        }
        if let Some(iter) = iteration {
            span_meta.push(("iteration", serde_json::json!(iter)));
            span_meta.push(("llm_attempt", serde_json::json!(attempt)));
        }
        annotate_current_span(&span_meta);

        let mut call_start_meta =
            serde_json::json!({"model": opts.model, "prompt_chars": prompt_chars});
        call_start_meta["stream_publicly"] =
            serde_json::json!(opts.response_format.as_deref() != Some("json"));
        call_start_meta["user_visible"] = serde_json::json!(user_visible);
        if let Some(iter) = iteration {
            call_start_meta["iteration"] = serde_json::json!(iter);
            call_start_meta["llm_attempt"] = serde_json::json!(attempt);
        }
        if let Some(b) = bridge {
            b.send_call_start(&call_id, "llm", "llm_call", call_start_meta);
        }

        dump_llm_request(
            iteration.unwrap_or(0),
            &call_id,
            &effective_tool_format,
            opts,
        );

        let first_token = super::first_token::FirstTokenTimer::for_current_span();
        let start = std::time::Instant::now();
        // The streaming detector runs once per LLM call. Move the
        // context into whichever forwarder we end up spawning so the
        // detector finalizes when the stream closes (or never spawns
        // if this call is non-streamed and there's nothing to listen
        // to).
        let detector_ctx = streaming_detector
            .as_ref()
            .map(|c| StreamingDetectorContext {
                session_id: c.session_id.clone(),
                known_tools: c.known_tools.clone(),
            });
        let llm_result = if let Some(b) = bridge {
            let delta_tx = spawn_progress_forwarder(
                b,
                call_id.clone(),
                user_visible,
                detector_ctx,
                first_token,
            );
            if offthread {
                vm_call_llm_full_streaming_offthread(opts, delta_tx).await
            } else {
                vm_call_llm_full_streaming(opts, delta_tx).await
            }
        } else if offthread {
            let delta_tx = match detector_ctx {
                Some(ctx) => spawn_detector_only_forwarder(ctx, first_token),
                None => {
                    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<String>();
                    tx
                }
            };
            vm_call_llm_full_streaming_offthread(opts, delta_tx).await
        } else {
            super::api::vm_call_llm_full(opts).await
        };
        drop(rate_limit_permit);
        let duration_ms = start.elapsed().as_millis() as u64;

        // Release the governor slot reserved above and drive AIMD + the circuit
        // (no-op when the flag is off). Runs once per gated attempt, BEFORE the
        // arms below branch into retry/return/error, so the slot accounting can
        // never leak. Detection (L0) also emits a `provider_throttle` record on
        // a throttle signal. `empty_under_load` fires only when this provider is
        // ALREADY throttled — the 24/24-empties soft-throttle signature.
        record_governor_call_outcome(
            &opts.provider,
            &governor_org_key,
            governor_reserved,
            &llm_result,
        );

        match llm_result {
            Ok(result) => {
                // An unproductive "success" the loop has no action to run on —
                // either a zero-token empty completion (provider stall) or an
                // errored-but-actionless turn (`stop_reason == "error"` that
                // narrated an intended tool call but emitted none). Both are
                // provider hiccups, not answers: retry within the
                // empty-completion budget rather than advancing the loop on a
                // broken turn (which would only reply with a generic
                // no-progress nag). Once the budget is exhausted, fall through
                // and return the result unchanged so callers see today's shape
                // rather than a novel error.
                if is_retryable_unproductive_completion(&result)
                    && attempt < empty_completion_retry_budget(retry_config, &opts.provider)
                {
                    let errored_actionless = is_errored_actionless_completion(&result);
                    annotate_current_span(&[
                        ("status", serde_json::json!("retrying")),
                        ("retry_reason", serde_json::json!("empty_completion")),
                        ("attempt", serde_json::json!(attempt)),
                    ]);
                    let detail = if errored_actionless {
                        format!(
                            "provider {} model {} ended with a provider error (stop_reason=error) and emitted no tool call (the intended action went only to the reasoning channel)",
                            opts.provider, opts.model
                        )
                    } else {
                        format!(
                            "provider {} model {} returned a zero-token empty completion (no content, thinking, or tool calls)",
                            opts.provider, opts.model
                        )
                    };
                    append_llm_observability_entry(
                        "empty_completion_retry",
                        serde_json::Map::from_iter([
                            (
                                "iteration".to_string(),
                                serde_json::json!(iteration.unwrap_or(0)),
                            ),
                            ("attempt".to_string(), serde_json::json!(attempt + 1)),
                            ("provider".to_string(), serde_json::json!(opts.provider)),
                            ("model".to_string(), serde_json::json!(opts.model)),
                            ("error".to_string(), serde_json::json!(detail.clone())),
                        ]),
                    );
                    super::trace::emit_agent_event(
                        super::trace::AgentTraceEvent::EmptyCompletionRetry {
                            iteration: iteration.unwrap_or(0),
                            attempt: attempt + 1,
                            error: detail.clone(),
                        },
                    );
                    if let Some(b) = bridge {
                        b.send_call_end(
                            &call_id,
                            "llm",
                            "llm_call",
                            duration_ms,
                            "retrying",
                            serde_json::json!({
                                "error": detail,
                                "retryable": true,
                                "attempt": attempt,
                                "user_visible": user_visible,
                            }),
                        );
                    }
                    attempt += 1;
                    empty_completion_retries += 1;
                    let backoff =
                        if crate::llm::providers::MockProvider::should_intercept(&opts.provider) {
                            0
                        } else {
                            base_retry_backoff_ms(retry_config, attempt)
                        };
                    crate::events::log_warn(
                        "llm",
                        &format!("{detail}; retrying in {backoff}ms (attempt {attempt})"),
                    );
                    if backoff > 0 {
                        crate::clock_mock::sleep(std::time::Duration::from_millis(backoff)).await;
                    }
                    continue;
                }
                let usage = crate::tracing::LlmCallUsage {
                    model: result.model.clone(),
                    provider: result.provider.clone(),
                    input_tokens: result.input_tokens,
                    output_tokens: result.output_tokens,
                    cache_read_tokens: result.cache_read_tokens,
                    cache_write_tokens: result.cache_write_tokens,
                    cost_usd: crate::llm::cost::pricing_aware_call_cost(
                        &result.provider,
                        &result.model,
                        result.input_tokens,
                        result.output_tokens,
                    ),
                };
                annotate_current_span(&[("status", serde_json::json!("ok"))]);
                annotate_current_span(&usage.metadata_pairs());
                dump_llm_response(
                    iteration.unwrap_or(0),
                    &call_id,
                    &result,
                    duration_ms,
                    opts.applied_structural_experiment.as_ref(),
                    opts.tools.as_ref(),
                );
                dump_resolved_dispatch(
                    iteration.unwrap_or(0),
                    &call_id,
                    opts,
                    &effective_tool_format,
                    &super::resolved_dispatch::DispatchOutcome::from_result(
                        &result,
                        empty_completion_retries,
                    ),
                );
                annotate_current_span(&[(
                    "structural_experiment",
                    opts.applied_structural_experiment
                        .as_ref()
                        .map(serde_json::to_value)
                        .transpose()
                        .unwrap_or(None)
                        .unwrap_or(serde_json::Value::Null),
                )]);
                if let Some(b) = bridge {
                    b.send_call_end(
                        &call_id,
                        "llm",
                        "llm_call",
                        duration_ms,
                        "ok",
                        serde_json::json!({
                            "model": result.model,
                            "input_tokens": result.input_tokens,
                            "output_tokens": result.output_tokens,
                            "user_visible": user_visible,
                            "structural_experiment": opts.applied_structural_experiment.as_ref(),
                        }),
                    );
                }
                trace_llm_call(LlmTraceEntry {
                    model: result.model.clone(),
                    input_tokens: result.input_tokens,
                    output_tokens: result.output_tokens,
                    duration_ms,
                });
                if let Some(metrics) = crate::active_metrics_registry() {
                    metrics.record_llm_call(
                        &result.provider,
                        &result.model,
                        "succeeded",
                        super::cost::calculate_cost_for_provider(
                            &result.provider,
                            &result.model,
                            result.input_tokens,
                            result.output_tokens,
                        ),
                    );
                    if result.cache_read_tokens > 0 {
                        metrics.record_llm_cache_hit(&result.provider);
                    }
                }
                super::trace::emit_agent_event(super::trace::AgentTraceEvent::LlmCall {
                    call_id: call_id.clone(),
                    model: result.model.clone(),
                    input_tokens: result.input_tokens,
                    output_tokens: result.output_tokens,
                    cache_tokens: result.cache_read_tokens,
                    duration_ms,
                    iteration: iteration.unwrap_or(0),
                });
                // A reachable, answering provider closes the network breaker
                // (and resets any half-open probe).
                super::rate_limit::observe_network_outcome_for_llm_call(opts, false);
                return Ok(result);
            }
            Err(error) => {
                let category = crate::value::error_to_category(&error);
                let message = error.to_string();
                let classified = super::api::classify_llm_error(category.clone(), &message);
                // Shared cooldown: 429 Retry-After, plus 529/503 overload
                // (with a default window when the provider sent no header) so
                // sibling agents on the same route back off together.
                super::rate_limit::observe_retry_after_for_llm_call(
                    opts,
                    shared_cooldown_ms_for_llm_error(&error),
                );
                // Feed transport-level network failures (connection/DNS/
                // timeout) AND provider overload (529/503) to the breaker —
                // never 429 (rate limit) or generic 5xx (single-request
                // server fault on a reachable, healthy link).
                super::rate_limit::observe_network_outcome_for_llm_call(
                    opts,
                    is_network_failure_llm_error(&error) || is_overloaded_llm_error(&error),
                );
                let retryable = is_retryable_llm_error(&error);
                // A *thrown* unproductive completion (zero-token empty, or the
                // billed-noncommittal contract violation whose tool call went
                // only to the reasoning channel) is retried within the same
                // bounded empty-completion budget the `Ok` arm uses — which
                // floors at 1 for real providers even when the caller's
                // transient-retry budget is 0. This unifies the thrown shape
                // onto the existing empty-completion retry path rather than
                // hard-breaking the loop as a silent `provider_error`; it does
                // NOT change `DEFAULT_LLM_CALL_RETRIES`. Once the budget is
                // exhausted the loud thrown error (which names the
                // `upstream contract violation`) is surfaced unchanged, so the
                // eval layer can still classify it as infra, not capability.
                let empty_completion_retry = is_empty_completion_retry_error(&error)
                    && attempt < empty_completion_retry_budget(retry_config, &opts.provider);
                // Runtime tool_format fallback: a native-channel request whose
                // failure fingerprint says the provider's native tool-call
                // channel itself is broken for this route cannot be rescued by
                // retrying native — every retry re-feeds the same broken channel.
                // Degrade ONCE to the text channel instead and retry there, so the
                // call yields parseable output rather than hard-failing or
                // parse-looping. Two broken-channel signatures qualify, both keyed
                // on the failure SIGNATURE (never a model name) and both only when
                // the request actually carried provider-native tools:
                //
                // 1. **Server-side parser choke** (5xx/EOF + tool-parser
                //    fingerprint): the documented Ollama 500 / EOF leak, or any
                //    serving stack that 500s/EOFs on the native assumption.
                //    Detected by [`is_native_tool_channel_failure`] (the #3500
                //    mechanism).
                // 2. **Billed-noncommittal vanishing call** (the canonical
                //    cheap-model signature): the upstream finished cleanly, billed
                //    output tokens, and emitted ZERO `tool_calls` — it serialized
                //    the action only onto a private reasoning channel or returned
                //    an empty committed message. Detected deterministically one
                //    layer down by [`super::api::is_billed_noncommittal_completion`]
                //    and thrown as `billed_noncommittal_completion_error`, matched
                //    here by [`is_billed_noncommittal_throw`]. Before this, that
                //    throw routed onto the bounded SAME-CHANNEL empty-completion
                //    retry, which just re-fed the broken native channel until the
                //    budget drained, then surfaced — never degrading. A native
                //    channel that vanishes once vanishes again; the right move is
                //    the same degrade-to-text as case 1, so the model can produce
                //    a parseable call on the text channel that the gate already
                //    guarantees this route can carry.
                let native_tool_channel_degrade = !degraded_to_text
                    && crate::llm_config::tool_format_channel(&effective_tool_format)
                        == Some(crate::llm_config::ToolFormatChannel::Native)
                    && opts.native_tools.is_some()
                    && (is_native_tool_channel_failure(&error)
                        || is_billed_noncommittal_throw(&error));
                let stream_transport_degrade = !degraded_stream_transport
                    && !native_tool_channel_degrade
                    && is_stream_transport_failure(&error)
                    && can_degrade_stream_transport(opts);
                let can_retry = (retryable && attempt < retry_config.retries)
                    || empty_completion_retry
                    || native_tool_channel_degrade
                    || stream_transport_degrade;
                let status = if can_retry {
                    "retrying"
                } else if retryable {
                    "retries_exhausted"
                } else {
                    "error"
                };
                annotate_current_span(&[
                    ("status", serde_json::json!(status)),
                    ("error", serde_json::json!(message.clone())),
                    ("retryable", serde_json::json!(retryable)),
                    ("attempt", serde_json::json!(attempt)),
                ]);
                append_llm_observability_entry(
                    "provider_call_error",
                    serde_json::Map::from_iter([
                        (
                            "iteration".to_string(),
                            serde_json::json!(iteration.unwrap_or(0)),
                        ),
                        ("call_id".to_string(), serde_json::json!(call_id.clone())),
                        ("attempt".to_string(), serde_json::json!(attempt)),
                        ("status".to_string(), serde_json::json!(status)),
                        ("provider".to_string(), serde_json::json!(opts.provider)),
                        ("model".to_string(), serde_json::json!(opts.model)),
                        ("category".to_string(), serde_json::json!(category.as_str())),
                        (
                            "kind".to_string(),
                            serde_json::json!(classified.kind.as_str()),
                        ),
                        (
                            "reason".to_string(),
                            serde_json::json!(classified.reason.as_str()),
                        ),
                        ("message".to_string(), serde_json::json!(message.clone())),
                        ("retryable".to_string(), serde_json::json!(retryable)),
                    ]),
                );
                if let Some(b) = bridge {
                    b.send_call_end(
                        &call_id,
                        "llm",
                        "llm_call",
                        duration_ms,
                        status,
                        serde_json::json!({
                            "error": error.to_string(),
                            "retryable": retryable,
                            "attempt": attempt,
                            "user_visible": user_visible,
                        }),
                    );
                }
                if !can_retry {
                    if let Some(metrics) = crate::active_metrics_registry() {
                        metrics.record_llm_call(&opts.provider, &opts.model, status, 0.0);
                    }
                    // Terminal failure: emit the self-contained dispatch record
                    // with the error-derived outcome so a consumer sees the
                    // resolved route AND why it failed without joining events or
                    // re-parsing the error string. Retryable attempts do NOT
                    // emit (the retry/recovery is the story there); only the
                    // surfaced terminal error does.
                    dump_resolved_dispatch(
                        iteration.unwrap_or(0),
                        &call_id,
                        opts,
                        &effective_tool_format,
                        &super::resolved_dispatch::DispatchOutcome::from_error_message(&message),
                    );
                    return Err(error);
                }
                if is_empty_completion_retry_error(&error) {
                    // This thrown empty completion is being retried (we passed
                    // the `!can_retry` gate). Count it so a subsequent serve is
                    // recorded as transient-recovered, not a clean serve.
                    empty_completion_retries += 1;
                    append_llm_observability_entry(
                        "empty_completion_retry",
                        serde_json::Map::from_iter([
                            (
                                "iteration".to_string(),
                                serde_json::json!(iteration.unwrap_or(0)),
                            ),
                            ("attempt".to_string(), serde_json::json!(attempt + 1)),
                            ("provider".to_string(), serde_json::json!(opts.provider)),
                            ("model".to_string(), serde_json::json!(opts.model)),
                            ("error".to_string(), serde_json::json!(error.to_string())),
                        ]),
                    );
                    super::trace::emit_agent_event(
                        super::trace::AgentTraceEvent::EmptyCompletionRetry {
                            iteration: iteration.unwrap_or(0),
                            attempt: attempt + 1,
                            error: error.to_string(),
                        },
                    );
                }
                // Apply the runtime tool_format degrade for the next attempt:
                // swap the working request to its text-channel form, flip the
                // effective format reported to telemetry, and record why. The
                // clone severs the shared borrow of `working` so it can be
                // reassigned; `opts` is not used again before the loop restarts.
                let degraded_options =
                    native_tool_channel_degrade.then(|| degrade_options_to_text_channel(opts));
                let stream_degraded_options = stream_transport_degrade
                    .then(|| degrade_options_to_non_streaming_transport(opts));
                attempt += 1;
                let backoff = llm_retry_backoff_ms(&error, retry_config, attempt, &opts.provider);
                crate::events::log_warn(
                    "llm",
                    &format!(
                        "LLM call failed ({}), retrying in {}ms (attempt {}/{})",
                        error, backoff, attempt, retry_config.retries
                    ),
                );
                if let Some(degraded) = degraded_options {
                    let detail = format!(
                        "provider {} model {} native tool channel failed (server-side tool-call \
                         parser 500/EOF: {error}); degrading tool_format native -> json and \
                         retrying on the text channel",
                        degraded.provider, degraded.model
                    );
                    crate::events::log_warn("llm", &detail);
                    append_llm_observability_entry(
                        "tool_format_degrade",
                        serde_json::Map::from_iter([
                            (
                                "iteration".to_string(),
                                serde_json::json!(iteration.unwrap_or(0)),
                            ),
                            ("attempt".to_string(), serde_json::json!(attempt)),
                            ("provider".to_string(), serde_json::json!(degraded.provider)),
                            ("model".to_string(), serde_json::json!(degraded.model)),
                            ("from".to_string(), serde_json::json!("native")),
                            ("to".to_string(), serde_json::json!("json")),
                            ("error".to_string(), serde_json::json!(error.to_string())),
                        ]),
                    );
                    annotate_current_span(&[
                        ("tool_format_degrade", serde_json::json!(true)),
                        ("tool_format_degrade_from", serde_json::json!("native")),
                        ("tool_format_degrade_to", serde_json::json!("json")),
                    ]);
                    effective_tool_format = "json".to_string();
                    degraded_to_text = true;
                    working = std::borrow::Cow::Owned(degraded);
                }
                if let Some(degraded) = stream_degraded_options {
                    let detail = format!(
                        "provider {} model {} streaming transport failed ({error}); degrading \
                         stream=true -> false and retrying through request/response transport",
                        degraded.provider, degraded.model
                    );
                    crate::events::log_warn("llm", &detail);
                    append_llm_observability_entry(
                        "stream_transport_degrade",
                        serde_json::Map::from_iter([
                            (
                                "iteration".to_string(),
                                serde_json::json!(iteration.unwrap_or(0)),
                            ),
                            ("attempt".to_string(), serde_json::json!(attempt)),
                            ("provider".to_string(), serde_json::json!(degraded.provider)),
                            ("model".to_string(), serde_json::json!(degraded.model)),
                            ("from".to_string(), serde_json::json!(true)),
                            ("to".to_string(), serde_json::json!(false)),
                            ("error".to_string(), serde_json::json!(error.to_string())),
                        ]),
                    );
                    annotate_current_span(&[
                        ("stream_transport_degrade", serde_json::json!(true)),
                        ("stream_transport_degrade_from", serde_json::json!(true)),
                        ("stream_transport_degrade_to", serde_json::json!(false)),
                    ]);
                    degraded_stream_transport = true;
                    working = std::borrow::Cow::Owned(degraded);
                }
                if backoff > 0 {
                    crate::clock_mock::sleep(std::time::Duration::from_millis(backoff)).await;
                }
            }
        }
    }
}

#[cfg(test)]
mod retry_tests {
    use super::*;
    use crate::value::{ErrorCategory, VmError, VmValue};

    fn thrown(s: &str) -> VmError {
        VmError::Thrown(VmValue::String(arcstr::ArcStr::from(s)))
    }

    fn categorized(msg: &str, category: ErrorCategory) -> VmError {
        VmError::CategorizedError {
            message: msg.to_string(),
            category,
        }
    }

    // ----- L0 governor throttle detection (VmError -> ThrottleSignal) --------

    #[test]
    fn governor_detects_rate_limit_and_overload_from_runtime_errors() {
        use crate::llm::rate_governor::ThrottleSignal;
        // A 429 rate-limit error (however carried) → RateLimit429.
        assert_eq!(
            governor_throttle_signal_for_error(&thrown(
                "anthropic HTTP 429 [rate_limited]: rate_limit_error"
            )),
            Some(ThrottleSignal::RateLimit429)
        );
        assert_eq!(
            governor_throttle_signal_for_error(&categorized(
                "provider rate limit exceeded",
                ErrorCategory::RateLimit
            )),
            Some(ThrottleSignal::RateLimit429)
        );
        // A provider overload (529/503 / overloaded_error) → Overloaded.
        assert_eq!(
            governor_throttle_signal_for_error(&categorized(
                "anthropic overloaded_error",
                ErrorCategory::Overloaded
            )),
            Some(ThrottleSignal::Overloaded)
        );
        // Non-throttle failures carry no governor signal (the network breaker /
        // retry path owns those).
        assert_eq!(
            governor_throttle_signal_for_error(&categorized(
                "connection reset by peer",
                ErrorCategory::NetworkError
            )),
            None
        );
        assert_eq!(
            governor_throttle_signal_for_error(&thrown("some generic 500 server_error")),
            None
        );
    }

    // ----- runtime tool_format fallback (native -> text) ---------------------

    #[test]
    fn native_tool_channel_failure_matches_ollama_500_parser_signature() {
        // The documented Ollama leak: the server-side qwen3 tool-call extractor
        // EOFs and the server returns HTTP 500 instead of degrading to content.
        assert!(is_native_tool_channel_failure(&thrown(
            "[http_error] ollama 500: tool call parser hit unexpected EOF while parsing"
        )));
        // The same shape carried as a CategorizedError.
        assert!(is_native_tool_channel_failure(&categorized(
            "status 500: server tool extractor failed to parse tool_calls",
            ErrorCategory::ServerError
        )));
        // A stream-cut EOF in the tool-call extractor (no explicit status code).
        assert!(is_native_tool_channel_failure(&thrown(
            "error decoding stream: EOF while parsing a tool call"
        )));
    }

    #[test]
    fn native_tool_channel_failure_ignores_generic_and_keyword_only_errors() {
        // A plain 503 with no tool-parser fingerprint stays an ordinary
        // transient retry — retrying native is correct when the LINK hiccuped.
        assert!(!is_native_tool_channel_failure(&thrown(
            "service unavailable (503): upstream temporarily overloaded"
        )));
        // A 429 rate limit is never a tool-channel failure.
        assert!(!is_native_tool_channel_failure(&thrown(
            "[rate_limited] too many requests"
        )));
        // The word "tool" alone, without a 5xx/EOF, must not trip the degrade
        // (e.g. a 400 complaining about a malformed tool schema).
        assert!(!is_native_tool_channel_failure(&thrown(
            "bad request: tool schema invalid"
        )));
        // A 500 with no tool-parser fingerprint is a generic server error.
        assert!(!is_native_tool_channel_failure(&thrown(
            "[http_error] 500 internal server error"
        )));
    }

    #[test]
    fn billed_noncommittal_throw_matches_vanish_and_function_call_refusal() {
        // (1) The billed-noncommittal contract violation (reasoning-channel-only
        // vanish) — the canonical cheap-model vanishing-call signature, observed
        // 13x on deepinfra gpt-oss-120b (tf=native) in the 2026-06 sweep.
        assert!(is_billed_noncommittal_throw(&thrown(
            "provider deepinfra model openai/gpt-oss-120b returned billed output \
             (completion_tokens=86) with no dispatchable tool call or answer \
             (upstream contract violation): the model finished cleanly but committed \
             neither a tool call nor visible text."
        )));
        // Also matches when re-wrapped as a categorized error.
        assert!(is_billed_noncommittal_throw(&categorized(
            "model m returned billed output (completion_tokens=5) with no dispatchable \
             tool call or answer (upstream contract violation)",
            ErrorCategory::Generic,
        )));
        // (2) The SambaNova native function-call protocol refusal (HTTP 400). It
        // is a 4xx, so `is_native_tool_channel_failure` (5xx/EOF only) does NOT
        // match it, but it is unambiguously a broken native tool channel.
        assert!(is_billed_noncommittal_throw(&thrown(
            "sambanova HTTP 400 Bad Request [invalid_request]: Model started a \
             function call but did not complete it."
        )));
        // 5xx-only path stays separate from the 400 refusal: the refusal is NOT a
        // 5xx/EOF parser choke, so the #3500 predicate must leave it alone.
        assert!(!is_native_tool_channel_failure(&thrown(
            "sambanova HTTP 400 Bad Request [invalid_request]: Model started a \
             function call but did not complete it."
        )));
    }

    #[test]
    fn billed_noncommittal_throw_ignores_stall_and_unrelated_errors() {
        // The bare provider stall (`delivered no content`) has no tool-channel
        // fingerprint: a same-channel retry is the right move, so the degrade
        // predicate must NOT match it (otherwise a transient hiccup would burn
        // the route's one-shot channel degrade).
        assert!(!is_billed_noncommittal_throw(&thrown(
            "openai-compatible model m reported completion_tokens=12 but delivered \
             no content, reasoning, or tool calls"
        )));
        // A billed-noncommittal phrase WITHOUT the billed-output marker must not
        // trip (mirrors the `completion_tokens=` guard in the empty-completion
        // predicate).
        assert!(!is_billed_noncommittal_throw(&thrown(
            "upstream contract violation with no dispatchable tool call or answer"
        )));
        // A generic 429 / rate limit is never a vanished tool channel.
        assert!(!is_billed_noncommittal_throw(&thrown(
            "[rate_limited] too many requests"
        )));
        // A 400 about a malformed tool SCHEMA (our request was wrong) is not a
        // function-call refusal — degrading the channel would not fix it.
        assert!(!is_billed_noncommittal_throw(&thrown(
            "bad request: tool schema invalid"
        )));
    }

    #[test]
    fn truncation_does_not_trigger_channel_degrade() {
        // REMEDY-ORDER INVARIANT: continue-on-truncation must sit ABOVE
        // channel-switch. A `length`/`max_tokens` truncation (valid tool name,
        // incomplete args) is a budget problem the loop continues/raises budget
        // on — NOT a broken channel. A channel switch invalidates the whole
        // prefix KV cache, so it must never fire for a deterministic truncation
        // that would just re-truncate. The billed-noncommittal *throw* is only
        // produced when the turn finished cleanly (the response-layer detector
        // excludes `stop_reason == length`), so a truncation can never reach the
        // degrade trigger; assert the predicates agree.
        assert!(!is_billed_noncommittal_throw(&thrown(
            "model m hit completion_tokens=2048 length cap mid tool call (truncated)"
        )));
        assert!(!is_native_tool_channel_failure(&thrown(
            "model m hit completion_tokens=2048 length cap mid tool call (truncated)"
        )));
        // (The structural detector's exclusion of `stop_reason == length` from the
        // zero-token empty path is covered by
        // `zero_token_empty_completion_predicate_edges`.)
    }

    #[test]
    fn degrade_options_to_text_channel_strips_native_tool_payload() {
        let mut opts = crate::llm::api::options::base_opts("ollama");
        opts.model = "qwen3.6-35b-a3b".to_string();
        opts.native_tools = Some(vec![serde_json::json!({"name": "edit"})]);
        opts.output_format = crate::llm::api::OutputFormat::JsonObject;
        opts.response_format = Some("json".to_string());
        opts.json_schema = Some(serde_json::json!({"type": "object"}));

        let degraded = degrade_options_to_text_channel(&opts);

        assert!(
            degraded.native_tools.is_none(),
            "the provider-native tool payload must be dropped"
        );
        assert!(
            matches!(degraded.output_format, crate::llm::api::OutputFormat::Text),
            "output must fall back to plain Text so the model answers in content"
        );
        assert!(degraded.response_format.is_none());
        assert!(degraded.json_schema.is_none());
        // The logical request is otherwise unchanged.
        assert_eq!(degraded.provider, "ollama");
        assert_eq!(degraded.model, "qwen3.6-35b-a3b");
        // The original is untouched (we degrade a clone).
        assert!(opts.native_tools.is_some());
    }

    #[test]
    fn template_render_event_round_trips_through_jsonl() {
        use crate::stdlib::template::{
            render_template_to_string, LlmRenderContext, LlmRenderContextGuard,
        };
        let dir = tempfile::tempdir().expect("tempdir");
        push_llm_transcript_dir(dir.path().to_str().expect("utf8"));
        {
            let _ctx = LlmRenderContextGuard::enter(LlmRenderContext::resolve(
                "anthropic",
                "claude-opus-4-7",
            ));
            let rendered = render_template_to_string(
                "{{ if llm.capabilities.native_tools }}native{{ else }}text{{ end }}\
                 {{ section \"task\" }}b{{ endsection }}",
                None,
                None,
                None,
            )
            .expect("render");
            assert!(rendered.contains("native"));
            assert!(rendered.contains("<task>"));
        }
        pop_llm_transcript_dir();
        let transcript = std::fs::read_to_string(dir.path().join("llm_transcript.jsonl"))
            .expect("read transcript");
        let line = transcript
            .lines()
            .find(|line| line.contains("\"template.render\""))
            .expect("template.render event present");
        let event: serde_json::Value = serde_json::from_str(line).expect("parse event");
        assert_eq!(event["type"], "template.render");
        assert_eq!(event["llm"]["provider"], "anthropic");
        assert_eq!(event["llm"]["family"], "anthropic-claude");
        assert_eq!(event["llm"]["capabilities"]["native_tools"], true);
        let branches = event["branches"].as_array().expect("branches array");
        let if_branch = branches
            .iter()
            .find(|b| b["kind"] == "if")
            .expect("if branch present");
        assert_eq!(if_branch["branch_id"], "if");
        let section_branch = branches
            .iter()
            .find(|b| b["kind"] == "section")
            .expect("section branch present");
        assert_eq!(section_branch["branch_id"], "xml");
        assert_eq!(section_branch["branch_label"], "task");
    }

    // Fix B regression: for text-format local models (llamacpp/qwen3.6) the
    // tool calls live only inline as `<tool_call>...</tool_call>` in the
    // assistant content — the provider-native `result.tool_calls` array is
    // EMPTY. The `provider_call_response` observability record used to carry
    // only that native array, so the JSONL transcript was not self-describing
    // for text-format runs. The record now also carries a `parsed_tool_calls`
    // sidecar holding the merged (native OR text-parsed) view.
    //
    // Critically this is OBSERVABILITY ONLY: the request-construction / history
    // path keys off `native_tool_calls` (the native-only list from
    // `vm_build_llm_result`, consumed by
    // `agent_session_host::assistant_message_from_llm_result`), which the test
    // also asserts stays empty for a text-format result — so the model's
    // next-turn payload is unchanged.
    #[test]
    fn response_record_exposes_text_parsed_calls_without_touching_history() {
        use super::super::api::{vm_build_llm_result, LlmResult, ProviderTelemetry};
        use crate::value::VmValue;

        // Minimal tool registry so the tagged parser resolves the `run` name
        // (mirrors the `tools` registry the request used).
        fn run_tool_registry() -> VmValue {
            let dict = |pairs: &[(&str, VmValue)]| -> VmValue {
                VmValue::dict(
                    pairs
                        .iter()
                        .map(|(k, v)| (crate::value::intern_key(k), v.clone()))
                        .collect::<crate::value::DictMap>(),
                )
            };
            let s = |v: &str| VmValue::String(arcstr::ArcStr::from(v));
            let run_tool = dict(&[
                ("name", s("run")),
                ("description", s("Run a shell command.")),
                (
                    "parameters",
                    dict(&[(
                        "command",
                        dict(&[("type", s("string")), ("description", s("Shell command."))]),
                    )]),
                ),
            ]);
            dict(&[("tools", VmValue::List(std::sync::Arc::new(vec![run_tool])))])
        }

        // A text-format completion: native tool_calls EMPTY, the call lives
        // inline as a canonical `<tool_call>` block (already canonicalized from
        // any `[[CALL]]` wire form by the time the result reaches here).
        let text = "<tool_call>\nrun({ command: \"ls\" })\n</tool_call>";
        let result = LlmResult {
            served_fast: false,
            text: text.to_string(),
            tool_calls: Vec::new(),
            input_tokens: 12,
            output_tokens: 5,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            cache_supported: true,
            model: "qwen3.6".to_string(),
            provider: "llamacpp".to_string(),
            thinking: None,
            thinking_summary: None,
            stop_reason: Some("stop".to_string()),
            blocks: Vec::new(),
            logprobs: Vec::new(),
            telemetry: ProviderTelemetry::default(),
        };
        let tools = run_tool_registry();

        // 1. Observability path: the response record now exposes the parsed
        //    call via the new sidecar, while native `tool_calls` stays empty.
        let dir = tempfile::tempdir().expect("tempdir");
        push_llm_transcript_dir(dir.path().to_str().expect("utf8"));
        dump_llm_response(0, "call-textfmt", &result, 42, None, Some(&tools));
        pop_llm_transcript_dir();

        let transcript = std::fs::read_to_string(dir.path().join("llm_transcript.jsonl"))
            .expect("read transcript");
        let line = transcript
            .lines()
            .find(|line| line.contains("\"provider_call_response\""))
            .expect("provider_call_response event present");
        let event: serde_json::Value = serde_json::from_str(line).expect("parse event");

        let native = event["tool_calls"].as_array().expect("tool_calls array");
        assert!(
            native.is_empty(),
            "native tool_calls must remain empty for a text-format result, got: {native:?}"
        );
        let parsed = event["parsed_tool_calls"]
            .as_array()
            .expect("parsed_tool_calls array");
        assert_eq!(
            parsed.len(),
            1,
            "the text-parsed call must surface in the sidecar, got: {parsed:?}"
        );
        assert_eq!(parsed[0]["name"], "run");
        // The provider-reported stop reason must ride the observability record
        // (an IDE host bug report: it was dropped here, blinding transcript mining to
        // length truncations on every provider route).
        assert_eq!(event["stop_reason"], "stop");

        // 2. Request-construction / history path is UNCHANGED: the value that
        //    feeds the assistant history envelope is `native_tool_calls`, which
        //    stays empty (native-only) for a text-format result. The merged
        //    `tool_calls` carries the call for unified-view callers, but the
        //    history-feeding native list does not.
        let vm_result = vm_build_llm_result(&result, None, None, Some(&tools));
        let VmValue::Dict(ref dict) = vm_result else {
            panic!("vm_build_llm_result must return a dict");
        };
        let native_history = dict
            .get("native_tool_calls")
            .expect("native_tool_calls present");
        match native_history {
            VmValue::List(items) => assert!(
                items.is_empty(),
                "native_tool_calls (history-feeding list) must stay empty for a \
                 text-format result, got: {items:?}"
            ),
            other => panic!("native_tool_calls must be a list, got {other:?}"),
        }
        // The merged `tool_calls` (unified view) does carry the call — proving
        // the sidecar mirrors the same merge the result builder already does,
        // not a divergent computation.
        let merged_history = dict.get("tool_calls").expect("tool_calls present");
        match merged_history {
            VmValue::List(items) => assert_eq!(
                items.len(),
                1,
                "merged tool_calls (unified view) should carry the text-parsed call"
            ),
            other => panic!("tool_calls must be a list, got {other:?}"),
        }
    }

    #[test]
    fn transcript_dir_option_overrides_env_until_popped() {
        push_llm_transcript_dir("/tmp/harn-transcript-a");
        assert_eq!(
            current_transcript_dir().as_deref(),
            Some("/tmp/harn-transcript-a")
        );
        push_llm_transcript_dir("/tmp/harn-transcript-b");
        assert_eq!(
            current_transcript_dir().as_deref(),
            Some("/tmp/harn-transcript-b")
        );
        pop_llm_transcript_dir();
        assert_eq!(
            current_transcript_dir().as_deref(),
            Some("/tmp/harn-transcript-a")
        );
        pop_llm_transcript_dir();
    }

    // Regression for #2660. `append_llm_transcript_event_log` used to
    // `handle.spawn` the event-log append as a detached task. The agent loop
    // and the test runner drive their tokio runtime with
    // `LocalSet::run_until`, which stops polling the moment the driving future
    // resolves — so those detached appends were never run to completion. Each
    // stranded task pinned a transcript-sized payload plus an
    // `Arc<AnyEventLog>` clone for the lifetime of the runtime, leaking ~one
    // transcript per test across a `harn test --parallel` worker until CI
    // OOM'd. The append must therefore complete synchronously: the entry has
    // to be readable from the log the instant the producing future resolves,
    // without any further runtime polling.
    #[test]
    fn transcript_event_is_appended_synchronously_under_run_until() {
        use crate::event_log::{
            install_memory_for_current_thread, reset_active_event_log, EventLog, Topic,
        };

        reset_active_event_log();
        let log = install_memory_for_current_thread(128);
        let topic = Topic::new("agent.transcript.llm").expect("static topic");

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let local = tokio::task::LocalSet::new();
        runtime.block_on(local.run_until(async {
            // Emit a transcript entry from inside the run_until future, exactly
            // as the agent loop does. With the old detached-spawn path the
            // append task is left scheduled-but-unpolled when this future
            // resolves; with the synchronous path it has already landed.
            append_llm_transcript_entry(&serde_json::json!({
                "type": "provider_call_request",
                "iteration": 0,
                "marker": "regression-2660",
            }));
        }));

        // The driving future has resolved and we are no longer polling the
        // runtime. The event must already be in the log — proving the append
        // ran synchronously rather than on a stranded detached task.
        let latest = futures::executor::block_on(log.latest(&topic))
            .expect("latest query")
            .expect("transcript event must be present immediately after run_until resolves");
        assert_eq!(latest, 1, "exactly one transcript event should be recorded");

        reset_active_event_log();
    }

    #[test]
    fn mock_provider_retry_backoff_is_zero() {
        let config = LlmRetryConfig {
            retries: 1,
            backoff_ms: 2000,
        };
        assert_eq!(
            llm_retry_backoff_ms(&thrown("HTTP 503"), &config, 1, "mock"),
            0
        );
    }

    #[test]
    fn equal_jitter_stays_within_ceil_half_to_ceil_bounds() {
        use rand::{rngs::StdRng, SeedableRng};
        // Sweep many seeds across the exponential ceil ladder and assert every
        // draw lands in `[ceil/2, ceil]` — never near-zero (the win over full
        // jitter), never above the exponential ceiling.
        for attempt in 0..8usize {
            let ceil = 250u64.saturating_mul(1 << attempt.min(4));
            let half = ceil / 2;
            for seed in 0..256u64 {
                let mut rng = StdRng::seed_from_u64(seed);
                let wait = equal_jitter_ms(ceil, &mut rng);
                assert!(
                    wait >= half && wait <= ceil,
                    "attempt={attempt} ceil={ceil} seed={seed} wait={wait} out of [{half}, {ceil}]"
                );
            }
        }
    }

    #[test]
    fn equal_jitter_desynchronizes_concurrent_callers() {
        use rand::{rngs::StdRng, SeedableRng};
        // Two callers drawing from distinct entropy must not resume in lockstep:
        // at least one differing pair proves the herd is broken (the old
        // zero-jitter path returned the identical `ceil` for every caller).
        let ceil = 4000u64;
        let mut any_differ = false;
        for seed in 0..64u64 {
            let a = equal_jitter_ms(ceil, &mut StdRng::seed_from_u64(seed));
            let b = equal_jitter_ms(ceil, &mut StdRng::seed_from_u64(seed + 10_000));
            if a != b {
                any_differ = true;
                break;
            }
        }
        assert!(any_differ, "equal jitter failed to desynchronize any pair");
    }

    #[test]
    fn equal_jitter_tiny_ceil_floors_at_ceil() {
        use rand::{rngs::StdRng, SeedableRng};
        // ceil/2 == 0 (e.g. ceil < 2) cannot host a range; return the ceil.
        let mut rng = StdRng::seed_from_u64(1);
        assert_eq!(equal_jitter_ms(0, &mut rng), 0);
        assert_eq!(equal_jitter_ms(1, &mut rng), 1);
    }

    #[test]
    fn retry_after_path_adds_bounded_jitter() {
        // A provider Retry-After is honored as a floor; the layered jitter never
        // exceeds `retry_after + backoff_base`, so the wait stays in a tight band
        // around the requested cooldown while desynchronizing siblings.
        let config = LlmRetryConfig {
            retries: 3,
            backoff_ms: DEFAULT_LLM_CALL_BACKOFF_MS,
        };
        let err = thrown("HTTP 429 rate_limited retry-after: 5");
        let base = extract_retry_after_ms(&err).expect("retry-after parsed");
        for _ in 0..256 {
            let wait = llm_retry_backoff_ms(&err, &config, 1, "openai");
            assert!(
                wait >= base && wait <= base + DEFAULT_LLM_CALL_BACKOFF_MS,
                "retry-after jitter {wait} out of [{base}, {}]",
                base + DEFAULT_LLM_CALL_BACKOFF_MS
            );
        }
    }

    #[test]
    fn base_backoff_real_provider_respects_exponential_ceiling() {
        // The live (un-seamed) path must still land in the equal-jitter band for
        // the configured base, exercising `rand::rng()` rather than the test RNG.
        let config = LlmRetryConfig {
            retries: 5,
            backoff_ms: DEFAULT_LLM_CALL_BACKOFF_MS,
        };
        for attempt in 1..=6usize {
            let ceil = DEFAULT_LLM_CALL_BACKOFF_MS.saturating_mul(1 << attempt.min(4));
            for _ in 0..64 {
                let wait = base_retry_backoff_ms(&config, attempt);
                assert!(
                    wait >= ceil / 2 && wait <= ceil,
                    "attempt={attempt} wait={wait} out of [{}, {ceil}]",
                    ceil / 2
                );
            }
        }
    }

    #[test]
    fn categorized_overloaded_is_retryable() {
        assert!(is_retryable_llm_error(&categorized(
            "upstream overloaded",
            ErrorCategory::Overloaded
        )));
    }

    #[test]
    fn overloaded_errors_feed_breaker_but_network_and_server_classes_stay_distinct() {
        // 529 / overloaded_error (the Anthropic overload shapes) must count as
        // breaker-feeding overload...
        assert!(is_overloaded_llm_error(&thrown(
            "anthropic HTTP 529 [http_error]: {\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}"
        )));
        assert!(is_overloaded_llm_error(&categorized(
            "upstream overloaded",
            ErrorCategory::Overloaded
        )));
        // ...while 429 stays rate limiting and generic 500/502 stays a plain
        // server error — neither may trip the breaker.
        assert!(!is_overloaded_llm_error(&thrown(
            "openai HTTP 429 [rate_limited]: too many requests"
        )));
        assert!(!is_overloaded_llm_error(&thrown(
            "openai HTTP 500 [http_error]: internal"
        )));
        assert!(!is_overloaded_llm_error(&categorized(
            "500 internal",
            ErrorCategory::ServerError
        )));
        // Overload is not a *network* failure — it reaches the breaker through
        // its own predicate, not by widening the network classifier.
        assert!(!is_network_failure_llm_error(&categorized(
            "upstream overloaded",
            ErrorCategory::Overloaded
        )));
    }

    #[test]
    fn shared_cooldown_covers_overload_with_default_and_honors_retry_after() {
        // Overload without Retry-After: fixed default window so sibling agents
        // stop hammering the provider.
        assert_eq!(
            shared_cooldown_ms_for_llm_error(&thrown(
                "anthropic HTTP 529 [http_error]: {\"type\":\"overloaded_error\"}"
            )),
            crate::llm::rate_limit::OVERLOAD_COOLDOWN_MS
        );
        // Overload WITH Retry-After: the provider's signal wins.
        assert_eq!(
            shared_cooldown_ms_for_llm_error(&thrown(
                "anthropic HTTP 529 [http_error]: overloaded_error (retry-after: 7)"
            )),
            7_000
        );
        // 429 keeps its existing semantics: Retry-After when sent, no default.
        assert_eq!(
            shared_cooldown_ms_for_llm_error(&thrown(
                "openai HTTP 429 [rate_limited]: slow down (retry-after: 3)"
            )),
            3_000
        );
        assert_eq!(
            shared_cooldown_ms_for_llm_error(&thrown("openai HTTP 429 [rate_limited]: slow down")),
            0
        );
        // Generic 500 and network failures never cool the shared route down.
        assert_eq!(
            shared_cooldown_ms_for_llm_error(&thrown("openai HTTP 500 [http_error]: internal")),
            0
        );
        assert_eq!(
            shared_cooldown_ms_for_llm_error(&categorized(
                "connection reset",
                ErrorCategory::TransientNetwork
            )),
            0
        );
    }

    #[test]
    fn categorized_server_error_is_retryable() {
        assert!(is_retryable_llm_error(&categorized(
            "500 internal",
            ErrorCategory::ServerError
        )));
    }

    #[test]
    fn categorized_transient_network_is_retryable() {
        assert!(is_retryable_llm_error(&categorized(
            "reset",
            ErrorCategory::TransientNetwork
        )));
    }

    #[test]
    fn categorized_auth_not_retryable() {
        assert!(!is_retryable_llm_error(&categorized(
            "invalid key",
            ErrorCategory::Auth
        )));
    }

    /// The billed-noncommittal thrown error (`response.rs`/`transport.rs`
    /// `billed_noncommittal_completion_error`) is NOT matched by the generic
    /// terminal-error classifier — that is precisely why the loop used to
    /// hard-break on it — but IS matched by the empty-completion retry
    /// predicate so it routes onto the bounded empty-completion budget.
    #[test]
    fn billed_noncommittal_is_empty_completion_retry_not_generic_retryable() {
        let billed = thrown(
            "provider openrouter model qwen/qwen3.6-35b-a3b returned billed output \
             (completion_tokens=342) with no dispatchable tool call or answer \
             (upstream contract violation): the model finished cleanly but committed \
             neither a tool call nor visible text.",
        );
        assert!(
            !is_retryable_llm_error(&billed),
            "generic terminal classifier must NOT match (root cause of the hard-break)"
        );
        assert!(
            is_empty_completion_retry_error(&billed),
            "must route onto the empty-completion retry budget"
        );
    }

    /// Edge truth-table for `is_empty_completion_retry_error`: both thrown
    /// unproductive shapes match; unrelated errors and partial signatures do
    /// not (so a real terminal error never gets laundered into a retry).
    #[test]
    fn empty_completion_retry_error_edges() {
        // (1) zero-token empty completion (transport guard).
        assert!(is_empty_completion_retry_error(&thrown(
            "openai-compatible model m reported completion_tokens=12 but delivered no content, reasoning, or tool calls"
        )));
        // (2) billed-noncommittal contract violation.
        assert!(is_empty_completion_retry_error(&thrown(
            "provider p model m returned billed output (completion_tokens=5) with no dispatchable tool call or answer (upstream contract violation)"
        )));
        // Also matches when re-wrapped as a categorized error.
        assert!(is_empty_completion_retry_error(&categorized(
            "model m returned billed output (completion_tokens=5) with no dispatchable tool call or answer (upstream contract violation)",
            ErrorCategory::Generic,
        )));
        // No `completion_tokens=` token: not the billed shape.
        assert!(!is_empty_completion_retry_error(&thrown(
            "upstream contract violation with no dispatchable tool call or answer"
        )));
        // A genuine terminal/context error must never match.
        assert!(!is_empty_completion_retry_error(&thrown(
            "local HTTP 400 Bad Request [context_overflow]: prompt is too long"
        )));
        // A 429 is retryable elsewhere but is not an empty-completion shape.
        assert!(!is_empty_completion_retry_error(&thrown(
            "429 too many requests"
        )));
    }

    #[test]
    fn llm_error_kind_dict_gates_retry() {
        let transient = VmError::Thrown(VmValue::dict(std::collections::BTreeMap::from([
            (
                "kind".to_string(),
                VmValue::String(arcstr::ArcStr::from("transient")),
            ),
            (
                "reason".to_string(),
                VmValue::String(arcstr::ArcStr::from("network_error")),
            ),
        ])));
        assert!(is_retryable_llm_error(&transient));

        let terminal = VmError::Thrown(VmValue::dict(std::collections::BTreeMap::from([
            (
                "kind".to_string(),
                VmValue::String(arcstr::ArcStr::from("terminal")),
            ),
            (
                "reason".to_string(),
                VmValue::String(arcstr::ArcStr::from("context_overflow")),
            ),
        ])));
        assert!(!is_retryable_llm_error(&terminal));
    }

    #[test]
    fn context_overflow_message_is_not_retryable() {
        assert!(!is_retryable_llm_error(&thrown(
            "local HTTP 400 Bad Request [context_overflow]: prompt is too long"
        )));
    }

    #[test]
    fn http_503_is_retryable_via_classifier() {
        assert!(is_retryable_llm_error(&thrown(
            "HTTP 503 Service Unavailable"
        )));
    }

    #[test]
    fn http_504_is_retryable() {
        assert!(is_retryable_llm_error(&thrown("HTTP 504 Gateway Timeout")));
    }

    #[test]
    fn http_529_is_retryable() {
        assert!(is_retryable_llm_error(&thrown("HTTP 529 overloaded_error")));
    }

    #[test]
    fn bad_gateway_string_is_retryable() {
        assert!(is_retryable_llm_error(&thrown("bad gateway response")));
    }

    #[test]
    fn service_unavailable_string_is_retryable() {
        assert!(is_retryable_llm_error(&thrown("service unavailable")));
    }

    #[test]
    fn auth_error_not_retryable() {
        assert!(!is_retryable_llm_error(&thrown("HTTP 401 Unauthorized")));
    }

    #[test]
    fn retry_after_integer_seconds() {
        assert_eq!(parse_retry_after("err: retry-after: 5"), Some(5_000));
    }

    #[test]
    fn retry_after_fractional_seconds() {
        assert_eq!(parse_retry_after("retry-after: 2.5"), Some(2_500));
    }

    #[test]
    fn retry_after_seconds_with_provider_message_punctuation() {
        let msg = "cerebras HTTP 429 Too Many Requests [rate_limited]: Tokens per minute limit exceeded (type: too_many_tokens_error, code: token_quota_exceeded) (retry-after: 60))";
        assert_eq!(parse_retry_after(msg), Some(60_000));
    }

    #[test]
    fn retry_after_clamped_to_cap() {
        assert_eq!(parse_retry_after("retry-after: 600"), Some(60_000));
    }

    #[test]
    fn retry_after_http_date_past_is_zero() {
        let past = "retry-after: Mon, 01 Jan 1990 00:00:00 GMT";
        assert_eq!(parse_retry_after(past), Some(0));
    }

    #[test]
    fn retry_after_missing_returns_none() {
        assert_eq!(parse_retry_after("nothing here"), None);
    }

    #[test]
    fn retry_after_malformed_returns_none() {
        assert_eq!(parse_retry_after("retry-after: soon-ish"), None);
    }
}

#[cfg(test)]
mod empty_completion_retry_tests {
    //! Zero-token empty-completion retry coverage. A provider stall can end
    //! with an empty HTTP 200 (observed live on OpenRouter: 133s hang,
    //! `output_tokens=0`), which is not an error at the wire level —
    //! `observed_llm_call` must treat it as a transient hiccup and retry,
    //! and must return the empty result unchanged once the budget is spent.
    //! Driven through `FakeLlmProvider` (an empty scripted stream produces
    //! exactly the zero-token empty shape) so the full retry loop runs
    //! without network I/O.

    use super::*;
    use crate::llm::fake::{
        fake_llm_captured_calls, install_fake_llm_script, FakeLlmEvent, FakeLlmScript, FakeLlmTurn,
        FakeStopReason,
    };
    use crate::llm::trace::{peek_agent_trace, reset_agent_trace_state, AgentTraceEvent};

    fn fake_opts() -> crate::llm::api::LlmCallOptions {
        let mut opts = crate::llm::api::options::base_opts("fake");
        opts.model = "fake-stream".to_string();
        opts.native_tools = None;
        opts.tools = None;
        opts.tool_choice = None;
        opts.provider_overrides = None;
        opts
    }

    fn current_thread_runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
    }

    fn empty_turn() -> FakeLlmTurn {
        FakeLlmTurn::stream(vec![FakeLlmEvent::Done(FakeStopReason::EndTurn)])
    }

    /// The live cheap-model failure: the turn narrates an intended tool call in
    /// its text but finishes with a provider error (`stop_reason == "error"`)
    /// and emits ZERO tool calls. Non-empty text means the zero-token predicate
    /// misses it, yet the loop has no action to run.
    fn errored_actionless_turn() -> FakeLlmTurn {
        FakeLlmTurn::stream(vec![
            FakeLlmEvent::Token("We need to make edit to create tests/foo_test.cpp".into()),
            FakeLlmEvent::Done(FakeStopReason::Custom("error".into())),
        ])
    }

    fn retry_config(retries: usize) -> LlmRetryConfig {
        LlmRetryConfig {
            retries,
            backoff_ms: 0,
        }
    }

    /// The *thrown* billed-noncommittal failure. The response/transport parsers
    /// raise `billed_noncommittal_completion_error` when a clean-finish turn
    /// bills output but commits no tool call or visible text (the action went
    /// only to the reasoning channel). The fake provider can't drive the real parser,
    /// so we re-create the exact wire-error message under a *non-transient*
    /// `Generic` category — proving the retry comes from the empty-completion
    /// budget, not from `is_retryable_llm_error` (which returns false here).
    fn billed_noncommittal_turn() -> FakeLlmTurn {
        FakeLlmTurn::Error(crate::llm::fake::FakeLlmError::new(
            crate::value::ErrorCategory::Generic,
            "provider openrouter model qwen/qwen3.6-35b-a3b returned billed output \
             (completion_tokens=342) with no dispatchable tool call or answer \
             (upstream contract violation): the model finished cleanly but committed \
             neither a tool call nor visible text.",
        ))
    }

    #[test]
    fn empty_completion_retries_then_succeeds_on_second_attempt() {
        current_thread_runtime().block_on(async {
            reset_agent_trace_state();
            let _guard = install_fake_llm_script(FakeLlmScript::new().push(empty_turn()).push(
                FakeLlmTurn::stream(vec![
                    FakeLlmEvent::Token("recovered".into()),
                    FakeLlmEvent::Done(FakeStopReason::EndTurn),
                ]),
            ));
            let result = observed_llm_call(
                &fake_opts(),
                None,
                None,
                &retry_config(1),
                None,
                false,
                false,
                None,
            )
            .await
            .expect("empty completion retry should recover");
            assert_eq!(result.text, "recovered");

            let retries: Vec<usize> = peek_agent_trace()
                .iter()
                .filter_map(|event| match event {
                    AgentTraceEvent::EmptyCompletionRetry { attempt, .. } => Some(*attempt),
                    _ => None,
                })
                .collect();
            assert_eq!(
                retries,
                vec![1],
                "expected exactly one EmptyCompletionRetry trace event"
            );
            reset_agent_trace_state();
            // _guard drop asserts both scripted turns were consumed.
        });
    }

    #[test]
    fn errored_actionless_completion_retries_then_succeeds() {
        // An errored turn that narrated intent but emitted no tool call must be
        // RETRIED (not advanced on); the next good turn proceeds.
        current_thread_runtime().block_on(async {
            reset_agent_trace_state();
            let _guard =
                install_fake_llm_script(FakeLlmScript::new().push(errored_actionless_turn()).push(
                    FakeLlmTurn::stream(vec![
                        FakeLlmEvent::Token("recovered".into()),
                        FakeLlmEvent::Done(FakeStopReason::EndTurn),
                    ]),
                ));
            let result = observed_llm_call(
                &fake_opts(),
                None,
                None,
                &retry_config(1),
                None,
                false,
                false,
                None,
            )
            .await
            .expect("errored-actionless retry should recover");
            assert_eq!(result.text, "recovered");

            let retries: Vec<usize> = peek_agent_trace()
                .iter()
                .filter_map(|event| match event {
                    AgentTraceEvent::EmptyCompletionRetry { attempt, .. } => Some(*attempt),
                    _ => None,
                })
                .collect();
            assert_eq!(retries, vec![1], "expected exactly one retry trace event");
            reset_agent_trace_state();
            // _guard drop asserts both scripted turns were consumed.
        });
    }

    #[test]
    fn errored_actionless_completion_returns_unchanged_after_budget_exhausted() {
        // Retries stay bounded: once the budget is spent, the errored turn is
        // returned unchanged (callers see today's shape, not a novel error).
        current_thread_runtime().block_on(async {
            reset_agent_trace_state();
            let _guard = install_fake_llm_script(
                FakeLlmScript::new()
                    .push(errored_actionless_turn())
                    .push(errored_actionless_turn()),
            );
            let result = observed_llm_call(
                &fake_opts(),
                None,
                None,
                &retry_config(1),
                None,
                false,
                false,
                None,
            )
            .await
            .expect("exhausted retries must return Ok, not a new error");
            assert!(result.tool_calls.is_empty());
            assert_eq!(result.stop_reason.as_deref(), Some("error"));

            let retries = peek_agent_trace()
                .iter()
                .filter(|event| matches!(event, AgentTraceEvent::EmptyCompletionRetry { .. }))
                .count();
            assert_eq!(retries, 1, "exactly one retry before the budget is spent");
            reset_agent_trace_state();
        });
    }

    #[test]
    fn empty_completion_returns_result_unchanged_after_budget_exhausted() {
        current_thread_runtime().block_on(async {
            reset_agent_trace_state();
            let _guard =
                install_fake_llm_script(FakeLlmScript::new().push(empty_turn()).push(empty_turn()));
            let result = observed_llm_call(
                &fake_opts(),
                None,
                None,
                &retry_config(1),
                None,
                false,
                false,
                None,
            )
            .await
            .expect("exhausted empty-completion retries must return Ok, not a new error");
            assert!(result.text.is_empty());
            assert!(result.tool_calls.is_empty());
            assert_eq!(result.output_tokens, 0);
            reset_agent_trace_state();
        });
    }

    #[test]
    fn billed_noncommittal_completion_retries_then_succeeds() {
        // The KEY missing case: a *thrown* billed-noncommittal turn (clean
        // finish, billed output, tool call only on the reasoning channel) must
        // be RETRIED within the empty-completion budget — not hard-break the
        // loop as a silent `provider_error` — and the next good turn proceeds.
        current_thread_runtime().block_on(async {
            reset_agent_trace_state();
            let _guard = install_fake_llm_script(
                FakeLlmScript::new()
                    .push(billed_noncommittal_turn())
                    .push(FakeLlmTurn::stream(vec![
                        FakeLlmEvent::Token("recovered".into()),
                        FakeLlmEvent::Done(FakeStopReason::EndTurn),
                    ])),
            );
            let result = observed_llm_call(
                &fake_opts(),
                None,
                None,
                &retry_config(1),
                None,
                false,
                false,
                None,
            )
            .await
            .expect("billed-noncommittal retry should recover");
            assert_eq!(result.text, "recovered");

            let retries: Vec<usize> = peek_agent_trace()
                .iter()
                .filter_map(|event| match event {
                    AgentTraceEvent::EmptyCompletionRetry { attempt, .. } => Some(*attempt),
                    _ => None,
                })
                .collect();
            assert_eq!(
                retries,
                vec![1],
                "expected exactly one EmptyCompletionRetry trace event"
            );
            reset_agent_trace_state();
            // _guard drop asserts both scripted turns were consumed.
        });
    }

    #[test]
    fn billed_noncommittal_completion_surfaces_contract_violation_after_budget_exhausted() {
        // Bounded retry on a chronically-broken upstream: once the budget is
        // spent the LOUD thrown error is surfaced unchanged (NOT a silent
        // advance), and it still names the `upstream contract violation` so the
        // a host eval layer can classify it as infra, not capability.
        current_thread_runtime().block_on(async {
            reset_agent_trace_state();
            let _guard = install_fake_llm_script(
                FakeLlmScript::new()
                    .push(billed_noncommittal_turn())
                    .push(billed_noncommittal_turn()),
            );
            let err = observed_llm_call(
                &fake_opts(),
                None,
                None,
                &retry_config(1),
                None,
                false,
                false,
                None,
            )
            .await
            .expect_err("exhausted billed-noncommittal retries must surface the loud error");
            let message = err.to_string();
            assert!(
                message.contains("upstream contract violation"),
                "exhausted-path error must stay tagged as a provider contract violation: {message}"
            );
            assert!(
                message.contains("completion_tokens="),
                "exhausted-path error must keep the billed-output signature: {message}"
            );

            let retries = peek_agent_trace()
                .iter()
                .filter(|event| matches!(event, AgentTraceEvent::EmptyCompletionRetry { .. }))
                .count();
            assert_eq!(retries, 1, "exactly one retry before the budget is spent");
            reset_agent_trace_state();
        });
    }

    #[test]
    fn fake_provider_without_retry_budget_does_not_silently_retry() {
        // Mock/fake providers replay scripted turns, so the built-in
        // empty-completion floor must not apply to them — only an explicit
        // budget. One scripted turn, zero retries: the guard would panic on
        // drop if a hidden retry consumed a second turn.
        current_thread_runtime().block_on(async {
            let _guard = install_fake_llm_script(FakeLlmScript::new().push(empty_turn()));
            let result = observed_llm_call(
                &fake_opts(),
                None,
                None,
                &retry_config(0),
                None,
                false,
                false,
                None,
            )
            .await
            .expect("empty completion without budget returns as today");
            assert!(result.text.is_empty());
        });
    }

    #[test]
    fn rate_limit_429_with_retry_after_triggers_bounded_retry() {
        // A 429 carrying a Retry-After hint must engage the dormant backoff
        // loop (retries > 0) and recover on the next turn, rather than failing
        // hard on the first transient error. The _guard drop asserts BOTH
        // scripted turns were consumed — i.e. the retry actually fired.
        current_thread_runtime().block_on(async {
            reset_agent_trace_state();
            let _guard = install_fake_llm_script(
                FakeLlmScript::new()
                    .push(FakeLlmTurn::Error(
                        crate::llm::fake::FakeLlmError::new(
                            crate::value::ErrorCategory::RateLimit,
                            "429 too many requests",
                        )
                        .with_retry_after_ms(10),
                    ))
                    .push(FakeLlmTurn::stream(vec![
                        FakeLlmEvent::Token("recovered".into()),
                        FakeLlmEvent::Done(FakeStopReason::EndTurn),
                    ])),
            );
            let result = observed_llm_call(
                &fake_opts(),
                None,
                None,
                &retry_config(2),
                None,
                false,
                false,
                None,
            )
            .await
            .expect("429 with retry-after should retry and recover");
            assert_eq!(result.text, "recovered");
            reset_agent_trace_state();
        });
    }

    #[test]
    fn rate_limit_429_fails_hard_when_retry_budget_is_zero() {
        // Regression guard: at retries=0 the same 429 is a hard failure (this
        // is exactly the gap the default bump to >=2 closes). Only one turn is
        // scripted, so a hidden retry would panic the guard on drop.
        current_thread_runtime().block_on(async {
            let _guard = install_fake_llm_script(
                FakeLlmScript::new().push(FakeLlmTurn::Error(
                    crate::llm::fake::FakeLlmError::new(
                        crate::value::ErrorCategory::RateLimit,
                        "429 too many requests",
                    )
                    .with_retry_after_ms(10),
                )),
            );
            let err = observed_llm_call(
                &fake_opts(),
                None,
                None,
                &retry_config(0),
                None,
                false,
                false,
                None,
            )
            .await
            .expect_err("429 with no retry budget must surface as an error");
            assert!(
                is_retryable_llm_error(&err),
                "the surfaced 429 should classify as retryable"
            );
        });
    }

    fn native_opts() -> crate::llm::api::LlmCallOptions {
        let mut opts = fake_opts();
        opts.native_tools = Some(vec![serde_json::json!({"name": "edit"})]);
        opts.tools = Some(crate::value::VmValue::Nil);
        opts
    }

    #[test]
    fn native_tool_channel_failure_degrades_to_text_and_recovers() {
        // The runtime tool_format fallback: a native-pinned tool call whose
        // server-side parser 500/EOFs degrades to the text channel and recovers
        // on the retry. Crucially this fires at retries=0 (the transient budget
        // is spent) — retrying native would re-feed the broken parser forever;
        // the productive move is to switch channels. The _guard drop asserts the
        // second (text-channel) turn was actually consumed.
        current_thread_runtime().block_on(async {
            let _guard = install_fake_llm_script(
                FakeLlmScript::new()
                    .push(FakeLlmTurn::Error(crate::llm::fake::FakeLlmError::new(
                        crate::value::ErrorCategory::ServerError,
                        "[http_error] ollama 500: tool call parser hit unexpected EOF \
                         while parsing tool_calls",
                    )))
                    .push(FakeLlmTurn::stream(vec![
                        FakeLlmEvent::Token(
                            "<tool_call>\nedit({ path: \"a.rs\" })\n</tool_call>".into(),
                        ),
                        FakeLlmEvent::Done(FakeStopReason::EndTurn),
                    ])),
            );
            let result = observed_llm_call(
                &native_opts(),
                Some("native"),
                None,
                &retry_config(0),
                None,
                false,
                false,
                None,
            )
            .await
            .expect("native tool-channel failure should degrade to text and recover");
            assert!(
                result.text.contains("edit({ path: \"a.rs\" })"),
                "the degraded text-channel turn should be returned"
            );
        });
    }

    #[test]
    fn native_tool_channel_failure_degrade_fires_at_most_once() {
        // The degrade is one-shot per call: if the text-channel retry ALSO
        // fails (a different problem), the loop does not keep re-degrading. With
        // retries=0 and two failing turns scripted, only the FIRST degrade may
        // fire; the second failure must surface as a hard error. Exactly two
        // turns are scripted, so a third call would panic the guard on drop.
        current_thread_runtime().block_on(async {
            let _guard = install_fake_llm_script(
                FakeLlmScript::new()
                    .push(FakeLlmTurn::Error(crate::llm::fake::FakeLlmError::new(
                        crate::value::ErrorCategory::ServerError,
                        "[http_error] 500: tool call parser unexpected EOF",
                    )))
                    .push(FakeLlmTurn::Error(crate::llm::fake::FakeLlmError::new(
                        crate::value::ErrorCategory::ServerError,
                        "[http_error] 500: tool call parser unexpected EOF",
                    ))),
            );
            let err = observed_llm_call(
                &native_opts(),
                Some("native"),
                None,
                &retry_config(0),
                None,
                false,
                false,
                None,
            )
            .await
            .expect_err("a second tool-channel failure after degrade must surface");
            assert!(
                err.to_string().to_lowercase().contains("tool call parser"),
                "the surfaced error is the second (post-degrade) failure"
            );
        });
    }

    #[test]
    fn stream_body_failure_degrades_to_non_streaming_and_recovers() {
        // A provider that accepts the request and then fails while reading the
        // streaming body should not keep retrying the identical SSE path. If the
        // route does not require streaming, retry once through the ordinary
        // request/response transport. The fake provider records the forwarded
        // request, so this test verifies the second call actually carries
        // `stream=false`.
        current_thread_runtime().block_on(async {
            let mut opts = fake_opts();
            opts.stream = true;
            let _guard = install_fake_llm_script(
                FakeLlmScript::new()
                    .push(FakeLlmTurn::Error(crate::llm::fake::FakeLlmError::new(
                        crate::value::ErrorCategory::TransientNetwork,
                        "llamacpp stream error (mid-stream read): error decoding response body",
                    )))
                    .push(FakeLlmTurn::stream(vec![
                        FakeLlmEvent::Token("recovered".into()),
                        FakeLlmEvent::Done(FakeStopReason::EndTurn),
                    ])),
            );
            let result = observed_llm_call(
                &opts,
                None,
                None,
                &retry_config(0),
                None,
                false,
                false,
                None,
            )
            .await
            .expect("stream body failure should degrade transport and recover");
            assert_eq!(result.text, "recovered");

            let calls = fake_llm_captured_calls();
            assert_eq!(calls.len(), 2, "expected one degraded retry");
            assert!(calls[0].stream, "first call should use streaming");
            assert!(
                !calls[1].stream,
                "degraded retry should use non-streaming transport"
            );
        });
    }

    #[test]
    fn stream_transport_degrade_fires_at_most_once() {
        current_thread_runtime().block_on(async {
            let mut opts = fake_opts();
            opts.stream = true;
            let _guard = install_fake_llm_script(
                FakeLlmScript::new()
                    .push(FakeLlmTurn::Error(crate::llm::fake::FakeLlmError::new(
                        crate::value::ErrorCategory::TransientNetwork,
                        "llamacpp stream error (mid-stream read): error decoding response body",
                    )))
                    .push(FakeLlmTurn::Error(crate::llm::fake::FakeLlmError::new(
                        crate::value::ErrorCategory::TransientNetwork,
                        "llamacpp stream error (mid-stream read): error decoding response body",
                    ))),
            );
            let err = observed_llm_call(
                &opts,
                None,
                None,
                &retry_config(0),
                None,
                false,
                false,
                None,
            )
            .await
            .expect_err("second transport failure after degrade must surface");
            assert!(
                err.to_string().contains("stream error"),
                "surface the post-degrade provider error, got: {err}"
            );

            let calls = fake_llm_captured_calls();
            assert_eq!(calls.len(), 2, "degrade should be one-shot");
            assert!(calls[0].stream);
            assert!(!calls[1].stream);
        });
    }

    #[test]
    fn billed_noncommittal_throw_degrades_to_text_and_recovers() {
        // Mechanism-fitness: the canonical cheap-model vanishing-call signature
        // (billed output, clean finish, zero tool calls — the action stranded in
        // the reasoning channel) on a NATIVE channel must degrade to text and
        // recover, NOT loop re-feeding the broken native channel. Before this
        // generalization the throw routed onto the bounded SAME-CHANNEL empty-
        // completion retry and never switched channels. The _guard drop asserts
        // the second (text-channel) turn was actually consumed.
        current_thread_runtime().block_on(async {
            let _guard = install_fake_llm_script(
                FakeLlmScript::new()
                    .push(FakeLlmTurn::Error(crate::llm::fake::FakeLlmError::new(
                        crate::value::ErrorCategory::Generic,
                        "provider deepinfra model openai/gpt-oss-120b returned billed \
                         output (completion_tokens=86) with no dispatchable tool call \
                         or answer (upstream contract violation): the model finished \
                         cleanly but committed neither a tool call nor visible text.",
                    )))
                    .push(FakeLlmTurn::stream(vec![
                        FakeLlmEvent::Token(
                            "<tool_call>\nedit({ path: \"a.rs\" })\n</tool_call>".into(),
                        ),
                        FakeLlmEvent::Done(FakeStopReason::EndTurn),
                    ])),
            );
            let result = observed_llm_call(
                &native_opts(),
                Some("native"),
                None,
                &retry_config(0),
                None,
                false,
                false,
                None,
            )
            .await
            .expect("billed-noncommittal vanish should degrade to text and recover");
            assert!(
                result.text.contains("edit({ path: \"a.rs\" })"),
                "the degraded text-channel turn should be returned"
            );
        });
    }

    #[test]
    fn sambanova_function_call_refusal_degrades_to_text_and_recovers() {
        // Mechanism-fitness: a native function-call protocol refusal (the observed
        // SambaNova HTTP 400 "Model started a function call but did not complete
        // it") is a 4xx, so the #3500 5xx/EOF predicate deliberately misses it and
        // it is NOT a generic-retryable error — yet it is unambiguously a broken
        // native tool channel for the route. It must earn the one-shot degrade to
        // text rather than aborting the run (the sweep showed 6 such calls abort a
        // run today).
        current_thread_runtime().block_on(async {
            let _guard = install_fake_llm_script(
                FakeLlmScript::new()
                    .push(FakeLlmTurn::Error(crate::llm::fake::FakeLlmError::new(
                        crate::value::ErrorCategory::Generic,
                        "sambanova HTTP 400 Bad Request [invalid_request]: Model \
                         started a function call but did not complete it.",
                    )))
                    .push(FakeLlmTurn::stream(vec![
                        FakeLlmEvent::Token(
                            "<tool_call>\nedit({ path: \"a.rs\" })\n</tool_call>".into(),
                        ),
                        FakeLlmEvent::Done(FakeStopReason::EndTurn),
                    ])),
            );
            let result = observed_llm_call(
                &native_opts(),
                Some("native"),
                None,
                &retry_config(0),
                None,
                false,
                false,
                None,
            )
            .await
            .expect("native function-call refusal should degrade to text and recover");
            assert!(
                result.text.contains("edit({ path: \"a.rs\" })"),
                "the degraded text-channel turn should be returned"
            );
        });
    }

    #[test]
    fn builtin_empty_retry_budget_floors_real_providers_only() {
        let zero = retry_config(0);
        assert_eq!(empty_completion_retry_budget(&zero, "openrouter"), 1);
        assert_eq!(empty_completion_retry_budget(&zero, "fake"), 0);
        assert_eq!(empty_completion_retry_budget(&zero, "mock"), 0);
        let three = retry_config(3);
        assert_eq!(empty_completion_retry_budget(&three, "openrouter"), 3);
        assert_eq!(empty_completion_retry_budget(&three, "fake"), 3);
    }

    fn empty_result() -> crate::llm::api::LlmResult {
        crate::llm::api::LlmResult {
            text: String::new(),
            tool_calls: Vec::new(),
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            cache_supported: true,
            model: "test-model".to_string(),
            provider: "openrouter".to_string(),
            thinking: None,
            thinking_summary: None,
            stop_reason: Some("stop".to_string()),
            served_fast: false,
            blocks: Vec::new(),
            logprobs: Vec::new(),
            telemetry: crate::llm::api::ProviderTelemetry::default(),
        }
    }

    #[test]
    fn zero_token_empty_completion_predicate_edges() {
        assert!(is_zero_token_empty_completion(&empty_result()));

        // Token-cap truncation is deterministic — not a retryable hiccup.
        let mut truncated = empty_result();
        truncated.stop_reason = Some("length".to_string());
        assert!(!is_zero_token_empty_completion(&truncated));
        let mut truncated_upper = empty_result();
        truncated_upper.stop_reason = Some("MAX_TOKENS".to_string());
        assert!(!is_zero_token_empty_completion(&truncated_upper));

        // Any delivered payload disqualifies.
        let mut with_text = empty_result();
        with_text.text = "hi".to_string();
        assert!(!is_zero_token_empty_completion(&with_text));
        let mut with_tokens = empty_result();
        with_tokens.output_tokens = 3;
        assert!(!is_zero_token_empty_completion(&with_tokens));
        let mut with_thinking = empty_result();
        with_thinking.thinking = Some("hmm".to_string());
        assert!(!is_zero_token_empty_completion(&with_thinking));
        let mut with_tool_call = empty_result();
        with_tool_call.tool_calls = vec![serde_json::json!({"id": "t1", "name": "look"})];
        assert!(!is_zero_token_empty_completion(&with_tool_call));
        let mut with_tool_search = empty_result();
        with_tool_search.blocks = vec![serde_json::json!({"type": "tool_search_query"})];
        assert!(!is_zero_token_empty_completion(&with_tool_search));
    }

    #[test]
    fn errored_actionless_completion_predicate_edges() {
        // The live failure shape: stop_reason=error, the model narrated an
        // intended tool call in its text but emitted ZERO tool calls. The
        // zero-token predicate misses it (non-empty text), but it IS retryable.
        let mut narrated = empty_result();
        narrated.stop_reason = Some("error".to_string());
        narrated.text = "We need to make edit to create tests/foo_test.cpp".to_string();
        narrated.output_tokens = 42;
        assert!(is_errored_actionless_completion(&narrated));
        assert!(!is_zero_token_empty_completion(&narrated));
        assert!(is_retryable_unproductive_completion(&narrated));

        // Case-insensitive on the stop reason.
        let mut upper = narrated.clone();
        upper.stop_reason = Some("ERROR".to_string());
        assert!(is_errored_actionless_completion(&upper));

        // A clean finish is not the errored-actionless case (it may still be a
        // genuine answer or a billed-noncommittal parse error — not ours).
        let mut clean = narrated.clone();
        clean.stop_reason = Some("stop".to_string());
        assert!(!is_errored_actionless_completion(&clean));

        // An errored turn that STILL dispatched a tool call has real work — the
        // loop must not treat it as actionless.
        let mut errored_with_call = narrated.clone();
        errored_with_call.tool_calls = vec![serde_json::json!({"id": "t1", "name": "edit"})];
        assert!(!is_errored_actionless_completion(&errored_with_call));

        // An errored turn whose only activity was a server-side tool search is
        // not actionless either (no dispatchable call, but the search counts).
        let mut errored_with_search = narrated;
        errored_with_search.blocks = vec![serde_json::json!({"type": "tool_search_query"})];
        assert!(errored_with_search.tool_calls.is_empty());
        assert!(!is_errored_actionless_completion(&errored_with_search));

        // The zero-token empty completion still flows through the unified
        // predicate.
        assert!(is_retryable_unproductive_completion(&empty_result()));
    }
}

#[cfg(test)]
mod streaming_detector_tests {
    //! Verify the streaming candidate detector glue (harn#692). The
    //! unit tests in `crate::llm::tools::parse::streaming` already cover
    //! the detector's state machine; these tests cover the loop body
    //! that pumps deltas through the detector and dispatches each event
    //! to a sink. Uses `run_detector_loop_with_sink` with a captured
    //! buffer so the test doesn't depend on the global session sink
    //! registry — other tests in this binary mutate the registry via
    //! `reset_all_sinks` and can race a per-session install otherwise.
    use std::cell::RefCell;
    use std::rc::Rc;

    use crate::agent_events::{AgentEvent, ToolCallStatus};

    use super::{run_detector_loop_with_sink, StreamingDetectorContext};

    /// Pipe `chunks` through `run_detector_loop_with_sink`, await its
    /// completion, and return the captured events in arrival order.
    async fn drive(session_id: &str, known: &[&str], chunks: &[&str]) -> Vec<AgentEvent> {
        let captured: Rc<RefCell<Vec<AgentEvent>>> = Rc::new(RefCell::new(Vec::new()));
        let sink_buf = captured.clone();
        let known_tools = known.iter().map(|s| (*s).to_string()).collect();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        for chunk in chunks {
            tx.send((*chunk).to_string()).expect("send delta");
        }
        drop(tx);
        run_detector_loop_with_sink(
            StreamingDetectorContext {
                session_id: session_id.to_string(),
                known_tools,
            },
            rx,
            move |event| sink_buf.borrow_mut().push(event.clone()),
        )
        .await;
        let events = captured.borrow().clone();
        events
    }

    #[tokio::test(flavor = "current_thread")]
    async fn detector_loop_emits_start_and_promoted_through_sink() {
        let events = drive(
            "session-stream-promote",
            &["read"],
            &["read({ path: \"a.md\" })"],
        )
        .await;
        assert_eq!(
            events.len(),
            2,
            "expected start + promoted, got: {events:#?}"
        );
        match &events[0] {
            AgentEvent::ToolCall {
                parsing,
                tool_name,
                status,
                ..
            } => {
                assert_eq!(*parsing, Some(true));
                assert_eq!(tool_name, "read");
                assert_eq!(*status, ToolCallStatus::Pending);
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
        match &events[1] {
            AgentEvent::ToolCallUpdate {
                parsing,
                status,
                error_category,
                raw_input,
                raw_output,
                ..
            } => {
                assert_eq!(*parsing, Some(false));
                assert_eq!(*status, ToolCallStatus::Pending);
                assert!(error_category.is_none());
                assert_eq!(
                    raw_input.as_ref(),
                    Some(&serde_json::json!({"path": "a.md"}))
                );
                assert!(raw_output.is_none(), "promoted args belong in raw_input");
            }
            other => panic!("expected ToolCallUpdate, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn detector_loop_finalizes_unclosed_tagged_block_as_aborted() {
        let events = drive(
            "session-stream-abort",
            &["run"],
            &["<tool_call>\nrun({ command: \"ls\""],
        )
        .await;
        assert_eq!(events.len(), 2, "events={events:#?}");
        match &events[1] {
            AgentEvent::ToolCallUpdate {
                status,
                error_category,
                parsing,
                ..
            } => {
                assert_eq!(*status, ToolCallStatus::Failed);
                assert_eq!(
                    *error_category,
                    Some(crate::agent_events::ToolCallErrorCategory::ParseAborted)
                );
                assert_eq!(*parsing, Some(false));
            }
            other => panic!("expected ToolCallUpdate, got {other:?}"),
        }
    }
}
