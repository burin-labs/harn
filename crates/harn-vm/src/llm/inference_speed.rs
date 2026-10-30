//! Per-call inference speed: time to first token and output tokens per second.
//!
//! One meter is attached to each provider attempt at the dispatch boundary, so
//! every provider adapter is measured the same way. Adapters report what the
//! model produced through [`observe_output`] (reasoning and tool-call
//! arguments) and the meter's own tee on the text channel; nothing else needs
//! to know how a rate is computed.
//!
//! Live snapshots are estimates: providers report usage only when a stream
//! ends. Each estimate is calibrated against the exact counts earlier calls on
//! the same route reported, and the final snapshot replaces it with the
//! provider's or server's own numbers. Field meanings follow the OpenTelemetry
//! GenAI client metrics: `time_to_first_token_ms` corresponds to
//! `gen_ai.client.inference.time_to_first_chunk`, and the rate is the inverse
//! of `gen_ai.client.inference.time_per_output_chunk` measured in tokens.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::time::Instant;

use super::api::{DeltaSender, LlmResult};
use crate::agent_events::{self, transport::WeakAgentEventTransport, AgentEvent};

/// Minimum spacing between live snapshots. A phase change publishes at once.
const SNAPSHOT_INTERVAL: Duration = Duration::from_millis(250);
/// Sliding window the live rate is measured over.
const RATE_WINDOW: Duration = Duration::from_secs(2);
/// Shortest span a rate may be computed over; shorter spans are dominated by
/// chunk batching rather than decode speed.
const MIN_RATE_SPAN: Duration = Duration::from_millis(500);
/// Fewest output tokens a final rate may be computed from. Shorter
/// completions produce division artifacts rather than measurements.
const MIN_FINAL_RATE_TOKENS: u64 = 20;

/// What the model is doing at the moment of a snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InferencePhase {
    AwaitingFirstToken,
    Reasoning,
    Responding,
    CallingTool,
    Complete,
}

/// Where a snapshot's token count came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenCountSource {
    /// Estimated from streamed text by the route's tokenizer, calibrated
    /// against earlier exact counts.
    Estimated,
    /// The provider's usage receipt.
    Provider,
    /// The inference server's own generation timings (llama.cpp, Ollama).
    Server,
}

/// One snapshot of a call's speed. Absent fields were not measured; a
/// consumer must not read them as zero.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InferenceSpeed {
    pub phase: InferencePhase,
    /// The call ended and these are its settled numbers.
    pub is_final: bool,
    /// Time since the request was dispatched.
    pub elapsed_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_to_first_token_ms: Option<u64>,
    /// Output tokens so far, reasoning and tool-call arguments included.
    pub output_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens_per_second: Option<f64>,
    pub token_source: TokenCountSource,
}

/// The kind of output an adapter observed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OutputKind {
    Text,
    Reasoning,
    ToolArguments,
}

impl OutputKind {
    fn phase(self) -> InferencePhase {
        match self {
            Self::Text => InferencePhase::Responding,
            Self::Reasoning => InferencePhase::Reasoning,
            Self::ToolArguments => InferencePhase::CallingTool,
        }
    }
}

tokio::task_local! {
    static CURRENT_METER: InferenceMeter;
}

/// Run `future` with `meter` receiving [`observe_output`] reports.
pub(crate) async fn scope<F: std::future::Future>(meter: InferenceMeter, future: F) -> F::Output {
    CURRENT_METER.scope(meter, future).await
}

/// Report output the model produced on a channel other than visible text.
/// A no-op outside a metered provider attempt.
pub(crate) fn observe_output(kind: OutputKind, text: &str) {
    if text.is_empty() {
        return;
    }
    let _ = CURRENT_METER.try_with(|meter| meter.observe(kind, text));
}

struct Identity {
    session_id: Option<String>,
    call_id: String,
    provider: String,
    model: String,
}

struct State {
    origin: Instant,
    first_output: Option<Instant>,
    last_output: Option<Instant>,
    phase: InferencePhase,
    pending: String,
    raw_estimate: f64,
    calibration: f64,
    window: VecDeque<(Instant, f64)>,
    last_snapshot: Option<Instant>,
    // Bound weakly at dispatch: the text tee runs outside the prompt's
    // per-poll transport scope, and must never keep its output open.
    transport: WeakAgentEventTransport,
    finished: bool,
}

/// The speed meter for one provider attempt. Cheap to clone; clones share
/// state.
#[derive(Clone)]
pub(crate) struct InferenceMeter {
    identity: Arc<Identity>,
    state: Arc<Mutex<State>>,
}

impl InferenceMeter {
    pub(crate) fn new(
        session_id: Option<String>,
        call_id: &str,
        provider: &str,
        model: &str,
    ) -> Self {
        let calibration = route_calibration(provider, model);
        Self {
            identity: Arc::new(Identity {
                session_id,
                call_id: call_id.to_string(),
                provider: provider.to_string(),
                model: model.to_string(),
            }),
            state: Arc::new(Mutex::new(State {
                origin: Instant::now(),
                first_output: None,
                last_output: None,
                phase: InferencePhase::AwaitingFirstToken,
                pending: String::new(),
                raw_estimate: 0.0,
                calibration,
                window: VecDeque::new(),
                last_snapshot: None,
                transport: WeakAgentEventTransport::default(),
                finished: false,
            })),
        }
    }

    /// Restart the clock at the moment the request is dispatched, and tell
    /// the host the call is waiting for its first token.
    pub(crate) fn start(&self) {
        let snapshot = {
            let mut state = self.lock();
            state.origin = Instant::now();
            state.last_snapshot = Some(state.origin);
            state.transport = agent_events::transport::current().downgrade();
            snapshot_of(&state, state.origin, false)
        };
        self.publish(snapshot);
    }

    /// Wrap the text channel so visible text is metered on its way through.
    /// Await the returned handle after the provider returns so every chunk is
    /// counted before [`Self::finish`].
    pub(crate) fn tee_text(
        &self,
        downstream: DeltaSender,
    ) -> (DeltaSender, tokio::task::JoinHandle<()>) {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let meter = self.clone();
        let handle = crate::task_spawn::spawn(async move {
            while let Some(delta) = rx.recv().await {
                meter.observe(OutputKind::Text, &delta);
                let _ = downstream.send(delta);
            }
        });
        (tx, handle)
    }

    pub(crate) fn observe(&self, kind: OutputKind, text: &str) {
        if text.is_empty() {
            return;
        }
        let now = Instant::now();
        let snapshot = {
            let mut state = self.lock();
            if state.finished {
                return;
            }
            state.first_output.get_or_insert(now);
            state.last_output = Some(now);
            state.pending.push_str(text);
            let phase = kind.phase();
            let phase_changed = state.phase != phase;
            state.phase = phase;
            let due = state
                .last_snapshot
                .is_none_or(|at| now.duration_since(at) >= SNAPSHOT_INTERVAL);
            if !(phase_changed || due) {
                return;
            }
            self.flush_pending(&mut state, now);
            state.last_snapshot = Some(now);
            snapshot_of(&state, now, false)
        };
        self.publish(snapshot);
    }

    /// Settle the call: exact counts replace the estimate, the final snapshot
    /// is published, and it is recorded on the result's telemetry.
    pub(crate) fn finish(&self, result: &mut LlmResult) {
        let now = Instant::now();
        let snapshot = {
            let mut state = self.lock();
            if state.finished {
                return;
            }
            self.flush_pending(&mut state, now);
            state.finished = true;
            let estimated = state.raw_estimate * state.calibration;
            let exact = u64::try_from(result.output_tokens)
                .ok()
                .filter(|tokens| *tokens > 0);
            if let Some(exact) = exact {
                record_route_calibration(
                    &self.identity.provider,
                    &self.identity.model,
                    exact as f64,
                    state.raw_estimate,
                );
            }
            let telemetry = &result.telemetry;
            let server_rate = telemetry
                .server_output_tokens
                .zip(telemetry.server_generation_ms)
                .and_then(|(tokens, ms)| rate(u64::try_from(tokens).ok()?, ms));
            let estimated_tokens = estimated.round() as u64;
            let (output_tokens, token_source) = match exact {
                Some(tokens) => (tokens, TokenCountSource::Provider),
                None => (estimated_tokens, TokenCountSource::Estimated),
            };
            // Reasoning a provider never streams is billed as output but was
            // produced before the first visible chunk, so it cannot be timed
            // against the streamed span.
            let streamed_tokens = match exact {
                Some(tokens) if is_tokenizer_drift(tokens as f64, state.raw_estimate) => tokens,
                _ => estimated_tokens,
            };
            let (output_tokens_per_second, token_source) = match server_rate {
                Some(server_rate) => (Some(server_rate), TokenCountSource::Server),
                None => (decode_rate(&state, streamed_tokens), token_source),
            };
            InferenceSpeed {
                phase: InferencePhase::Complete,
                is_final: true,
                elapsed_ms: millis(now.duration_since(state.origin)),
                time_to_first_token_ms: state
                    .first_output
                    .map(|at| millis(at.duration_since(state.origin))),
                output_tokens,
                output_tokens_per_second,
                token_source,
            }
        };
        result.telemetry.inference_speed = Some(Box::new(snapshot.clone()));
        self.publish(snapshot);
    }

    /// Settle a call that failed: the host stops showing a live rate.
    pub(crate) fn abandon(&self) {
        let now = Instant::now();
        let snapshot = {
            let mut state = self.lock();
            if state.finished {
                return;
            }
            self.flush_pending(&mut state, now);
            state.finished = true;
            state.phase = InferencePhase::Complete;
            let mut snapshot = snapshot_of(&state, now, true);
            snapshot.output_tokens_per_second = None;
            snapshot
        };
        self.publish(snapshot);
    }

    #[cfg(test)]
    fn snapshot(&self) -> InferenceSpeed {
        let now = Instant::now();
        let mut state = self.lock();
        self.flush_pending(&mut state, now);
        snapshot_of(&state, now, false)
    }

    fn flush_pending(&self, state: &mut State, now: Instant) {
        if !state.pending.is_empty() {
            let pending = std::mem::take(&mut state.pending);
            state.raw_estimate +=
                harn_llm::token_count::estimate_text_tokens(&pending, Some(&self.identity.model))
                    .tokens as f64;
        }
        let tokens = state.raw_estimate * state.calibration;
        state.window.push_back((now, tokens));
        while state.window.len() > 2
            && state
                .window
                .get(1)
                .is_some_and(|(at, _)| now.duration_since(*at) >= RATE_WINDOW)
        {
            state.window.pop_front();
        }
    }

    fn publish(&self, speed: InferenceSpeed) {
        let Some(session_id) = self.identity.session_id.clone() else {
            return;
        };
        let event = AgentEvent::InferenceSpeed {
            session_id,
            call_id: self.identity.call_id.clone(),
            provider: self.identity.provider.clone(),
            model: self.identity.model.clone(),
            speed,
        };
        let transport = self.lock().transport.upgrade();
        transport.with(|| agent_events::emit_event(&event));
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn snapshot_of(state: &State, now: Instant, is_final: bool) -> InferenceSpeed {
    InferenceSpeed {
        phase: state.phase,
        is_final,
        elapsed_ms: millis(now.duration_since(state.origin)),
        time_to_first_token_ms: state
            .first_output
            .map(|at| millis(at.duration_since(state.origin))),
        output_tokens: (state.raw_estimate * state.calibration).round() as u64,
        output_tokens_per_second: live_rate(state),
        token_source: TokenCountSource::Estimated,
    }
}

/// Rate over the sliding window, or over the whole stream while the window is
/// still shorter than [`MIN_RATE_SPAN`].
fn live_rate(state: &State) -> Option<f64> {
    let (newest_at, newest) = *state.window.back()?;
    let (oldest_at, oldest) = *state.window.front()?;
    let span = newest_at.duration_since(oldest_at);
    if span >= MIN_RATE_SPAN {
        return per_second(newest - oldest, span);
    }
    let first = state.first_output?;
    let span = newest_at.duration_since(first);
    (span >= MIN_RATE_SPAN)
        .then(|| per_second(newest, span))
        .flatten()
}

/// Settled rate: tokens after the first chunk over the time from the first
/// chunk to the last. A stream that arrived in one burst has no measurable
/// decode span and gets no rate.
fn decode_rate(state: &State, output_tokens: u64) -> Option<f64> {
    if output_tokens < MIN_FINAL_RATE_TOKENS {
        return None;
    }
    let span = state.last_output?.duration_since(state.first_output?);
    if span < MIN_RATE_SPAN {
        return None;
    }
    per_second(output_tokens.saturating_sub(1) as f64, span)
}

fn rate(tokens: u64, ms: u64) -> Option<f64> {
    if tokens < MIN_FINAL_RATE_TOKENS || ms == 0 {
        return None;
    }
    per_second(tokens as f64, Duration::from_millis(ms))
}

fn per_second(tokens: f64, span: Duration) -> Option<f64> {
    let seconds = span.as_secs_f64();
    (tokens > 0.0 && seconds > 0.0).then(|| (tokens / seconds * 10.0).round() / 10.0)
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// Exact-to-estimated output ratio per route, learned from settled calls so
/// live estimates converge on the provider's own tokenizer.
fn calibrations() -> &'static Mutex<HashMap<(String, String), f64>> {
    static CALIBRATIONS: OnceLock<Mutex<HashMap<(String, String), f64>>> = OnceLock::new();
    CALIBRATIONS.get_or_init(Default::default)
}

fn route_calibration(provider: &str, model: &str) -> f64 {
    calibrations()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&(provider.to_string(), model.to_string()))
        .copied()
        .unwrap_or(1.0)
}

/// Whether `exact` differs from the uncalibrated estimate only by the kind of
/// gap two tokenizers produce on the same text. A larger gap means output the
/// stream never showed, such as unstreamed reasoning.
fn is_tokenizer_drift(exact: f64, raw_estimate: f64) -> bool {
    raw_estimate >= MIN_FINAL_RATE_TOKENS as f64
        && (TOKENIZER_DRIFT.0..=TOKENIZER_DRIFT.1).contains(&(exact / raw_estimate))
}

/// The exact-to-estimated ratios a tokenizer mismatch alone can explain.
const TOKENIZER_DRIFT: (f64, f64) = (0.6, 1.6);

fn record_route_calibration(provider: &str, model: &str, exact: f64, raw_estimate: f64) {
    if !is_tokenizer_drift(exact, raw_estimate) {
        return;
    }
    let observed = exact / raw_estimate;
    let mut table = calibrations()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let entry = table
        .entry((provider.to_string(), model.to_string()))
        .or_insert(observed);
    *entry = f64::midpoint(*entry, observed);
}

#[cfg(test)]
#[path = "inference_speed_tests.rs"]
mod tests;
