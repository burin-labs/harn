//! Budgeted executor for Flow invariant predicates.
//!
//! The executor is intentionally small: callers provide predicate runners, and
//! this module owns scheduling, per-kind budgets, semantic cheap-judge limits,
//! and deterministic replay drift detection.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
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

/// How a semantic predicate uses its deterministic fallback.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticFallbackPolicy {
    /// The semantic and deterministic results are composed by strictness.
    #[default]
    Enforce,
    /// The deterministic result is evaluated and recorded for replay audit,
    /// but does not alter the live semantic verdict.
    Advisory,
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
#[async_trait]
pub trait CheapJudge: Send + Sync {
    async fn cheap_judge(
        &self,
        request: CheapJudgeRequest,
    ) -> Result<CheapJudgeResponse, InvariantBlockError>;
}

/// Predicate runner supplied by a collector or Harn adapter.
#[async_trait]
pub trait PredicateRunner: Send + Sync {
    fn hash(&self) -> PredicateHash;
    fn name(&self) -> String {
        self.hash().as_str().to_string()
    }
    fn kind(&self) -> PredicateKind;
    fn fallback_hash(&self) -> Option<PredicateHash> {
        None
    }
    fn fallback_policy(&self) -> SemanticFallbackPolicy {
        SemanticFallbackPolicy::Enforce
    }
    fn fallback_diagnostic(&self) -> Option<InvariantBlockError> {
        None
    }
    fn raw_result(&self) -> Option<serde_json::Value> {
        None
    }
    fn enforced(&self) -> bool {
        true
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
    inner: Arc<PredicateContextInner>,
}

struct PredicateContextInner {
    slice: Arc<Slice>,
    kind: PredicateKind,
    evidence: BTreeMap<String, String>,
    cheap_judge: Option<Arc<dyn CheapJudge>>,
    semantic_token_cap: u64,
    cancel_token: Arc<AtomicBool>,
    judge_state: Mutex<JudgeBudgetState>,
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
        slice: Arc<Slice>,
        kind: PredicateKind,
        evidence: BTreeMap<String, String>,
        cheap_judge: Option<Arc<dyn CheapJudge>>,
        semantic_token_cap: u64,
        cancel_token: Arc<AtomicBool>,
    ) -> Self {
        Self {
            inner: Arc::new(PredicateContextInner {
                slice,
                kind,
                evidence,
                cheap_judge,
                semantic_token_cap,
                cancel_token,
                judge_state: Mutex::new(JudgeBudgetState::default()),
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
            let mut state = self
                .inner
                .judge_state
                .lock()
                .expect("predicate judge state lock");
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
            let mut state = self
                .inner
                .judge_state
                .lock()
                .expect("predicate judge state lock");
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
            let mut state = self
                .inner
                .judge_state
                .lock()
                .expect("predicate judge state lock");
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
        self.inner
            .judge_state
            .lock()
            .expect("predicate judge state lock")
            .block_error
            .clone()
    }

    fn semantic_audit(&self) -> Option<SemanticReplayAuditMetadata> {
        self.inner
            .judge_state
            .lock()
            .expect("predicate judge state lock")
            .semantic_audit
            .clone()
    }

    fn record_block(&self, error: InvariantBlockError) -> InvariantBlockError {
        self.inner
            .judge_state
            .lock()
            .expect("predicate judge state lock")
            .block_error = Some(error.clone());
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
    /// Re-run successful deterministic predicates to detect result drift.
    pub replay_deterministic: bool,
    /// Cross-slice fairness and aggregate-envelope scheduling knobs.
    pub scheduler: PredicateSchedulerConfig,
}

impl Default for PredicateExecutorConfig {
    fn default() -> Self {
        Self {
            deterministic_budget: DEFAULT_DETERMINISTIC_BUDGET,
            semantic_budget: DEFAULT_SEMANTIC_BUDGET,
            semantic_token_cap: DEFAULT_SEMANTIC_TOKEN_CAP,
            replay_deterministic: true,
            scheduler: PredicateSchedulerConfig::default(),
        }
    }
}

/// Budgeted predicate executor.
#[derive(Clone)]
pub struct PredicateExecutor {
    config: PredicateExecutorConfig,
    cheap_judge: Option<Arc<dyn CheapJudge>>,
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
        cheap_judge: Arc<dyn CheapJudge>,
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
        predicates: &[Arc<dyn PredicateRunner>],
    ) -> PredicateExecutionReport {
        let mut reports = self
            .execute_slices(vec![(slice.clone(), predicates.to_vec())])
            .await;
        reports.pop().unwrap_or(PredicateExecutionReport {
            records: Vec::new(),
            skipped: Vec::new(),
        })
    }

    /// Evaluate one slice serially through the same budget, replay, fallback,
    /// and record implementation used by the concurrent scheduler.
    pub async fn execute_slice_serial(
        &self,
        slice: &Slice,
        predicates: &[Arc<dyn PredicateRunner>],
    ) -> PredicateExecutionReport {
        let scheduler = &self.config.scheduler;
        let lanes = SliceLanes::new(
            Arc::new(Semaphore::new(1)),
            Arc::new(Semaphore::new(1)),
            1,
            1,
        );
        let envelope = SliceEnvelope::new(
            scheduler.slice_deterministic_envelope,
            scheduler.slice_semantic_envelope,
        );
        let slice = Arc::new(slice.clone());
        let mut records = Vec::with_capacity(predicates.len());
        for runner in predicates {
            records.push(
                self.execute_one(
                    slice.clone(),
                    runner.clone(),
                    lanes.clone(),
                    envelope.clone(),
                )
                .await,
            );
        }
        self.apply_semantic_fallbacks(&mut records);
        records.sort_by(|left, right| left.predicate_hash.cmp(&right.predicate_hash));
        PredicateExecutionReport {
            records,
            skipped: Vec::new(),
        }
    }

    /// Select named predicates and their semantic fallback dependencies, then
    /// execute them serially. This is the public-adapter seam: selection,
    /// dependency skips, enforcement membership, budgets, and fallback
    /// composition all stay in the executor.
    pub async fn execute_named_slice_serial(
        &self,
        slice: &Slice,
        predicates: &[Arc<dyn PredicateRunner>],
        requested_names: Option<&BTreeSet<String>>,
        include_semantic: bool,
    ) -> PredicateExecutionReport {
        let directly_selected = predicates
            .iter()
            .filter(|runner| {
                requested_names
                    .map(|names| names.contains(&runner.name()))
                    .unwrap_or(true)
                    && (include_semantic || runner.kind() != PredicateKind::Semantic)
            })
            .map(|runner| runner.hash())
            .collect::<BTreeSet<_>>();
        let by_hash = predicates
            .iter()
            .map(|runner| (runner.hash(), runner.clone()))
            .collect::<BTreeMap<_, _>>();
        let mut selected = directly_selected.clone();
        for runner in predicates {
            if directly_selected.contains(&runner.hash())
                && runner.kind() == PredicateKind::Semantic
            {
                if let Some(fallback_hash) = runner.fallback_hash() {
                    if by_hash.contains_key(&fallback_hash) {
                        selected.insert(fallback_hash);
                    }
                }
            }
        }

        let selected_runners = predicates
            .iter()
            .filter(|runner| selected.contains(&runner.hash()))
            .cloned()
            .collect::<Vec<_>>();
        let skipped = predicates
            .iter()
            .filter(|runner| !selected.contains(&runner.hash()))
            .map(|runner| PredicateExecutionSkip {
                name: runner.name(),
                predicate_hash: runner.hash(),
                kind: runner.kind(),
                reason: if runner.kind() == PredicateKind::Semantic
                    && requested_names
                        .map(|names| names.contains(&runner.name()))
                        .unwrap_or(true)
                    && !include_semantic
                {
                    "semantic predicates require an explicit include_semantic option".to_string()
                } else {
                    "not selected".to_string()
                },
            })
            .collect::<Vec<_>>();

        let mut report = self.execute_slice_serial(slice, &selected_runners).await;
        for record in &mut report.records {
            record.enforced &= directly_selected.contains(&record.predicate_hash);
        }
        report.skipped = skipped;
        report
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
        slices: Vec<(Slice, Vec<Arc<dyn PredicateRunner>>)>,
    ) -> Vec<PredicateExecutionReport> {
        let scheduler = &self.config.scheduler;
        let det_global = Arc::new(Semaphore::new(clamp_permits(
            scheduler.max_deterministic_lanes,
        )));
        let sem_global = Arc::new(Semaphore::new(clamp_permits(scheduler.max_semantic_lanes)));

        let slice_futures = slices
            .into_iter()
            .map(|(slice, predicates)| {
                let executor = self.clone();
                let det_global = det_global.clone();
                let sem_global = sem_global.clone();
                async move {
                    executor
                        .execute_slice_inner(slice, predicates, det_global, sem_global)
                        .await
                }
            })
            .collect::<Vec<_>>();

        futures::future::join_all(slice_futures).await
    }

    async fn execute_slice_inner(
        &self,
        slice: Slice,
        predicates: Vec<Arc<dyn PredicateRunner>>,
        det_global: Arc<Semaphore>,
        sem_global: Arc<Semaphore>,
    ) -> PredicateExecutionReport {
        let scheduler = &self.config.scheduler;
        let slice_rc = Arc::new(slice);
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
                let executor = self.clone();
                let slice = slice_rc.clone();
                let lanes = lanes.clone();
                let envelope = envelope.clone();
                async move { executor.execute_one(slice, runner, lanes, envelope).await }
            })
            .buffer_unordered(buffer)
            .collect::<Vec<_>>()
            .await;

        self.apply_semantic_fallbacks(&mut records);
        records.sort_by(|left, right| left.predicate_hash.cmp(&right.predicate_hash));
        PredicateExecutionReport {
            records,
            skipped: Vec::new(),
        }
    }

    async fn execute_one(
        &self,
        slice: Arc<Slice>,
        runner: Arc<dyn PredicateRunner>,
        lanes: SliceLanes,
        envelope: SliceEnvelope,
    ) -> PredicateExecutionRecord {
        let started = Instant::now();
        let predicate_hash = runner.hash();
        let name = runner.name();
        let kind = runner.kind();
        let first = self
            .run_attempt(slice.clone(), runner.as_ref(), &lanes, &envelope)
            .await;
        let first_hash = hash_result(&first.result);
        let mut result = first.result;
        let mut attempts = 1;
        let mut second_hash = None;
        let semantic_replay_audit = first.semantic_audit;

        if self.config.replay_deterministic
            && kind == PredicateKind::Deterministic
            && !result.is_blocking()
        {
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
            name,
            predicate_hash,
            kind,
            fallback_hash: runner.fallback_hash(),
            fallback_policy: runner.fallback_policy(),
            fallback_diagnostic: runner.fallback_diagnostic(),
            result,
            raw_result: runner.raw_result(),
            enforced: runner.enforced(),
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
        slice: Arc<Slice>,
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
            if let Some(diagnostic) = record.fallback_diagnostic.take() {
                record.result = InvariantResult::block(diagnostic);
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
                    "fallback_unselected",
                    format!(
                        "semantic predicate fallback {} was not selected for evaluation",
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
            if record.fallback_policy == SemanticFallbackPolicy::Enforce {
                record.result = stricter_result(&record.result, fallback_result);
            }
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
    pub name: String,
    #[serde(rename = "hash")]
    pub predicate_hash: PredicateHash,
    pub kind: PredicateKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_hash: Option<PredicateHash>,
    #[serde(default)]
    pub fallback_policy: SemanticFallbackPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_diagnostic: Option<InvariantBlockError>,
    pub result: InvariantResult,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_result: Option<serde_json::Value>,
    /// Whether this record independently contributes to the slice verdict.
    /// Dependency-only advisory fallbacks remain visible but are not enforced.
    #[serde(default = "default_true")]
    pub enforced: bool,
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
    #[serde(default)]
    pub skipped: Vec<PredicateExecutionSkip>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PredicateExecutionSkip {
    pub name: String,
    #[serde(rename = "hash")]
    pub predicate_hash: PredicateHash,
    pub kind: PredicateKind,
    pub reason: String,
}

impl PredicateExecutionReport {
    pub fn invariants_applied(&self) -> Vec<(PredicateHash, InvariantResult)> {
        self.records
            .iter()
            .map(|record| (record.predicate_hash.clone(), record.result.clone()))
            .collect()
    }

    pub fn is_allowed(&self) -> bool {
        self.records
            .iter()
            .filter(|record| record.enforced)
            .all(|record| !record.result.is_blocking())
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

fn default_true() -> bool {
    true
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
    deterministic_used: Arc<Mutex<Duration>>,
    semantic_used: Arc<Mutex<Duration>>,
    deterministic_budget: Duration,
    semantic_budget: Duration,
}

impl SliceEnvelope {
    fn new(deterministic_budget: Duration, semantic_budget: Duration) -> Self {
        Self {
            deterministic_used: Arc::new(Mutex::new(Duration::ZERO)),
            semantic_used: Arc::new(Mutex::new(Duration::ZERO)),
            deterministic_budget,
            semantic_budget,
        }
    }

    fn counter(&self, kind: PredicateKind) -> &Mutex<Duration> {
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
        let used = *self.counter(kind).lock().expect("slice envelope lock");
        Some(budget.saturating_sub(used))
    }

    fn charge(&self, kind: PredicateKind, elapsed: Duration) {
        let mut used = self.counter(kind).lock().expect("slice envelope lock");
        *used = used.saturating_add(elapsed);
    }
}

#[cfg(test)]
#[path = "executor_tests.rs"]
mod tests;
