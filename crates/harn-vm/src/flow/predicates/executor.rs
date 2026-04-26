//! Budgeted executor for Flow invariant predicates.
//!
//! The executor is intentionally small: callers provide predicate runners, and
//! this module owns scheduling, per-kind budgets, semantic cheap-judge limits,
//! and deterministic replay drift detection.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::compose::verdict_strictness;
use crate::flow::{InvariantBlockError, InvariantResult, PredicateHash, Slice};

const DEFAULT_DETERMINISTIC_BUDGET: Duration = Duration::from_millis(50);
const DEFAULT_SEMANTIC_BUDGET: Duration = Duration::from_secs(2);
const DEFAULT_SEMANTIC_TOKEN_CAP: u64 = 1024;

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

/// Executor configuration.
#[derive(Clone, Debug)]
pub struct PredicateExecutorConfig {
    pub deterministic_budget: Duration,
    pub semantic_budget: Duration,
    pub semantic_token_cap: u64,
    /// Maximum number of predicates polled concurrently for one slice.
    pub max_parallel_predicates: usize,
}

impl Default for PredicateExecutorConfig {
    fn default() -> Self {
        Self {
            deterministic_budget: DEFAULT_DETERMINISTIC_BUDGET,
            semantic_budget: DEFAULT_SEMANTIC_BUDGET,
            semantic_token_cap: DEFAULT_SEMANTIC_TOKEN_CAP,
            max_parallel_predicates: usize::MAX,
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

    pub async fn execute_slice(
        &self,
        slice: &Slice,
        predicates: &[Rc<dyn PredicateRunner>],
    ) -> PredicateExecutionReport {
        let parallelism = self
            .config
            .max_parallel_predicates
            .max(1)
            .min(predicates.len().max(1));
        let slice = Rc::new(slice.clone());
        let records = futures::stream::iter(predicates.iter())
            .map(|runner| self.execute_one(slice.clone(), runner.as_ref()))
            .buffer_unordered(parallelism)
            .collect::<Vec<_>>()
            .await;

        let mut records = records;
        self.apply_semantic_fallbacks(&mut records);
        records.sort_by(|left, right| left.predicate_hash.cmp(&right.predicate_hash));
        PredicateExecutionReport { records }
    }

    async fn execute_one(
        &self,
        slice: Rc<Slice>,
        runner: &dyn PredicateRunner,
    ) -> PredicateExecutionRecord {
        let started = Instant::now();
        let predicate_hash = runner.hash();
        let kind = runner.kind();
        let first = self.run_attempt(slice.clone(), runner).await;
        let first_hash = hash_result(&first.result);
        let mut result = first.result;
        let mut attempts = 1;
        let mut second_hash = None;
        let semantic_replay_audit = first.semantic_audit;

        if kind == PredicateKind::Deterministic && !result.is_blocking() {
            let second = self.run_attempt(slice, runner).await;
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
    ) -> PredicateAttempt {
        let kind = runner.kind();
        let timeout = match kind {
            PredicateKind::Deterministic => self.config.deterministic_budget,
            PredicateKind::Semantic => self.config.semantic_budget,
        };
        let context = PredicateContext::new(
            slice,
            kind,
            runner.evidence(),
            self.cheap_judge.clone(),
            self.config.semantic_token_cap,
            Arc::new(AtomicBool::new(false)),
        );
        match tokio::time::timeout(timeout, runner.evaluate(context.clone())).await {
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
        }
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

struct PredicateAttempt {
    result: InvariantResult,
    semantic_audit: Option<SemanticReplayAuditMetadata>,
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
        active: Rc<Cell<usize>>,
        max_active: Rc<Cell<usize>>,
    }

    #[async_trait(?Send)]
    impl PredicateRunner for ParallelProbe {
        fn hash(&self) -> PredicateHash {
            PredicateHash::new(self.hash)
        }

        fn kind(&self) -> PredicateKind {
            PredicateKind::Semantic
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
                active: active.clone(),
                max_active: max_active.clone(),
            }),
            Rc::new(ParallelProbe {
                hash: "parallel-b",
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
}
