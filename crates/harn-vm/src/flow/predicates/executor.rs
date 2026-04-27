//! Budgeted executor for Flow invariant predicates.
//!
//! The executor is intentionally small: callers provide predicate runners, and
//! this module owns scheduling, per-kind budgets, semantic cheap-judge limits,
//! and deterministic replay drift detection.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;

use super::compose::verdict_strictness;
use crate::flow::{InvariantBlockError, InvariantResult, PredicateHash, Slice};

const DEFAULT_DETERMINISTIC_BUDGET: Duration = Duration::from_millis(50);
const DEFAULT_SEMANTIC_BUDGET: Duration = Duration::from_secs(2);
const DEFAULT_SEMANTIC_TOKEN_CAP: u64 = 1024;
const DEFAULT_MAX_DETERMINISTIC_LANES: usize = 16;
const DEFAULT_MAX_SEMANTIC_LANES: usize = 2;
const DEFAULT_MAX_DETERMINISTIC_LANES_PER_SLICE: usize = usize::MAX;
const DEFAULT_MAX_SEMANTIC_LANES_PER_SLICE: usize = 1;
const DEFAULT_SLICE_DETERMINISTIC_ENVELOPE: Duration = Duration::from_secs(5);
const DEFAULT_SLICE_SEMANTIC_ENVELOPE: Duration = Duration::from_secs(20);

/// Predicate execution mode declared by predicate author annotations.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PredicateKind {
    /// Pure Harn predicate. No shell, network, LLM, or host side effects.
    Deterministic,
    /// Semantic predicate. May make one `cheap_judge` call over pre-baked
    /// evidence within the semantic wall-clock and token budgets.
    Semantic,
}

/// Request passed to the cheap semantic judge.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheapJudgeRequest {
    pub prompt: String,
    pub evidence_key: String,
    pub evidence: String,
}

/// Response returned by the cheap semantic judge.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheapJudgeResponse {
    pub passes: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cheap_judge_version: Option<String>,
}

/// Replay-audit metadata for a semantic predicate's judge call.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticReplayAuditMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    pub prompt_hash: String,
    pub evidence_hashes: BTreeMap<String, String>,
    pub token_cap: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cheap_judge_version: Option<String>,
}

/// Host-provided adapter for semantic predicate judging.
#[async_trait(?Send)]
pub trait CheapJudge {
    async fn cheap_judge(
        &self,
        request: CheapJudgeRequest,
    ) -> Result<CheapJudgeResponse, InvariantBlockError>;
}

/// Predicate runner supplied by a collector or Harn adapter.
#[async_trait(?Send)]
pub trait PredicateRunner {
    fn hash(&self) -> PredicateHash;
    fn kind(&self) -> PredicateKind;
    fn fallback_hash(&self) -> Option<PredicateHash> {
        None
    }

    /// Static evidence captured when the predicate was authored. Semantic
    /// predicates may only judge over this map; the executor never fetches
    /// evidence during evaluation.
    fn evidence(&self) -> BTreeMap<String, String> {
        BTreeMap::new()
    }

    async fn evaluate(&self, context: PredicateContext) -> InvariantResult;
}

/// Runtime context made available to a predicate invocation.
#[derive(Clone)]
pub struct PredicateContext {
    inner: Rc<PredicateContextInner>,
}

struct PredicateContextInner {
    slice: Rc<Slice>,
    kind: PredicateKind,
    evidence: BTreeMap<String, String>,
    cheap_judge: Option<Rc<dyn CheapJudge>>,
    semantic_token_cap: u64,
    cancel_token: Arc<AtomicBool>,
    judge_state: RefCell<JudgeBudgetState>,
}

#[derive(Default)]
struct JudgeBudgetState {
    calls: u64,
    tokens: u64,
    block_error: Option<InvariantBlockError>,
    semantic_audit: Option<SemanticReplayAuditMetadata>,
}

impl PredicateContext {
    fn new(
        slice: Rc<Slice>,
        kind: PredicateKind,
        evidence: BTreeMap<String, String>,
        cheap_judge: Option<Rc<dyn CheapJudge>>,
        semantic_token_cap: u64,
        cancel_token: Arc<AtomicBool>,
    ) -> Self {
        Self {
            inner: Rc::new(PredicateContextInner {
                slice,
                kind,
                evidence,
                cheap_judge,
                semantic_token_cap,
                cancel_token,
                judge_state: RefCell::new(JudgeBudgetState::default()),
            }),
        }
    }

    pub fn slice(&self) -> &Slice {
        &self.inner.slice
    }

    pub fn kind(&self) -> PredicateKind {
        self.inner.kind
    }

    pub fn evidence(&self, key: &str) -> Option<&str> {
        self.inner.evidence.get(key).map(String::as_str)
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.cancel_token.load(Ordering::SeqCst)
    }

    /// Invoke the semantic cheap judge with executor-enforced limits.
    ///
    /// Deterministic predicates cannot call this method. Semantic predicates
    /// get one call per predicate evaluation attempt, and the request must
    /// refer to evidence already present in the predicate's evidence map.
    pub async fn cheap_judge(
        &self,
        prompt: impl Into<String>,
        evidence_key: impl Into<String>,
    ) -> Result<CheapJudgeResponse, InvariantBlockError> {
        if self.inner.kind != PredicateKind::Semantic {
            return Err(self.record_block(InvariantBlockError::new(
                "side_effect_denied",
                "deterministic predicates cannot invoke cheap_judge",
            )));
        }

        let prompt = prompt.into();
        let evidence_key = evidence_key.into();
        let Some(evidence) = self.inner.evidence.get(&evidence_key).cloned() else {
            return Err(self.record_block(InvariantBlockError::new(
                "evidence_missing",
                format!(
                    "semantic predicate requested evidence key '{evidence_key}' that was not pre-baked"
                ),
            )));
        };

        let estimated_tokens = estimate_tokens(&prompt).saturating_add(estimate_tokens(&evidence));
        {
            let mut state = self.inner.judge_state.borrow_mut();
            if state.calls >= 1 {
                let error = InvariantBlockError::budget_exceeded(
                    "semantic predicate exceeded one cheap_judge call",
                );
                state.block_error = Some(error.clone());
                return Err(error);
            }
            if state.tokens.saturating_add(estimated_tokens) > self.inner.semantic_token_cap {
                let error = InvariantBlockError::budget_exceeded(format!(
                    "semantic predicate cheap_judge request exceeds token cap {}",
                    self.inner.semantic_token_cap
                ));
                state.block_error = Some(error.clone());
                return Err(error);
            }
            state.calls += 1;
            state.tokens = state.tokens.saturating_add(estimated_tokens);
        }

        let Some(judge) = self.inner.cheap_judge.clone() else {
            return Err(self.record_block(InvariantBlockError::new(
                "llm_unavailable",
                "semantic predicate cheap_judge was requested but no judge is installed",
            )));
        };

        let response = match judge
            .cheap_judge(CheapJudgeRequest {
                prompt: prompt.clone(),
                evidence_key: evidence_key.clone(),
                evidence: evidence.clone(),
            })
            .await
        {
            Ok(response) => response,
            Err(error) => return Err(self.record_block(error)),
        };
        {
            let mut state = self.inner.judge_state.borrow_mut();
            state.semantic_audit = Some(SemanticReplayAuditMetadata {
                provider_id: response.provider_id.clone(),
                model_id: response.model_id.clone(),
                prompt_hash: stable_hash(prompt.as_bytes()),
                evidence_hashes: self
                    .inner
                    .evidence
                    .iter()
                    .map(|(key, value)| (key.clone(), stable_hash(value.as_bytes())))
                    .collect(),
                token_cap: self.inner.semantic_token_cap,
                cheap_judge_version: response.cheap_judge_version.clone(),
            });
        }
        let response_tokens = response.input_tokens.saturating_add(response.output_tokens);
        {
            let mut state = self.inner.judge_state.borrow_mut();
            state.tokens = state.tokens.saturating_add(response_tokens);
            if state.tokens > self.inner.semantic_token_cap {
                let error = InvariantBlockError::budget_exceeded(format!(
                    "semantic predicate cheap_judge response exceeded token cap {}",
                    self.inner.semantic_token_cap
                ));
                state.block_error = Some(error.clone());
                return Err(error);
            }
        }
        Ok(response)
    }

    fn cancel(&self) {
        self.inner.cancel_token.store(true, Ordering::SeqCst);
    }

    fn block_error(&self) -> Option<InvariantBlockError> {
        self.inner.judge_state.borrow().block_error.clone()
    }

    fn semantic_audit(&self) -> Option<SemanticReplayAuditMetadata> {
        self.inner.judge_state.borrow().semantic_audit.clone()
    }

    fn record_block(&self, error: InvariantBlockError) -> InvariantBlockError {
        self.inner.judge_state.borrow_mut().block_error = Some(error.clone());
        error
    }
}

/// Cross-slice scheduling configuration.
///
/// The fairness scheduler enforces three orthogonal limits on top of the
/// existing per-predicate hard timeouts:
///
/// 1. **Global lane caps** (`max_deterministic_lanes`, `max_semantic_lanes`)
///    bound how many predicates of each kind may run concurrently across all
///    queued slices. Deterministic and semantic lanes are independent so
///    deterministic work continues to make progress while semantic predicates
///    wait for cheap-judge slots.
/// 2. **Per-slice lane caps**
///    (`max_deterministic_lanes_per_slice`, `max_semantic_lanes_per_slice`)
///    keep one slice from monopolizing all global lanes. With the default
///    semantic-per-slice cap of 1, two slices each holding a permit alternate
///    fairly through the global semantic semaphore (FIFO under tokio).
/// 3. **Aggregate per-slice envelopes**
///    (`slice_deterministic_envelope`, `slice_semantic_envelope`) bound the
///    total wall-clock spent on each kind within one slice. Once exhausted,
///    every remaining predicate of that kind for that slice short-circuits to
///    a structured `budget_exceeded` block — never a panic and never an
///    implicit approval.
#[derive(Clone, Debug)]
pub struct PredicateSchedulerConfig {
    /// Global cap on concurrent deterministic predicate evaluations across
    /// all slices.
    pub max_deterministic_lanes: usize,
    /// Global cap on concurrent semantic predicate evaluations across all
    /// slices.
    pub max_semantic_lanes: usize,
    /// Per-slice cap on concurrent deterministic predicate evaluations.
    pub max_deterministic_lanes_per_slice: usize,
    /// Per-slice cap on concurrent semantic predicate evaluations. Keep this
    /// strictly less than `max_semantic_lanes` to enforce fairness when more
    /// than one slice is queued.
    pub max_semantic_lanes_per_slice: usize,
    /// Aggregate wall-clock envelope summed across all deterministic predicate
    /// attempts for one slice. Both the first attempt and the replay attempt
    /// of a deterministic predicate count against this envelope. Setting this
    /// to `Duration::ZERO` disables aggregate enforcement and falls back to
    /// the per-predicate hard timeout alone.
    pub slice_deterministic_envelope: Duration,
    /// Aggregate wall-clock envelope summed across all semantic predicate
    /// attempts for one slice. `Duration::ZERO` disables aggregate
    /// enforcement.
    pub slice_semantic_envelope: Duration,
}

impl Default for PredicateSchedulerConfig {
    fn default() -> Self {
        Self {
            max_deterministic_lanes: DEFAULT_MAX_DETERMINISTIC_LANES,
            max_semantic_lanes: DEFAULT_MAX_SEMANTIC_LANES,
            max_deterministic_lanes_per_slice: DEFAULT_MAX_DETERMINISTIC_LANES_PER_SLICE,
            max_semantic_lanes_per_slice: DEFAULT_MAX_SEMANTIC_LANES_PER_SLICE,
            slice_deterministic_envelope: DEFAULT_SLICE_DETERMINISTIC_ENVELOPE,
            slice_semantic_envelope: DEFAULT_SLICE_SEMANTIC_ENVELOPE,
        }
    }
}

/// Executor configuration.
#[derive(Clone, Debug)]
pub struct PredicateExecutorConfig {
    pub deterministic_budget: Duration,
    pub semantic_budget: Duration,
    pub semantic_token_cap: u64,
    /// Cross-slice fairness and aggregate-envelope scheduling knobs.
    pub scheduler: PredicateSchedulerConfig,
}

impl Default for PredicateExecutorConfig {
    fn default() -> Self {
        Self {
            deterministic_budget: DEFAULT_DETERMINISTIC_BUDGET,
            semantic_budget: DEFAULT_SEMANTIC_BUDGET,
            semantic_token_cap: DEFAULT_SEMANTIC_TOKEN_CAP,
            scheduler: PredicateSchedulerConfig::default(),
        }
    }
}

/// Budgeted predicate executor.
#[derive(Clone)]
pub struct PredicateExecutor {
    config: PredicateExecutorConfig,
    cheap_judge: Option<Rc<dyn CheapJudge>>,
}

impl PredicateExecutor {
    pub fn new(config: PredicateExecutorConfig) -> Self {
        Self {
            config,
            cheap_judge: None,
        }
    }

    pub fn with_cheap_judge(
        config: PredicateExecutorConfig,
        cheap_judge: Rc<dyn CheapJudge>,
    ) -> Self {
        Self {
            config,
            cheap_judge: Some(cheap_judge),
        }
    }

    /// Evaluate predicates for a single slice using the scheduler defaults.
    ///
    /// This is shorthand for [`execute_slices`](Self::execute_slices) with a
    /// single-element queue. Even one slice still flows through the cross-slice
    /// scheduler so the aggregate per-slice envelope and lane caps apply.
    pub async fn execute_slice(
        &self,
        slice: &Slice,
        predicates: &[Rc<dyn PredicateRunner>],
    ) -> PredicateExecutionReport {
        let mut reports = self
            .execute_slices(vec![(slice.clone(), predicates.to_vec())])
            .await;
        reports.pop().unwrap_or(PredicateExecutionReport {
            records: Vec::new(),
        })
    }

    /// Evaluate predicates for several candidate slices fairly.
    ///
    /// All slices share the global deterministic and semantic lane semaphores
    /// configured in [`PredicateSchedulerConfig`]. Each slice additionally
    /// owns its own per-slice lane semaphores (so one slice cannot occupy all
    /// global semantic lanes), and each slice independently tracks aggregate
    /// wall-clock against the per-kind envelopes. Once a slice exhausts an
    /// envelope, all remaining predicates of that kind for that slice
    /// short-circuit to a `budget_exceeded` `Block`.
    ///
    /// Output preserves input slice order. Within each slice, records are
    /// sorted by predicate hash so reports are deterministic regardless of
    /// the order predicates finished.
    pub async fn execute_slices(
        &self,
        slices: Vec<(Slice, Vec<Rc<dyn PredicateRunner>>)>,
    ) -> Vec<PredicateExecutionReport> {
        let scheduler = &self.config.scheduler;
        let det_global = Arc::new(Semaphore::new(clamp_permits(
            scheduler.max_deterministic_lanes,
        )));
        let sem_global = Arc::new(Semaphore::new(clamp_permits(scheduler.max_semantic_lanes)));

        let slice_futures = slices
            .into_iter()
            .map(|(slice, predicates)| {
                let det_global = det_global.clone();
                let sem_global = sem_global.clone();
                async move {
                    self.execute_slice_inner(slice, predicates, det_global, sem_global)
                        .await
                }
            })
            .collect::<Vec<_>>();

        futures::future::join_all(slice_futures).await
    }

    async fn execute_slice_inner(
        &self,
        slice: Slice,
        predicates: Vec<Rc<dyn PredicateRunner>>,
        det_global: Arc<Semaphore>,
        sem_global: Arc<Semaphore>,
    ) -> PredicateExecutionReport {
        let scheduler = &self.config.scheduler;
        let slice_rc = Rc::new(slice);
        let lanes = SliceLanes::new(
            det_global,
            sem_global,
            scheduler.max_deterministic_lanes_per_slice,
            scheduler.max_semantic_lanes_per_slice,
        );
        let envelope = SliceEnvelope::new(
            scheduler.slice_deterministic_envelope,
            scheduler.slice_semantic_envelope,
        );

        // The lane semaphores enforce real concurrency. `buffer_unordered`
        // just decides how many futures we polled-but-not-yet-completed at
        // once; cap it at the actual predicate count so we don't allocate
        // internal slots for predicates that don't exist.
        let buffer = predicates.len().max(1);

        let mut records = futures::stream::iter(predicates)
            .map(|runner| {
                let slice = slice_rc.clone();
                let lanes = lanes.clone();
                let envelope = envelope.clone();
                async move { self.execute_one(slice, runner, lanes, envelope).await }
            })
            .buffer_unordered(buffer)
            .collect::<Vec<_>>()
            .await;

        self.apply_semantic_fallbacks(&mut records);
        records.sort_by(|left, right| left.predicate_hash.cmp(&right.predicate_hash));
        PredicateExecutionReport { records }
    }

    async fn execute_one(
        &self,
        slice: Rc<Slice>,
        runner: Rc<dyn PredicateRunner>,
        lanes: SliceLanes,
        envelope: SliceEnvelope,
    ) -> PredicateExecutionRecord {
        let started = Instant::now();
        let predicate_hash = runner.hash();
        let kind = runner.kind();
        let first = self
            .run_attempt(slice.clone(), runner.as_ref(), &lanes, &envelope)
            .await;
        let first_hash = hash_result(&first.result);
        let mut result = first.result;
        let mut attempts = 1;
        let mut second_hash = None;
        let semantic_replay_audit = first.semantic_audit;

        if kind == PredicateKind::Deterministic && !result.is_blocking() {
            let second = self
                .run_attempt(slice, runner.as_ref(), &lanes, &envelope)
                .await;
            attempts = 2;
            let replay_hash = hash_result(&second.result);
            second_hash = replay_hash.clone();
            if second.result.is_blocking() {
                result = second.result;
            } else {
                match (first_hash.as_ref(), replay_hash.as_ref()) {
                    (Some(left), Some(right)) if left == right => {}
                    (Some(left), Some(right)) => {
                        result = InvariantResult::block(InvariantBlockError::nondeterministic_drift(
                            format!(
                                "deterministic predicate result drifted across replay: {left} != {right}"
                            ),
                        ));
                    }
                    _ => {
                        result = InvariantResult::block(InvariantBlockError::new(
                            "result_hash_failed",
                            "failed to hash deterministic predicate replay result",
                        ));
                    }
                }
            }
        }

        PredicateExecutionRecord {
            predicate_hash,
            kind,
            fallback_hash: runner.fallback_hash(),
            result,
            elapsed_ms: started.elapsed().as_millis() as u64,
            attempts,
            replayable: kind == PredicateKind::Deterministic,
            first_result_hash: first_hash,
            second_result_hash: second_hash,
            semantic_replay_audit,
        }
    }

    async fn run_attempt(
        &self,
        slice: Rc<Slice>,
        runner: &dyn PredicateRunner,
        lanes: &SliceLanes,
        envelope: &SliceEnvelope,
    ) -> PredicateAttempt {
        let kind = runner.kind();
        let timeout = match kind {
            PredicateKind::Deterministic => self.config.deterministic_budget,
            PredicateKind::Semantic => self.config.semantic_budget,
        };

        if let Some(attempt) =
            envelope_exhausted_attempt(envelope, kind, "before this predicate started")
        {
            return attempt;
        }

        let _permits = lanes.acquire(kind).await;

        // Re-check after acquiring the lane: the queue may have been long
        // enough that the slice's envelope drained while we waited.
        if let Some(attempt) =
            envelope_exhausted_attempt(envelope, kind, "while waiting for a lane")
        {
            return attempt;
        }

        let context = PredicateContext::new(
            slice,
            kind,
            runner.evidence(),
            self.cheap_judge.clone(),
            self.config.semantic_token_cap,
            Arc::new(AtomicBool::new(false)),
        );
        let started = Instant::now();
        let attempt = match tokio::time::timeout(timeout, runner.evaluate(context.clone())).await {
            Ok(result) => PredicateAttempt {
                result: context
                    .block_error()
                    .map(InvariantResult::block)
                    .unwrap_or(result),
                semantic_audit: context.semantic_audit(),
            },
            Err(_) => {
                context.cancel();
                PredicateAttempt {
                    result: InvariantResult::block(InvariantBlockError::budget_exceeded(format!(
                        "{kind:?} predicate exceeded {}ms budget",
                        timeout.as_millis()
                    ))),
                    semantic_audit: context.semantic_audit(),
                }
            }
        };
        envelope.charge(kind, started.elapsed());
        attempt
    }

    fn apply_semantic_fallbacks(&self, records: &mut [PredicateExecutionRecord]) {
        let by_hash = records
            .iter()
            .map(|record| {
                (
                    record.predicate_hash.clone(),
                    (record.kind, record.result.clone()),
                )
            })
            .collect::<BTreeMap<_, _>>();

        for record in records {
            if record.kind != PredicateKind::Semantic {
                continue;
            }
            let Some(fallback_hash) = record.fallback_hash.as_ref() else {
                record.result = InvariantResult::block(InvariantBlockError::new(
                    "fallback_missing",
                    "semantic predicate did not declare a deterministic fallback",
                ));
                continue;
            };
            let Some((fallback_kind, fallback_result)) = by_hash.get(fallback_hash) else {
                record.result = InvariantResult::block(InvariantBlockError::new(
                    "fallback_missing",
                    format!(
                        "semantic predicate fallback {} was not evaluated",
                        fallback_hash.as_str()
                    ),
                ));
                continue;
            };
            if *fallback_kind != PredicateKind::Deterministic {
                record.result = InvariantResult::block(InvariantBlockError::new(
                    "fallback_not_deterministic",
                    format!(
                        "semantic predicate fallback {} is not deterministic",
                        fallback_hash.as_str()
                    ),
                ));
                continue;
            }
            record.result = stricter_result(&record.result, fallback_result);
        }
    }
}

impl Default for PredicateExecutor {
    fn default() -> Self {
        Self::new(PredicateExecutorConfig::default())
    }
}

/// Per-predicate execution metadata.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PredicateExecutionRecord {
    pub predicate_hash: PredicateHash,
    pub kind: PredicateKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_hash: Option<PredicateHash>,
    pub result: InvariantResult,
    pub elapsed_ms: u64,
    pub attempts: u8,
    pub replayable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_result_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub second_result_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_replay_audit: Option<SemanticReplayAuditMetadata>,
}

/// Complete result of executing all predicates for one slice.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PredicateExecutionReport {
    pub records: Vec<PredicateExecutionRecord>,
}

impl PredicateExecutionReport {
    pub fn invariants_applied(&self) -> Vec<(PredicateHash, InvariantResult)> {
        self.records
            .iter()
            .map(|record| (record.predicate_hash.clone(), record.result.clone()))
            .collect()
    }
}

fn hash_result(result: &InvariantResult) -> Option<String> {
    let bytes = serde_json::to_vec(result).ok()?;
    Some(hex::encode(Sha256::digest(bytes)))
}

fn stable_hash(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

fn stricter_result(left: &InvariantResult, right: &InvariantResult) -> InvariantResult {
    if verdict_strictness(&left.verdict) >= verdict_strictness(&right.verdict) {
        left.clone()
    } else {
        right.clone()
    }
}

fn estimate_tokens(value: &str) -> u64 {
    value.split_whitespace().count().max(1) as u64
}

fn clamp_permits(value: usize) -> usize {
    value.clamp(1, Semaphore::MAX_PERMITS)
}

/// Short-circuit return for predicates whose slice envelope is already
/// exhausted. Returns `None` when the envelope still has headroom (or is
/// disabled by a zero budget).
fn envelope_exhausted_attempt(
    envelope: &SliceEnvelope,
    kind: PredicateKind,
    when: &str,
) -> Option<PredicateAttempt> {
    let remaining = envelope.remaining(kind)?;
    if !remaining.is_zero() {
        return None;
    }
    Some(PredicateAttempt {
        result: InvariantResult::block(InvariantBlockError::budget_exceeded(format!(
            "slice {kind:?} envelope exhausted {when}"
        ))),
        semantic_audit: None,
    })
}

struct PredicateAttempt {
    result: InvariantResult,
    semantic_audit: Option<SemanticReplayAuditMetadata>,
}

/// Per-slice lane semaphores. Pairs a global (cross-slice) semaphore with a
/// per-slice semaphore so a single slice cannot occupy every global lane.
#[derive(Clone)]
struct SliceLanes {
    deterministic_global: Arc<Semaphore>,
    deterministic_local: Arc<Semaphore>,
    semantic_global: Arc<Semaphore>,
    semantic_local: Arc<Semaphore>,
}

impl SliceLanes {
    fn new(
        deterministic_global: Arc<Semaphore>,
        semantic_global: Arc<Semaphore>,
        deterministic_per_slice: usize,
        semantic_per_slice: usize,
    ) -> Self {
        Self {
            deterministic_global,
            deterministic_local: Arc::new(Semaphore::new(clamp_permits(deterministic_per_slice))),
            semantic_global,
            semantic_local: Arc::new(Semaphore::new(clamp_permits(semantic_per_slice))),
        }
    }

    async fn acquire(&self, kind: PredicateKind) -> LaneTickets {
        let (global, local) = match kind {
            PredicateKind::Deterministic => (&self.deterministic_global, &self.deterministic_local),
            PredicateKind::Semantic => (&self.semantic_global, &self.semantic_local),
        };
        // Take per-slice first to preserve global FIFO admission. Acquiring
        // the global permit first would let one slice park its waiters at the
        // head of the global queue and starve other slices' lanes.
        let local_ticket = local
            .clone()
            .acquire_owned()
            .await
            .expect("predicate lane semaphore closed");
        let global_ticket = global
            .clone()
            .acquire_owned()
            .await
            .expect("predicate lane semaphore closed");
        LaneTickets {
            _local: local_ticket,
            _global: global_ticket,
        }
    }
}

/// RAII permit holder. Both permits release when the value drops.
struct LaneTickets {
    _local: tokio::sync::OwnedSemaphorePermit,
    _global: tokio::sync::OwnedSemaphorePermit,
}

/// Aggregate per-kind wall-clock counters for one slice's predicate envelope.
#[derive(Clone)]
struct SliceEnvelope {
    deterministic_used: Rc<Cell<Duration>>,
    semantic_used: Rc<Cell<Duration>>,
    deterministic_budget: Duration,
    semantic_budget: Duration,
}

impl SliceEnvelope {
    fn new(deterministic_budget: Duration, semantic_budget: Duration) -> Self {
        Self {
            deterministic_used: Rc::new(Cell::new(Duration::ZERO)),
            semantic_used: Rc::new(Cell::new(Duration::ZERO)),
            deterministic_budget,
            semantic_budget,
        }
    }

    fn cell(&self, kind: PredicateKind) -> &Cell<Duration> {
        match kind {
            PredicateKind::Deterministic => &self.deterministic_used,
            PredicateKind::Semantic => &self.semantic_used,
        }
    }

    fn budget(&self, kind: PredicateKind) -> Duration {
        match kind {
            PredicateKind::Deterministic => self.deterministic_budget,
            PredicateKind::Semantic => self.semantic_budget,
        }
    }

    fn remaining(&self, kind: PredicateKind) -> Option<Duration> {
        let budget = self.budget(kind);
        if budget.is_zero() {
            return None;
        }
        let used = self.cell(kind).get();
        Some(budget.saturating_sub(used))
    }

    fn charge(&self, kind: PredicateKind, elapsed: Duration) {
        let cell = self.cell(kind);
        cell.set(cell.get().saturating_add(elapsed));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::{Approval, AtomId, PredicateHash, Slice, SliceId, SliceStatus, TestId};
    use std::cell::Cell;

    fn slice() -> Slice {
        Slice {
            id: SliceId([9; 32]),
            atoms: vec![AtomId([1; 32])],
            intents: Vec::new(),
            invariants_applied: Vec::new(),
            required_tests: vec![TestId::new("test:unit")],
            approval_chain: Vec::<Approval>::new(),
            base_ref: AtomId([0; 32]),
            status: SliceStatus::Ready,
        }
    }

    struct StaticPredicate {
        hash: &'static str,
        kind: PredicateKind,
        fallback_hash: Option<&'static str>,
        result: InvariantResult,
        delay: Duration,
    }

    #[async_trait(?Send)]
    impl PredicateRunner for StaticPredicate {
        fn hash(&self) -> PredicateHash {
            PredicateHash::new(self.hash)
        }

        fn kind(&self) -> PredicateKind {
            self.kind
        }

        fn fallback_hash(&self) -> Option<PredicateHash> {
            self.fallback_hash.map(PredicateHash::new)
        }

        async fn evaluate(&self, _context: PredicateContext) -> InvariantResult {
            if !self.delay.is_zero() {
                tokio::time::sleep(self.delay).await;
            }
            self.result.clone()
        }
    }

    struct DriftingPredicate {
        calls: Cell<u64>,
    }

    #[async_trait(?Send)]
    impl PredicateRunner for DriftingPredicate {
        fn hash(&self) -> PredicateHash {
            PredicateHash::new("drift")
        }

        fn kind(&self) -> PredicateKind {
            PredicateKind::Deterministic
        }

        async fn evaluate(&self, _context: PredicateContext) -> InvariantResult {
            let calls = self.calls.get();
            self.calls.set(calls + 1);
            if calls == 0 {
                InvariantResult::allow()
            } else {
                InvariantResult::warn("changed")
            }
        }
    }

    struct SemanticPredicate {
        calls: u8,
        fallback_hash: Option<&'static str>,
    }

    #[async_trait(?Send)]
    impl PredicateRunner for SemanticPredicate {
        fn hash(&self) -> PredicateHash {
            PredicateHash::new(format!("semantic-{}", self.calls))
        }

        fn kind(&self) -> PredicateKind {
            PredicateKind::Semantic
        }

        fn fallback_hash(&self) -> Option<PredicateHash> {
            self.fallback_hash.map(PredicateHash::new)
        }

        fn evidence(&self) -> BTreeMap<String, String> {
            BTreeMap::from([("case".to_string(), "pre-baked evidence".to_string())])
        }

        async fn evaluate(&self, context: PredicateContext) -> InvariantResult {
            for _ in 0..self.calls {
                let Err(error) = context.cheap_judge("judge the case", "case").await else {
                    continue;
                };
                return InvariantResult::block(error);
            }
            InvariantResult::allow()
        }
    }

    struct PassingJudge;

    #[async_trait(?Send)]
    impl CheapJudge for PassingJudge {
        async fn cheap_judge(
            &self,
            _request: CheapJudgeRequest,
        ) -> Result<CheapJudgeResponse, InvariantBlockError> {
            Ok(CheapJudgeResponse {
                passes: true,
                reason: None,
                input_tokens: 2,
                output_tokens: 1,
                provider_id: Some("mock-provider".to_string()),
                model_id: Some("mock-model-1".to_string()),
                cheap_judge_version: Some("cheap-judge-v1".to_string()),
            })
        }
    }

    struct ParallelProbe {
        hash: &'static str,
        kind: PredicateKind,
        active: Rc<Cell<usize>>,
        max_active: Rc<Cell<usize>>,
    }

    #[async_trait(?Send)]
    impl PredicateRunner for ParallelProbe {
        fn hash(&self) -> PredicateHash {
            PredicateHash::new(self.hash)
        }

        fn kind(&self) -> PredicateKind {
            self.kind
        }

        async fn evaluate(&self, _context: PredicateContext) -> InvariantResult {
            let active = self.active.get() + 1;
            self.active.set(active);
            self.max_active.set(self.max_active.get().max(active));
            tokio::time::sleep(Duration::from_millis(10)).await;
            self.active.set(self.active.get() - 1);
            InvariantResult::allow()
        }
    }

    #[tokio::test]
    async fn deterministic_predicate_replays_bit_identically() {
        let executor = PredicateExecutor::default();
        let predicates: Vec<Rc<dyn PredicateRunner>> = vec![Rc::new(StaticPredicate {
            hash: "stable",
            kind: PredicateKind::Deterministic,
            fallback_hash: None,
            result: InvariantResult::allow(),
            delay: Duration::ZERO,
        })];

        let report = executor.execute_slice(&slice(), &predicates).await;

        assert_eq!(report.records.len(), 1);
        let record = &report.records[0];
        assert_eq!(record.result, InvariantResult::allow());
        assert_eq!(record.attempts, 2);
        assert_eq!(record.first_result_hash, record.second_result_hash);
    }

    #[tokio::test]
    async fn deterministic_drift_blocks_the_predicate() {
        let executor = PredicateExecutor::default();
        let predicates: Vec<Rc<dyn PredicateRunner>> = vec![Rc::new(DriftingPredicate {
            calls: Cell::new(0),
        })];

        let report = executor.execute_slice(&slice(), &predicates).await;

        let block = report.records[0].result.block_error().expect("blocked");
        assert_eq!(block.code, "nondeterministic_drift");
    }

    #[tokio::test]
    async fn deterministic_budget_overrun_blocks_instead_of_panicking() {
        let executor = PredicateExecutor::new(PredicateExecutorConfig {
            deterministic_budget: Duration::from_millis(1),
            ..PredicateExecutorConfig::default()
        });
        let predicates: Vec<Rc<dyn PredicateRunner>> = vec![Rc::new(StaticPredicate {
            hash: "slow",
            kind: PredicateKind::Deterministic,
            fallback_hash: None,
            result: InvariantResult::allow(),
            delay: Duration::from_millis(20),
        })];

        let report = executor.execute_slice(&slice(), &predicates).await;

        let block = report.records[0].result.block_error().expect("blocked");
        assert_eq!(block.code, "budget_exceeded");
    }

    #[tokio::test]
    async fn predicates_are_polled_concurrently_for_a_slice() {
        let active = Rc::new(Cell::new(0));
        let max_active = Rc::new(Cell::new(0));
        let predicates: Vec<Rc<dyn PredicateRunner>> = vec![
            Rc::new(ParallelProbe {
                hash: "parallel-a",
                kind: PredicateKind::Deterministic,
                active: active.clone(),
                max_active: max_active.clone(),
            }),
            Rc::new(ParallelProbe {
                hash: "parallel-b",
                kind: PredicateKind::Deterministic,
                active,
                max_active: max_active.clone(),
            }),
        ];

        let report = PredicateExecutor::default()
            .execute_slice(&slice(), &predicates)
            .await;

        assert_eq!(report.records.len(), 2);
        assert_eq!(max_active.get(), 2);
    }

    #[tokio::test]
    async fn semantic_predicate_gets_one_cheap_judge_call() {
        let executor = PredicateExecutor::with_cheap_judge(
            PredicateExecutorConfig::default(),
            Rc::new(PassingJudge),
        );
        let predicates: Vec<Rc<dyn PredicateRunner>> = vec![
            Rc::new(SemanticPredicate {
                calls: 2,
                fallback_hash: Some("fallback"),
            }),
            Rc::new(StaticPredicate {
                hash: "fallback",
                kind: PredicateKind::Deterministic,
                fallback_hash: None,
                result: InvariantResult::allow(),
                delay: Duration::ZERO,
            }),
        ];

        let report = executor.execute_slice(&slice(), &predicates).await;

        let semantic = report
            .records
            .iter()
            .find(|record| record.kind == PredicateKind::Semantic)
            .unwrap();
        let block = semantic.result.block_error().expect("blocked");
        assert_eq!(block.code, "budget_exceeded");
    }

    #[tokio::test]
    async fn semantic_and_fallback_agree_records_both_results() {
        let executor = PredicateExecutor::default();
        let predicates: Vec<Rc<dyn PredicateRunner>> = vec![
            Rc::new(StaticPredicate {
                hash: "semantic",
                kind: PredicateKind::Semantic,
                fallback_hash: Some("fallback"),
                result: InvariantResult::warn("semantic concern"),
                delay: Duration::ZERO,
            }),
            Rc::new(StaticPredicate {
                hash: "fallback",
                kind: PredicateKind::Deterministic,
                fallback_hash: None,
                result: InvariantResult::warn("fallback concern"),
                delay: Duration::ZERO,
            }),
        ];

        let report = executor.execute_slice(&slice(), &predicates).await;

        assert_eq!(report.records.len(), 2);
        assert_eq!(report.invariants_applied().len(), 2);
        let semantic = report
            .records
            .iter()
            .find(|record| record.predicate_hash == PredicateHash::new("semantic"))
            .unwrap();
        assert_eq!(semantic.fallback_hash, Some(PredicateHash::new("fallback")));
        assert!(matches!(
            semantic.result.verdict,
            crate::flow::Verdict::Warn { .. }
        ));
    }

    #[tokio::test]
    async fn semantic_fallback_disagreement_selects_stricter_verdict() {
        let executor = PredicateExecutor::default();
        let predicates: Vec<Rc<dyn PredicateRunner>> = vec![
            Rc::new(StaticPredicate {
                hash: "semantic",
                kind: PredicateKind::Semantic,
                fallback_hash: Some("fallback"),
                result: InvariantResult::allow(),
                delay: Duration::ZERO,
            }),
            Rc::new(StaticPredicate {
                hash: "fallback",
                kind: PredicateKind::Deterministic,
                fallback_hash: None,
                result: InvariantResult::block(InvariantBlockError::new(
                    "fallback_policy",
                    "fallback blocked",
                )),
                delay: Duration::ZERO,
            }),
        ];

        let report = executor.execute_slice(&slice(), &predicates).await;
        let semantic = report
            .records
            .iter()
            .find(|record| record.predicate_hash == PredicateHash::new("semantic"))
            .unwrap();

        let block = semantic
            .result
            .block_error()
            .expect("stricter fallback wins");
        assert_eq!(block.code, "fallback_policy");
    }

    #[tokio::test]
    async fn semantic_missing_fallback_blocks() {
        let executor = PredicateExecutor::default();
        let predicates: Vec<Rc<dyn PredicateRunner>> = vec![Rc::new(StaticPredicate {
            hash: "semantic",
            kind: PredicateKind::Semantic,
            fallback_hash: None,
            result: InvariantResult::allow(),
            delay: Duration::ZERO,
        })];

        let report = executor.execute_slice(&slice(), &predicates).await;

        let block = report.records[0].result.block_error().expect("blocked");
        assert_eq!(block.code, "fallback_missing");
    }

    #[tokio::test]
    async fn semantic_predicate_requires_prebaked_evidence() {
        struct MissingEvidence;

        #[async_trait(?Send)]
        impl PredicateRunner for MissingEvidence {
            fn hash(&self) -> PredicateHash {
                PredicateHash::new("missing-evidence")
            }

            fn kind(&self) -> PredicateKind {
                PredicateKind::Semantic
            }

            fn fallback_hash(&self) -> Option<PredicateHash> {
                Some(PredicateHash::new("fallback"))
            }

            async fn evaluate(&self, context: PredicateContext) -> InvariantResult {
                match context.cheap_judge("judge", "missing").await {
                    Ok(_) => InvariantResult::allow(),
                    Err(error) => InvariantResult::block(error),
                }
            }
        }

        let executor = PredicateExecutor::with_cheap_judge(
            PredicateExecutorConfig::default(),
            Rc::new(PassingJudge),
        );
        let predicates: Vec<Rc<dyn PredicateRunner>> = vec![
            Rc::new(MissingEvidence),
            Rc::new(StaticPredicate {
                hash: "fallback",
                kind: PredicateKind::Deterministic,
                fallback_hash: None,
                result: InvariantResult::allow(),
                delay: Duration::ZERO,
            }),
        ];

        let report = executor.execute_slice(&slice(), &predicates).await;

        let semantic = report
            .records
            .iter()
            .find(|record| record.kind == PredicateKind::Semantic)
            .unwrap();
        let block = semantic.result.block_error().expect("blocked");
        assert_eq!(block.code, "evidence_missing");
    }

    #[tokio::test]
    async fn semantic_replay_audit_records_judge_hashes() {
        let executor = PredicateExecutor::with_cheap_judge(
            PredicateExecutorConfig::default(),
            Rc::new(PassingJudge),
        );
        let predicates: Vec<Rc<dyn PredicateRunner>> = vec![
            Rc::new(SemanticPredicate {
                calls: 1,
                fallback_hash: Some("fallback"),
            }),
            Rc::new(StaticPredicate {
                hash: "fallback",
                kind: PredicateKind::Deterministic,
                fallback_hash: None,
                result: InvariantResult::allow(),
                delay: Duration::ZERO,
            }),
        ];

        let report = executor.execute_slice(&slice(), &predicates).await;
        let semantic = report
            .records
            .iter()
            .find(|record| record.kind == PredicateKind::Semantic)
            .unwrap();
        let audit = semantic
            .semantic_replay_audit
            .as_ref()
            .expect("semantic audit metadata");

        assert_eq!(audit.provider_id.as_deref(), Some("mock-provider"));
        assert_eq!(audit.model_id.as_deref(), Some("mock-model-1"));
        assert_eq!(audit.prompt_hash, stable_hash("judge the case".as_bytes()));
        let expected_evidence_hash = stable_hash("pre-baked evidence".as_bytes());
        assert_eq!(
            audit.evidence_hashes.get("case").map(String::as_str),
            Some(expected_evidence_hash.as_str())
        );
        assert_eq!(audit.token_cap, DEFAULT_SEMANTIC_TOKEN_CAP);
    }

    fn slice_with_id(id: u8) -> Slice {
        let mut value = slice();
        value.id = SliceId([id; 32]);
        value
    }

    /// Probe that records the wall-clock instant it became active, so a test
    /// can compare which slice actually got a lane first.
    struct FinishingProbe {
        hash: &'static str,
        kind: PredicateKind,
        delay: Duration,
        // Per-slice list of (predicate_hash, finish_micros_since_start).
        finish_log: Rc<RefCell<Vec<(String, u128)>>>,
        epoch: Instant,
    }

    #[async_trait(?Send)]
    impl PredicateRunner for FinishingProbe {
        fn hash(&self) -> PredicateHash {
            PredicateHash::new(self.hash)
        }

        fn kind(&self) -> PredicateKind {
            self.kind
        }

        async fn evaluate(&self, _context: PredicateContext) -> InvariantResult {
            tokio::time::sleep(self.delay).await;
            self.finish_log
                .borrow_mut()
                .push((self.hash.to_string(), self.epoch.elapsed().as_micros()));
            InvariantResult::allow()
        }
    }

    #[tokio::test]
    async fn execute_slices_returns_one_report_per_slice_in_input_order() {
        let executor = PredicateExecutor::default();
        let slice_a = slice_with_id(1);
        let slice_b = slice_with_id(2);

        let predicates_a: Vec<Rc<dyn PredicateRunner>> = vec![Rc::new(StaticPredicate {
            hash: "alpha",
            kind: PredicateKind::Deterministic,
            fallback_hash: None,
            result: InvariantResult::allow(),
            delay: Duration::ZERO,
        })];
        let predicates_b: Vec<Rc<dyn PredicateRunner>> = vec![Rc::new(StaticPredicate {
            hash: "beta",
            kind: PredicateKind::Deterministic,
            fallback_hash: None,
            result: InvariantResult::allow(),
            delay: Duration::ZERO,
        })];

        let reports = executor
            .execute_slices(vec![(slice_a, predicates_a), (slice_b, predicates_b)])
            .await;

        assert_eq!(reports.len(), 2);
        assert_eq!(
            reports[0].records[0].predicate_hash,
            PredicateHash::new("alpha")
        );
        assert_eq!(
            reports[1].records[0].predicate_hash,
            PredicateHash::new("beta")
        );
    }

    #[tokio::test]
    async fn semantic_lane_fairness_prevents_monopolization() {
        // Two slices each enqueue four semantic predicates. With one global
        // semantic lane plus a per-slice cap of one, slice B's first
        // predicate must execute before slice A's last predicate, even
        // though slice A submitted its predicates first.
        let config = PredicateExecutorConfig {
            scheduler: PredicateSchedulerConfig {
                max_semantic_lanes: 1,
                max_semantic_lanes_per_slice: 1,
                slice_semantic_envelope: Duration::from_secs(60),
                ..PredicateSchedulerConfig::default()
            },
            ..PredicateExecutorConfig::default()
        };
        let executor = PredicateExecutor::with_cheap_judge(config, Rc::new(PassingJudge));

        let log_a = Rc::new(RefCell::new(Vec::new()));
        let log_b = Rc::new(RefCell::new(Vec::new()));
        let epoch = Instant::now();

        let predicates_a: Vec<Rc<dyn PredicateRunner>> = (0..4)
            .map(|i| -> Rc<dyn PredicateRunner> {
                Rc::new(FinishingProbe {
                    hash: ["a0", "a1", "a2", "a3"][i],
                    kind: PredicateKind::Semantic,
                    delay: Duration::from_millis(20),
                    finish_log: log_a.clone(),
                    epoch,
                })
            })
            .collect();
        let predicates_b: Vec<Rc<dyn PredicateRunner>> = (0..4)
            .map(|i| -> Rc<dyn PredicateRunner> {
                Rc::new(FinishingProbe {
                    hash: ["b0", "b1", "b2", "b3"][i],
                    kind: PredicateKind::Semantic,
                    delay: Duration::from_millis(20),
                    finish_log: log_b.clone(),
                    epoch,
                })
            })
            .collect();

        let _ = executor
            .execute_slices(vec![
                (slice_with_id(1), predicates_a),
                (slice_with_id(2), predicates_b),
            ])
            .await;

        let log_a_snapshot = log_a.borrow().clone();
        let log_b_snapshot = log_b.borrow().clone();
        assert_eq!(log_a_snapshot.len(), 4);
        assert_eq!(log_b_snapshot.len(), 4);
        let earliest_b = log_b_snapshot
            .iter()
            .map(|(_, micros)| *micros)
            .min()
            .unwrap();
        let latest_a = log_a_snapshot
            .iter()
            .map(|(_, micros)| *micros)
            .max()
            .unwrap();
        assert!(
            earliest_b < latest_a,
            "fair scheduler should interleave slices: B's first finished at {earliest_b}us, A's last at {latest_a}us",
        );
    }

    #[tokio::test]
    async fn deterministic_progress_continues_while_semantic_work_waits() {
        // Single global semantic lane with a slow semantic predicate that
        // dominates the scheduler. Deterministic predicates must complete
        // before the semantic predicate because deterministic and semantic
        // lanes are independent.
        let config = PredicateExecutorConfig {
            semantic_budget: Duration::from_secs(10),
            scheduler: PredicateSchedulerConfig {
                max_semantic_lanes: 1,
                max_semantic_lanes_per_slice: 1,
                slice_semantic_envelope: Duration::from_secs(60),
                ..PredicateSchedulerConfig::default()
            },
            ..PredicateExecutorConfig::default()
        };
        let executor = PredicateExecutor::with_cheap_judge(config, Rc::new(PassingJudge));

        let log = Rc::new(RefCell::new(Vec::new()));
        let epoch = Instant::now();

        let predicates: Vec<Rc<dyn PredicateRunner>> = vec![
            Rc::new(FinishingProbe {
                hash: "slow-semantic",
                kind: PredicateKind::Semantic,
                delay: Duration::from_millis(80),
                finish_log: log.clone(),
                epoch,
            }),
            Rc::new(FinishingProbe {
                hash: "det-1",
                kind: PredicateKind::Deterministic,
                delay: Duration::from_millis(5),
                finish_log: log.clone(),
                epoch,
            }),
            Rc::new(FinishingProbe {
                hash: "det-2",
                kind: PredicateKind::Deterministic,
                delay: Duration::from_millis(5),
                finish_log: log.clone(),
                epoch,
            }),
        ];

        let _ = executor.execute_slice(&slice(), &predicates).await;
        let snapshot = log.borrow().clone();
        let det_finish = snapshot
            .iter()
            .filter_map(|(name, micros)| name.starts_with("det-").then_some(*micros))
            .max()
            .expect("deterministic finished");
        let semantic_finish = snapshot
            .iter()
            .find(|(name, _)| name == "slow-semantic")
            .map(|(_, micros)| *micros)
            .expect("semantic finished");

        assert!(
            det_finish < semantic_finish,
            "deterministic ({det_finish}us) should finish before slow semantic ({semantic_finish}us)",
        );
    }

    #[tokio::test]
    async fn slice_deterministic_envelope_blocks_remaining_predicates() {
        // Tight aggregate envelope with predicates that each consume
        // measurable wall-clock. After the first runs, the envelope is
        // exhausted and the rest must short-circuit to budget_exceeded.
        let config = PredicateExecutorConfig {
            scheduler: PredicateSchedulerConfig {
                max_deterministic_lanes_per_slice: 1,
                slice_deterministic_envelope: Duration::from_millis(15),
                ..PredicateSchedulerConfig::default()
            },
            ..PredicateExecutorConfig::default()
        };
        let executor = PredicateExecutor::new(config);
        let predicates: Vec<Rc<dyn PredicateRunner>> = vec![
            Rc::new(StaticPredicate {
                hash: "first",
                kind: PredicateKind::Deterministic,
                fallback_hash: None,
                result: InvariantResult::allow(),
                delay: Duration::from_millis(20),
            }),
            Rc::new(StaticPredicate {
                hash: "second",
                kind: PredicateKind::Deterministic,
                fallback_hash: None,
                result: InvariantResult::allow(),
                delay: Duration::ZERO,
            }),
            Rc::new(StaticPredicate {
                hash: "third",
                kind: PredicateKind::Deterministic,
                fallback_hash: None,
                result: InvariantResult::allow(),
                delay: Duration::ZERO,
            }),
        ];

        let report = executor.execute_slice(&slice(), &predicates).await;

        let by_hash: BTreeMap<_, _> = report
            .records
            .iter()
            .map(|record| (record.predicate_hash.as_str().to_string(), record))
            .collect();
        // The first predicate runs (and may itself block on the per-predicate
        // 50ms timeout — irrelevant for envelope semantics). The second and
        // third must be denied admission with structured budget_exceeded
        // blocks rather than silently allowing them through.
        let second = by_hash.get("second").expect("second present");
        let third = by_hash.get("third").expect("third present");
        let second_block = second.result.block_error().expect("second blocked");
        assert_eq!(second_block.code, "budget_exceeded");
        let third_block = third.result.block_error().expect("third blocked");
        assert_eq!(third_block.code, "budget_exceeded");
    }

    #[tokio::test]
    async fn slice_semantic_envelope_blocks_remaining_predicates() {
        let config = PredicateExecutorConfig {
            scheduler: PredicateSchedulerConfig {
                max_semantic_lanes: 1,
                max_semantic_lanes_per_slice: 1,
                slice_semantic_envelope: Duration::from_millis(15),
                ..PredicateSchedulerConfig::default()
            },
            ..PredicateExecutorConfig::default()
        };
        let executor = PredicateExecutor::with_cheap_judge(config, Rc::new(PassingJudge));

        let predicates: Vec<Rc<dyn PredicateRunner>> = vec![
            Rc::new(StaticPredicate {
                hash: "sem-first",
                kind: PredicateKind::Semantic,
                fallback_hash: Some("sem-fallback"),
                result: InvariantResult::allow(),
                delay: Duration::from_millis(30),
            }),
            Rc::new(StaticPredicate {
                hash: "sem-second",
                kind: PredicateKind::Semantic,
                fallback_hash: Some("sem-fallback"),
                result: InvariantResult::allow(),
                delay: Duration::ZERO,
            }),
            Rc::new(StaticPredicate {
                hash: "sem-fallback",
                kind: PredicateKind::Deterministic,
                fallback_hash: None,
                result: InvariantResult::allow(),
                delay: Duration::ZERO,
            }),
        ];

        let report = executor.execute_slice(&slice(), &predicates).await;
        let second = report
            .records
            .iter()
            .find(|record| record.predicate_hash == PredicateHash::new("sem-second"))
            .expect("sem-second present");
        let block = second.result.block_error().expect("blocked");
        assert_eq!(block.code, "budget_exceeded");
    }

    #[tokio::test]
    async fn slice_envelopes_are_independent_across_slices() {
        // Slice A blows its semantic envelope. Slice B's semantic predicate
        // must still run normally; envelopes are per-slice, not global.
        let config = PredicateExecutorConfig {
            scheduler: PredicateSchedulerConfig {
                max_semantic_lanes: 2,
                max_semantic_lanes_per_slice: 1,
                slice_semantic_envelope: Duration::from_millis(15),
                ..PredicateSchedulerConfig::default()
            },
            ..PredicateExecutorConfig::default()
        };
        let executor = PredicateExecutor::with_cheap_judge(config, Rc::new(PassingJudge));

        let slice_a_preds: Vec<Rc<dyn PredicateRunner>> = vec![
            Rc::new(StaticPredicate {
                hash: "a-slow",
                kind: PredicateKind::Semantic,
                fallback_hash: Some("a-fallback"),
                result: InvariantResult::allow(),
                delay: Duration::from_millis(30),
            }),
            Rc::new(StaticPredicate {
                hash: "a-second",
                kind: PredicateKind::Semantic,
                fallback_hash: Some("a-fallback"),
                result: InvariantResult::allow(),
                delay: Duration::ZERO,
            }),
            Rc::new(StaticPredicate {
                hash: "a-fallback",
                kind: PredicateKind::Deterministic,
                fallback_hash: None,
                result: InvariantResult::allow(),
                delay: Duration::ZERO,
            }),
        ];
        let slice_b_preds: Vec<Rc<dyn PredicateRunner>> = vec![
            Rc::new(StaticPredicate {
                hash: "b-fast",
                kind: PredicateKind::Semantic,
                fallback_hash: Some("b-fallback"),
                result: InvariantResult::allow(),
                delay: Duration::from_millis(2),
            }),
            Rc::new(StaticPredicate {
                hash: "b-fallback",
                kind: PredicateKind::Deterministic,
                fallback_hash: None,
                result: InvariantResult::allow(),
                delay: Duration::ZERO,
            }),
        ];

        let reports = executor
            .execute_slices(vec![
                (slice_with_id(1), slice_a_preds),
                (slice_with_id(2), slice_b_preds),
            ])
            .await;

        let slice_b = &reports[1];
        let b_fast = slice_b
            .records
            .iter()
            .find(|record| record.predicate_hash == PredicateHash::new("b-fast"))
            .unwrap();
        // b-fast must not be blocked by budget_exceeded — slice A's envelope
        // exhaustion must not cross the slice boundary.
        assert!(b_fast.result.block_error().is_none());
    }

    #[tokio::test]
    async fn output_ordering_is_deterministic_across_random_finish_order() {
        // Predicates with varying delays finish in non-deterministic order.
        // The report must still sort by predicate hash so two replays of the
        // same scheduler produce bit-identical record orderings.
        let make_predicates = || -> Vec<Rc<dyn PredicateRunner>> {
            vec![
                Rc::new(StaticPredicate {
                    hash: "z-last",
                    kind: PredicateKind::Deterministic,
                    fallback_hash: None,
                    result: InvariantResult::allow(),
                    delay: Duration::from_millis(15),
                }),
                Rc::new(StaticPredicate {
                    hash: "a-first",
                    kind: PredicateKind::Deterministic,
                    fallback_hash: None,
                    result: InvariantResult::allow(),
                    delay: Duration::ZERO,
                }),
                Rc::new(StaticPredicate {
                    hash: "m-mid",
                    kind: PredicateKind::Deterministic,
                    fallback_hash: None,
                    result: InvariantResult::allow(),
                    delay: Duration::from_millis(7),
                }),
            ]
        };

        let executor = PredicateExecutor::default();
        let report_one = executor.execute_slice(&slice(), &make_predicates()).await;
        let report_two = executor.execute_slice(&slice(), &make_predicates()).await;
        let order_one: Vec<_> = report_one
            .records
            .iter()
            .map(|record| record.predicate_hash.as_str().to_string())
            .collect();
        let order_two: Vec<_> = report_two
            .records
            .iter()
            .map(|record| record.predicate_hash.as_str().to_string())
            .collect();

        assert_eq!(order_one, vec!["a-first", "m-mid", "z-last"]);
        assert_eq!(order_one, order_two);
    }
}
