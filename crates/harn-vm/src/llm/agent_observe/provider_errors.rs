use super::*;

pub(super) fn governor_throttle_signal_for_error(
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
pub(super) fn governor_estimated_tokens(opts: &super::api::LlmCallOptions) -> u64 {
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
pub(super) async fn await_governor_admission(
    provider: &str,
    org_key: &str,
    est_tokens: u64,
) -> bool {
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
    let reserved = matches!(gate(provider, org_key, est_tokens), GateOutcome::Proceed);
    if !reserved {
        // Falling through unreserved abandons the back-pressure guarantee the
        // rest of the system assumes is in force, and it used to do so with no
        // event, no record, and no log — so the 429 storm that follows reads as
        // a provider problem rather than as the governor having given up
        // (harn#5142).
        crate::boundary::BoundaryFailure::new(
            crate::boundary::BoundaryId::ProviderAdmissionGate,
            crate::boundary::BoundaryFailureKind::Capped,
            format!(
                "rate governor circuit for `{provider}` stayed open through \
                 {GOVERNOR_MAX_ADMISSION_WAITS} admission waits; the call proceeds unreserved"
            ),
        )
        .report();
    }
    reserved
}

/// Release the governor slot reserved by [`await_governor_admission`] and record
/// the call's outcome (AIMD + circuit + L0 `provider_throttle` emission). No-op
/// when the flag is off. Runs exactly once per gated attempt.
pub(super) fn record_governor_call_outcome(
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
            // Empty-under-load: committed nothing WHILE this provider is
            // already throttled → soft-throttle, not capability. A zero-token
            // stall under an OPEN/HALF-OPEN circuit is just as much load
            // evidence as a billed empty response; lone empties on healthy
            // providers still classify as served/no-signal here and are owned
            // by the bounded empty-completion retry path.
            let committed_nothing = result.committed_nothing_usable();
            if let Some(signal) = ThrottleSignal::classify(
                None,
                "",
                committed_nothing,
                rate_governor::provider_already_throttled(provider, org_key),
            ) {
                (
                    GovernorOutcome::Throttled {
                        signal,
                        retry_after_ms: None,
                    },
                    Some((signal, None)),
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
pub(super) fn emit_provider_throttle(
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
#[cfg(test)]
pub(super) fn is_empty_completion_retry_error(err: &VmError) -> bool {
    empty_completion_retry_reason(err).is_some()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum UnproductiveCompletionReason {
    EmptyGeneration,
    UnproductiveCompletion,
}

impl UnproductiveCompletionReason {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::EmptyGeneration => "empty_generation",
            Self::UnproductiveCompletion => "unproductive_completion",
        }
    }
}

pub(super) fn empty_completion_retry_reason(err: &VmError) -> Option<UnproductiveCompletionReason> {
    if let VmError::Thrown(crate::value::VmValue::Dict(fields)) = err {
        return match fields
            .get("code")
            .map(crate::value::VmValue::display)
            .as_deref()
        {
            Some("empty_generation") => Some(UnproductiveCompletionReason::EmptyGeneration),
            Some("unproductive_completion") => {
                Some(UnproductiveCompletionReason::UnproductiveCompletion)
            }
            _ => None,
        };
    }
    let msg = match err {
        VmError::Thrown(crate::value::VmValue::String(s)) => s.as_ref(),
        VmError::CategorizedError { message, .. } => message.as_str(),
        VmError::Runtime(s) => s.as_str(),
        _ => return None,
    };
    let lower = msg.to_lowercase();
    if !lower.contains("completion_tokens=") {
        return None;
    }
    // (1) zero-token empty completion, or (2) billed-noncommittal completion.
    if lower.contains("delivered no content") {
        return Some(UnproductiveCompletionReason::EmptyGeneration);
    }
    if lower.contains("no dispatchable tool call or answer")
        && lower.contains("upstream contract violation")
    {
        return Some(UnproductiveCompletionReason::UnproductiveCompletion);
    }
    None
}

/// Output tokens the provider billed for a thrown empty completion, when it
/// reported them. `empty_generation_error` carries this field on every throw
/// that reached a usage block.
pub(super) fn thrown_output_tokens(err: &VmError) -> Option<i64> {
    if let VmError::Thrown(crate::value::VmValue::Dict(fields)) = err {
        if let Some(crate::value::VmValue::Int(tokens)) = fields.get("output_tokens") {
            return Some(*tokens);
        }
    }
    // The same failure reaches some boundaries flattened to a message, which
    // names the count too. `empty_completion_retry_reason` already reads both
    // shapes; the budget comparison it feeds has to read both as well, or a
    // flattened throw silently loses the fact that decides the retry.
    let message = match err {
        VmError::Thrown(crate::value::VmValue::String(text)) => text.as_ref(),
        VmError::CategorizedError { message, .. } => message.as_str(),
        VmError::Runtime(text) => text.as_str(),
        _ => return None,
    };
    let digits: String = message
        .split("completion_tokens=")
        .nth(1)?
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

/// An empty completion that billed the whole output budget is a *deterministic*
/// budget failure, not a provider hiccup. The reasoning channel consumed every
/// token the cap allowed and the committed message came back empty; the same
/// context under the same cap exhausts the same way, so a byte-identical replay
/// spends a second call to learn what the first one already proved. Recovery is
/// a larger cap or a smaller request.
///
/// The cap is the request's, so this predicate lives at the retry boundary —
/// the response parser sees usage but never the cap it was measured against.
pub(super) fn is_output_budget_exhausted(err: &VmError, max_tokens: i64) -> bool {
    max_tokens > 0
        && matches!(
            empty_completion_retry_reason(err),
            Some(UnproductiveCompletionReason::EmptyGeneration)
        )
        && thrown_output_tokens(err).is_some_and(|tokens| tokens >= max_tokens)
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
///    canonical cheap-model "vanishing call" signature, and the error message
///    itself prescribes a "Harn text/json tool format".
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
pub(super) fn is_billed_noncommittal_throw(err: &VmError) -> bool {
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
pub(super) fn message_is_billed_noncommittal_throw(msg: &str) -> bool {
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
pub(super) fn is_native_tool_channel_failure(err: &VmError) -> bool {
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
pub(super) fn message_is_native_tool_channel_failure(msg: &str) -> bool {
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
pub(super) fn is_stream_transport_failure(err: &VmError) -> bool {
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

pub(super) fn message_is_stream_transport_failure(msg: &str) -> bool {
    let lower = msg.to_lowercase();
    lower.contains("stream error")
        && (lower.contains("mid-stream")
            || lower.contains("response body")
            || lower.contains("body")
            || lower.contains("error decoding stream")
            || lower.contains("connection reset"))
}

pub(super) fn can_degrade_stream_transport(opts: &super::api::LlmCallOptions) -> bool {
    opts.stream
        && !crate::llm::managed_supply::capabilities_for(&opts.provider, &opts.model)
            .requires_streaming
        && !crate::llm::provider::provider_uses_ollama_messages(&opts.provider, &opts.model)
}

/// A wire-level "success" that carries nothing the agent loop can act on: no
/// visible text (whitespace-only counts as empty), no thinking, no tool calls,
/// and no server-side tool-search activity. Covers both the zero-token provider
/// stall (an empty 200 — observed live on OpenRouter) and a whitespace-only or
/// echoed-stop-sequence completion that billed tokens but committed nothing
/// usable (harn#4744): keying on trimmed content instead of `output_tokens == 0`
/// is what closes that hole. Either way the loop would burn an iteration
/// recovering from an empty assistant turn, so it is treated as a transient
/// provider hiccup and retried in [`observed_llm_call`].
///
/// Token-cap truncations (`stop_reason` length/max_tokens) are excluded — a
/// deterministic cap would just re-truncate on every retry, mirroring the
/// `done_reason == "length"` carve-out on the Ollama NDJSON path.
pub(super) fn is_empty_unproductive_completion(result: &super::api::LlmResult) -> bool {
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
    result.committed_nothing_usable() && !truncated && !has_tool_search_block
}

/// A wire-level "success" whose `stop_reason` reports a provider *error* yet
/// carries no dispatchable action. Observed live (cheap-model eval meter):
/// a generation comes back with `stop_reason == "error"` after only narrating
/// an intended tool call in its text/thinking ("We need to make edit to create
/// tests/...test.cpp...") but with ZERO parsed tool calls. Unlike
/// [`is_empty_unproductive_completion`], the reasoning channel is non-empty, so
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
pub(super) fn is_errored_actionless_completion(result: &super::api::LlmResult) -> bool {
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
pub(super) fn is_retryable_unproductive_completion(result: &super::api::LlmResult) -> bool {
    is_empty_unproductive_completion(result) || is_errored_actionless_completion(result)
}

/// The crate-internal LLM *simulators* — `fake` (scripted streams) and `mock`
/// (replayed turns) — are test/replay routes, not real provider endpoints. They
/// are already excluded from the empty-completion retry budget
/// ([`empty_completion_retry_budget`]) and backoff ([`llm_retry_backoff_ms`]);
/// exclude them from terminal empty-generation recovery for the same reason. A
/// scripted empty turn is a fixture, not a dead provider lane.
pub(super) fn terminal_unproductive_completion_failover_error(
    opts: &super::api::LlmCallOptions,
    result: &super::api::LlmResult,
    provider_under_throttle: bool,
    attempt_count: usize,
    duration_ms: Option<u64>,
) -> Option<VmError> {
    if crate::llm::providers::is_internal_simulator(&opts.provider)
        || !is_retryable_unproductive_completion(result)
    {
        return None;
    }

    if is_empty_unproductive_completion(result) {
        let detail = format!(
            "returned completion_tokens={} and delivered no content, thinking, or tool calls",
            result.output_tokens
        );
        return Some(provider_exhausted_error(
            opts,
            UnproductiveCompletionReason::EmptyGeneration,
            attempt_count,
            duration_ms,
            format!(
                "provider {} model {} exhausted empty-completion retry budget: reason=empty_generation attempt_count={attempt_count}; {detail}",
                opts.provider, opts.model
            ),
        ));
    }
    if !provider_under_throttle {
        return None;
    }

    let detail = format!(
        "ended with stop_reason={} after completion_tokens={} and delivered no dispatchable tool call",
        result.stop_reason.as_deref().unwrap_or("unknown"),
        result.output_tokens
    );
    Some(provider_exhausted_error(
        opts,
        UnproductiveCompletionReason::UnproductiveCompletion,
        attempt_count,
        duration_ms,
        format!(
            "provider {} model {} exhausted unproductive-completion retry budget while rate governor circuit_open/under throttle: reason=unproductive_completion attempt_count={attempt_count}; {detail}",
            opts.provider, opts.model,
        ),
    ))
}

pub(super) fn terminal_unproductive_completion_failure(
    opts: &super::api::LlmCallOptions,
    result: &super::api::LlmResult,
    provider_under_throttle: bool,
    attempt_count: usize,
    duration_ms: u64,
) -> Option<VmError> {
    let error = terminal_unproductive_completion_failover_error(
        opts,
        result,
        provider_under_throttle,
        attempt_count,
        Some(duration_ms),
    )?;
    let reason = if is_empty_unproductive_completion(result) {
        UnproductiveCompletionReason::EmptyGeneration
    } else {
        UnproductiveCompletionReason::UnproductiveCompletion
    };
    super::rate_limit::observe_unproductive_completion_for_llm_call(opts, reason.as_str());
    Some(error)
}

pub(super) fn provider_exhausted_error(
    opts: &super::api::LlmCallOptions,
    reason: UnproductiveCompletionReason,
    attempt_count: usize,
    duration_ms: Option<u64>,
    message: String,
) -> VmError {
    let mut attempt = std::collections::BTreeMap::from([
        (
            "provider".to_string(),
            VmValue::String(arcstr::ArcStr::from(opts.provider.clone())),
        ),
        (
            "model".to_string(),
            VmValue::String(arcstr::ArcStr::from(opts.model.clone())),
        ),
        (
            "attempt_count".to_string(),
            VmValue::Int(attempt_count as i64),
        ),
        (
            "reason".to_string(),
            VmValue::String(arcstr::ArcStr::from(reason.as_str())),
        ),
    ]);
    if let Some(duration_ms) = duration_ms {
        attempt.insert("duration_ms".to_string(), VmValue::Int(duration_ms as i64));
    }
    super::routing::provider_exhausted_error(
        "circuit_open",
        reason.as_str(),
        attempt_count,
        message,
        VmValue::List(std::sync::Arc::new(vec![VmValue::dict(attempt)])),
    )
}

pub(super) fn emit_empty_completion_retry(
    iteration: usize,
    attempt: usize,
    opts: &super::api::LlmCallOptions,
    reason: UnproductiveCompletionReason,
    duration_ms: u64,
    error: &str,
    usage: Option<&crate::llm::usage::LlmUsage>,
) {
    let mut fields = serde_json::Map::from_iter([
        (
            "schema".to_string(),
            serde_json::json!("harn.llm.empty_completion_retry.v1"),
        ),
        (
            "receipt_kind".to_string(),
            serde_json::json!("empty_completion_retry"),
        ),
        ("iteration".to_string(), serde_json::json!(iteration)),
        ("attempt".to_string(), serde_json::json!(attempt)),
        ("provider".to_string(), serde_json::json!(opts.provider)),
        ("model".to_string(), serde_json::json!(opts.model)),
        ("reason".to_string(), serde_json::json!(reason.as_str())),
        ("duration_ms".to_string(), serde_json::json!(duration_ms)),
        ("error".to_string(), serde_json::json!(error)),
    ]);
    if let Some(usage) = usage {
        usage.project_onto_fields(&mut fields);
    } else {
        // A thrown completion has no trustworthy token ledger. It may still
        // have reached a billable provider boundary, so preserve an explicit
        // unknown transaction instead of reporting a free retry.
        crate::llm::usage::LlmUsage::unknown_attempt().project_onto_fields(&mut fields);
    }
    append_llm_observability_entry("empty_completion_retry", fields);
    super::trace::emit_agent_event(super::trace::AgentTraceEvent::EmptyCompletionRetry {
        iteration,
        attempt,
        provider: opts.provider.clone(),
        model: opts.model.clone(),
        reason: reason.as_str().to_string(),
        duration_ms,
        error: error.to_string(),
    });
}

pub(super) struct ProviderCallErrorObservation<'a> {
    pub(super) iteration: usize,
    pub(super) call_id: &'a str,
    /// One-based number of the provider attempt that produced this error.
    pub(super) attempt: usize,
    pub(super) status: &'a str,
    pub(super) opts: &'a super::api::LlmCallOptions,
    pub(super) category: &'a crate::value::ErrorCategory,
    pub(super) classified: &'a super::api::LlmErrorInfo,
    pub(super) message: &'a str,
    pub(super) stream_failure: Option<&'a crate::value::ProviderStreamFailure>,
    /// Usage from a completed response that the caller must still surface as
    /// terminally unusable. Transport and parser failures pass `None`.
    pub(super) usage: Option<&'a crate::llm::usage::LlmUsage>,
    pub(super) retryable: bool,
    pub(super) failover_eligible: bool,
    pub(super) attempt_count: Option<usize>,
}

pub(super) fn append_provider_call_error_observability(
    observation: ProviderCallErrorObservation<'_>,
) {
    let ProviderCallErrorObservation {
        iteration,
        call_id,
        attempt,
        status,
        opts,
        category,
        classified,
        message,
        stream_failure,
        usage,
        retryable,
        failover_eligible,
        attempt_count,
    } = observation;
    let mut fields = serde_json::Map::from_iter([
        ("iteration".to_string(), serde_json::json!(iteration)),
        ("call_id".to_string(), serde_json::json!(call_id)),
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
        ("message".to_string(), serde_json::json!(message)),
        ("retryable".to_string(), serde_json::json!(retryable)),
    ]);
    if let Some(usage) = usage {
        usage.project_onto_fields(&mut fields);
    } else {
        // A thrown completion may have reached a billable provider boundary
        // without yielding a trustworthy ledger. Preserve that uncertainty
        // explicitly instead of presenting a free retry.
        crate::llm::usage::LlmUsage::unknown_attempt().project_onto_fields(&mut fields);
    }
    if let Some(stage) = opts
        .call_stage
        .as_deref()
        .map(str::trim)
        .filter(|stage| !stage.is_empty())
    {
        fields.insert("stage".to_string(), serde_json::json!(stage));
    }
    if failover_eligible {
        fields.insert(
            "failover_eligible".to_string(),
            serde_json::json!(failover_eligible),
        );
    }
    if let Some(attempt_count) = attempt_count {
        fields.insert(
            "attempt_count".to_string(),
            serde_json::json!(attempt_count),
        );
    }
    if let Some(failure) = stream_failure {
        fields.insert("source".to_string(), serde_json::json!("provider_stream"));
        fields.insert(
            "phase".to_string(),
            serde_json::json!(failure.phase.as_str()),
        );
        fields.insert(
            "deadline".to_string(),
            failure
                .deadline
                .map(|deadline| serde_json::json!(deadline.as_str()))
                .unwrap_or(serde_json::Value::Null),
        );
        fields.insert("partial".to_string(), serde_json::json!(failure.partial));
    }
    append_llm_observability_entry("provider_call_error", fields);
}

#[cfg(test)]
mod stream_failure_observability_tests {
    use super::*;

    #[test]
    fn provider_error_receipt_projects_stream_phase_and_deadline() {
        let transcript_dir = tempfile::tempdir().expect("transcript tempdir");
        push_llm_transcript_dir(transcript_dir.path().to_str().expect("utf8 tempdir"));
        let opts = crate::llm::api::options::base_opts("openai");
        let category = crate::value::ErrorCategory::Timeout;
        let classified =
            crate::llm::api::classify_llm_error(category.clone(), "idle deadline elapsed");
        let failure = crate::value::ProviderStreamFailure {
            provider: "openai".to_string(),
            phase: crate::value::ProviderStreamPhase::Streaming,
            reason: crate::value::ProviderStreamFailureReason::Deadline,
            deadline: Some(crate::value::ProviderStreamDeadline::Idle),
            partial: true,
            detail: "idle deadline elapsed".to_string(),
        };

        append_provider_call_error_observability(ProviderCallErrorObservation {
            iteration: 1,
            call_id: "call-stream-timeout",
            attempt: 1,
            status: "error",
            opts: &opts,
            category: &category,
            classified: &classified,
            message: "idle deadline elapsed",
            stream_failure: Some(&failure),
            usage: None,
            retryable: true,
            failover_eligible: false,
            attempt_count: None,
        });
        pop_llm_transcript_dir();

        let receipt: serde_json::Value = serde_json::from_str(
            std::fs::read_to_string(transcript_dir.path().join("llm_transcript.jsonl"))
                .expect("provider error receipt")
                .trim(),
        )
        .expect("valid provider error receipt");
        assert_eq!(receipt["type"], "provider_call_error");
        assert_eq!(receipt["source"], "provider_stream");
        assert_eq!(receipt["phase"], "streaming");
        assert_eq!(receipt["deadline"], "idle");
        assert_eq!(receipt["partial"], true);
    }
}
