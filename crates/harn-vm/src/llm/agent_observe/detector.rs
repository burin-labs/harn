use super::*;

pub(super) fn tee_delta_sender(mut senders: Vec<DeltaSender>) -> DeltaSender {
    if senders.len() == 1 {
        return senders.pop().expect("len checked above");
    }

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    tokio::task::spawn_local(async move {
        while let Some(delta) = rx.recv().await {
            for sender in &senders {
                let _ = sender.send(delta.clone());
            }
        }
    });
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
pub(super) async fn run_detector_loop_with_sink(
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
pub(super) async fn run_detector_loop(
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

/// Base exponential backoff (ms) for the built-in provider-hiccup retries
/// below (empty completions, tool-channel / stream-transport degrades).
///
/// A raw `llm_call` is fail-fast on transient errors (documented contract; see
/// the quickref). The pre-v0.10 `llm_retries` / `llm_backoff_ms` options were
/// removed; transient-retry policy is composed in Harn via
/// `with_retry(default_llm_caller(), {...})` from `std/llm/handlers` (note the
/// off-by-one: `llm_retries: K` retried K times after the first attempt, so it
/// maps to `with_retry(..., {max_attempts: K + 1})`).
pub(crate) const DEFAULT_LLM_CALL_BACKOFF_MS: u64 = 250;

/// Built-in retry budget for zero-token empty completions. Applies even when
/// the caller's transient-retry budget is 0 (the fail-fast `llm_call`
/// default), mirroring the transport's unconditional single retry for the
/// Ollama empty-content parser bug: an empty 200 is clearly a provider
/// hiccup, and most live callers (e.g. a host agent loop) retry only on
/// *errors*, so an empty Ok would otherwise sail through untouched.
pub(super) const EMPTY_COMPLETION_BUILTIN_RETRIES: usize = 1;
