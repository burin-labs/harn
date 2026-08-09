use super::*;
use crate::flow::{Approval, AtomId, PredicateHash, Slice, SliceId, SliceStatus, TestId};

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

#[async_trait]
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
    calls: Mutex<u64>,
}

#[async_trait]
impl PredicateRunner for DriftingPredicate {
    fn hash(&self) -> PredicateHash {
        PredicateHash::new("drift")
    }

    fn kind(&self) -> PredicateKind {
        PredicateKind::Deterministic
    }

    async fn evaluate(&self, _context: PredicateContext) -> InvariantResult {
        let mut calls = self.calls.lock().expect("drift calls lock");
        let previous = *calls;
        *calls += 1;
        if previous == 0 {
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

struct AdvisorySemanticPredicate;

#[async_trait]
impl PredicateRunner for AdvisorySemanticPredicate {
    fn hash(&self) -> PredicateHash {
        PredicateHash::new("semantic")
    }

    fn kind(&self) -> PredicateKind {
        PredicateKind::Semantic
    }

    fn fallback_hash(&self) -> Option<PredicateHash> {
        Some(PredicateHash::new("fallback"))
    }

    fn fallback_policy(&self) -> SemanticFallbackPolicy {
        SemanticFallbackPolicy::Advisory
    }

    async fn evaluate(&self, _context: PredicateContext) -> InvariantResult {
        InvariantResult::allow()
    }
}

struct InvalidFallbackPredicate {
    code: &'static str,
}

#[async_trait]
impl PredicateRunner for InvalidFallbackPredicate {
    fn hash(&self) -> PredicateHash {
        PredicateHash::new("semantic")
    }

    fn kind(&self) -> PredicateKind {
        PredicateKind::Semantic
    }

    fn fallback_diagnostic(&self) -> Option<InvariantBlockError> {
        Some(InvariantBlockError::new(self.code, "invalid fallback"))
    }

    async fn evaluate(&self, _context: PredicateContext) -> InvariantResult {
        InvariantResult::allow()
    }
}

#[async_trait]
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

#[async_trait]
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
    active: Arc<Mutex<usize>>,
    max_active: Arc<Mutex<usize>>,
}

#[async_trait]
impl PredicateRunner for ParallelProbe {
    fn hash(&self) -> PredicateHash {
        PredicateHash::new(self.hash)
    }

    fn kind(&self) -> PredicateKind {
        self.kind
    }

    async fn evaluate(&self, _context: PredicateContext) -> InvariantResult {
        let active = {
            let mut count = self.active.lock().expect("active probe lock");
            *count += 1;
            *count
        };
        {
            let mut max_active = self.max_active.lock().expect("max active probe lock");
            *max_active = (*max_active).max(active);
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
        *self.active.lock().expect("active probe lock") -= 1;
        InvariantResult::allow()
    }
}

#[tokio::test]
async fn deterministic_predicate_replays_bit_identically() {
    let executor = PredicateExecutor::default();
    let predicates: Vec<Arc<dyn PredicateRunner>> = vec![Arc::new(StaticPredicate {
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
    let predicates: Vec<Arc<dyn PredicateRunner>> = vec![Arc::new(DriftingPredicate {
        calls: Mutex::new(0),
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
    let predicates: Vec<Arc<dyn PredicateRunner>> = vec![Arc::new(StaticPredicate {
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
    let active = Arc::new(Mutex::new(0));
    let max_active = Arc::new(Mutex::new(0));
    let predicates: Vec<Arc<dyn PredicateRunner>> = vec![
        Arc::new(ParallelProbe {
            hash: "parallel-a",
            kind: PredicateKind::Deterministic,
            active: active.clone(),
            max_active: max_active.clone(),
        }),
        Arc::new(ParallelProbe {
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
    assert_eq!(*max_active.lock().expect("max active probe lock"), 2);
}

#[tokio::test]
async fn semantic_predicate_gets_one_cheap_judge_call() {
    let executor = PredicateExecutor::with_cheap_judge(
        PredicateExecutorConfig::default(),
        Arc::new(PassingJudge),
    );
    let predicates: Vec<Arc<dyn PredicateRunner>> = vec![
        Arc::new(SemanticPredicate {
            calls: 2,
            fallback_hash: Some("fallback"),
        }),
        Arc::new(StaticPredicate {
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
    let predicates: Vec<Arc<dyn PredicateRunner>> = vec![
        Arc::new(StaticPredicate {
            hash: "semantic",
            kind: PredicateKind::Semantic,
            fallback_hash: Some("fallback"),
            result: InvariantResult::warn("semantic concern"),
            delay: Duration::ZERO,
        }),
        Arc::new(StaticPredicate {
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
async fn named_selection_includes_fallback_once_without_marking_it_skipped() {
    let executor = PredicateExecutor::default();
    let predicates: Vec<Arc<dyn PredicateRunner>> = vec![
        Arc::new(StaticPredicate {
            hash: "semantic",
            kind: PredicateKind::Semantic,
            fallback_hash: Some("fallback"),
            result: InvariantResult::allow(),
            delay: Duration::ZERO,
        }),
        Arc::new(StaticPredicate {
            hash: "fallback",
            kind: PredicateKind::Deterministic,
            fallback_hash: None,
            result: InvariantResult::allow(),
            delay: Duration::ZERO,
        }),
        Arc::new(StaticPredicate {
            hash: "other",
            kind: PredicateKind::Deterministic,
            fallback_hash: None,
            result: InvariantResult::allow(),
            delay: Duration::ZERO,
        }),
    ];

    let report = executor
        .execute_named_slice_serial(
            &slice(),
            &predicates,
            Some(&BTreeSet::from(["semantic".to_string()])),
            true,
        )
        .await;

    assert_eq!(report.records.len(), 2);
    assert_eq!(report.skipped.len(), 1);
    assert_eq!(report.skipped[0].name, "other");
    let fallback = report
        .records
        .iter()
        .find(|record| record.name == "fallback")
        .unwrap();
    assert!(!fallback.enforced);
}

#[tokio::test]
async fn semantic_fallback_disagreement_selects_stricter_verdict() {
    let executor = PredicateExecutor::default();
    let predicates: Vec<Arc<dyn PredicateRunner>> = vec![
        Arc::new(StaticPredicate {
            hash: "semantic",
            kind: PredicateKind::Semantic,
            fallback_hash: Some("fallback"),
            result: InvariantResult::allow(),
            delay: Duration::ZERO,
        }),
        Arc::new(StaticPredicate {
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
async fn advisory_fallback_is_recorded_without_changing_semantic_verdict() {
    let executor = PredicateExecutor::default();
    let predicates: Vec<Arc<dyn PredicateRunner>> = vec![
        Arc::new(AdvisorySemanticPredicate),
        Arc::new(StaticPredicate {
            hash: "fallback",
            kind: PredicateKind::Deterministic,
            fallback_hash: None,
            result: InvariantResult::block(InvariantBlockError::new(
                "unrelated_advisory",
                "advisory fallback blocked",
            )),
            delay: Duration::ZERO,
        }),
    ];

    let report = executor.execute_slice(&slice(), &predicates).await;

    assert_eq!(report.records.len(), 2);
    let semantic = report
        .records
        .iter()
        .find(|record| record.predicate_hash == PredicateHash::new("semantic"))
        .unwrap();
    assert_eq!(semantic.fallback_policy, SemanticFallbackPolicy::Advisory);
    assert_eq!(semantic.result, InvariantResult::allow());
}

#[tokio::test]
async fn semantic_missing_fallback_blocks() {
    let executor = PredicateExecutor::default();
    let predicates: Vec<Arc<dyn PredicateRunner>> = vec![Arc::new(StaticPredicate {
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
async fn semantic_omitted_fallback_fails_closed_as_unselected() {
    let predicates: Vec<Arc<dyn PredicateRunner>> = vec![Arc::new(StaticPredicate {
        hash: "semantic",
        kind: PredicateKind::Semantic,
        fallback_hash: Some("fallback"),
        result: InvariantResult::allow(),
        delay: Duration::ZERO,
    })];

    let report = PredicateExecutor::default()
        .execute_slice(&slice(), &predicates)
        .await;

    let block = report.records[0].result.block_error().expect("blocked");
    assert_eq!(block.code, "fallback_unselected");
}

#[tokio::test]
async fn semantic_fallback_must_be_deterministic() {
    let predicates: Vec<Arc<dyn PredicateRunner>> = vec![
        Arc::new(StaticPredicate {
            hash: "semantic",
            kind: PredicateKind::Semantic,
            fallback_hash: Some("fallback"),
            result: InvariantResult::allow(),
            delay: Duration::ZERO,
        }),
        Arc::new(StaticPredicate {
            hash: "fallback",
            kind: PredicateKind::Semantic,
            fallback_hash: None,
            result: InvariantResult::allow(),
            delay: Duration::ZERO,
        }),
    ];

    let report = PredicateExecutor::default()
        .execute_slice(&slice(), &predicates)
        .await;
    let semantic = report
        .records
        .iter()
        .find(|record| record.predicate_hash == PredicateHash::new("semantic"))
        .unwrap();
    let block = semantic.result.block_error().expect("blocked");
    assert_eq!(block.code, "fallback_not_deterministic");
}

#[tokio::test]
async fn adapter_fallback_diagnostics_fail_closed_with_stable_codes() {
    for code in ["fallback_unresolved", "fallback_unselected"] {
        let predicates: Vec<Arc<dyn PredicateRunner>> =
            vec![Arc::new(InvalidFallbackPredicate { code })];
        let report = PredicateExecutor::default()
            .execute_slice(&slice(), &predicates)
            .await;
        let block = report.records[0].result.block_error().expect("blocked");
        assert_eq!(block.code, code);
    }
}

#[tokio::test]
async fn semantic_predicate_requires_prebaked_evidence() {
    struct MissingEvidence;

    #[async_trait]
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
        Arc::new(PassingJudge),
    );
    let predicates: Vec<Arc<dyn PredicateRunner>> = vec![
        Arc::new(MissingEvidence),
        Arc::new(StaticPredicate {
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
        Arc::new(PassingJudge),
    );
    let predicates: Vec<Arc<dyn PredicateRunner>> = vec![
        Arc::new(SemanticPredicate {
            calls: 1,
            fallback_hash: Some("fallback"),
        }),
        Arc::new(StaticPredicate {
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
    assert_eq!(audit.prompt_hash, stable_hash(b"judge the case"));
    let expected_evidence_hash = stable_hash(b"pre-baked evidence");
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
    finish_log: Arc<Mutex<Vec<(String, u128)>>>,
    epoch: Instant,
}

#[async_trait]
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
            .lock()
            .expect("finish log lock")
            .push((self.hash.to_string(), self.epoch.elapsed().as_micros()));
        InvariantResult::allow()
    }
}

#[tokio::test]
async fn execute_slices_returns_one_report_per_slice_in_input_order() {
    let executor = PredicateExecutor::default();
    let slice_a = slice_with_id(1);
    let slice_b = slice_with_id(2);

    let predicates_a: Vec<Arc<dyn PredicateRunner>> = vec![Arc::new(StaticPredicate {
        hash: "alpha",
        kind: PredicateKind::Deterministic,
        fallback_hash: None,
        result: InvariantResult::allow(),
        delay: Duration::ZERO,
    })];
    let predicates_b: Vec<Arc<dyn PredicateRunner>> = vec![Arc::new(StaticPredicate {
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
            slice_semantic_envelope: Duration::from_mins(1),
            ..PredicateSchedulerConfig::default()
        },
        ..PredicateExecutorConfig::default()
    };
    let executor = PredicateExecutor::with_cheap_judge(config, Arc::new(PassingJudge));

    let log_a = Arc::new(Mutex::new(Vec::new()));
    let log_b = Arc::new(Mutex::new(Vec::new()));
    let epoch = Instant::now();

    let predicates_a: Vec<Arc<dyn PredicateRunner>> = (0..4)
        .map(|i| -> Arc<dyn PredicateRunner> {
            Arc::new(FinishingProbe {
                hash: ["a0", "a1", "a2", "a3"][i],
                kind: PredicateKind::Semantic,
                delay: Duration::from_millis(20),
                finish_log: log_a.clone(),
                epoch,
            })
        })
        .collect();
    let predicates_b: Vec<Arc<dyn PredicateRunner>> = (0..4)
        .map(|i| -> Arc<dyn PredicateRunner> {
            Arc::new(FinishingProbe {
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

    let log_a_snapshot = log_a.lock().expect("finish log lock").clone();
    let log_b_snapshot = log_b.lock().expect("finish log lock").clone();
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
            slice_semantic_envelope: Duration::from_mins(1),
            ..PredicateSchedulerConfig::default()
        },
        ..PredicateExecutorConfig::default()
    };
    let executor = PredicateExecutor::with_cheap_judge(config, Arc::new(PassingJudge));

    let log = Arc::new(Mutex::new(Vec::new()));
    let epoch = Instant::now();

    let predicates: Vec<Arc<dyn PredicateRunner>> = vec![
        Arc::new(FinishingProbe {
            hash: "slow-semantic",
            kind: PredicateKind::Semantic,
            delay: Duration::from_millis(80),
            finish_log: log.clone(),
            epoch,
        }),
        Arc::new(FinishingProbe {
            hash: "det-1",
            kind: PredicateKind::Deterministic,
            delay: Duration::from_millis(5),
            finish_log: log.clone(),
            epoch,
        }),
        Arc::new(FinishingProbe {
            hash: "det-2",
            kind: PredicateKind::Deterministic,
            delay: Duration::from_millis(5),
            finish_log: log.clone(),
            epoch,
        }),
    ];

    let _ = executor.execute_slice(&slice(), &predicates).await;
    let snapshot = log.lock().expect("finish log lock").clone();
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
    let predicates: Vec<Arc<dyn PredicateRunner>> = vec![
        Arc::new(StaticPredicate {
            hash: "first",
            kind: PredicateKind::Deterministic,
            fallback_hash: None,
            result: InvariantResult::allow(),
            delay: Duration::from_millis(20),
        }),
        Arc::new(StaticPredicate {
            hash: "second",
            kind: PredicateKind::Deterministic,
            fallback_hash: None,
            result: InvariantResult::allow(),
            delay: Duration::ZERO,
        }),
        Arc::new(StaticPredicate {
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
    let executor = PredicateExecutor::with_cheap_judge(config, Arc::new(PassingJudge));

    let predicates: Vec<Arc<dyn PredicateRunner>> = vec![
        Arc::new(StaticPredicate {
            hash: "sem-first",
            kind: PredicateKind::Semantic,
            fallback_hash: Some("sem-fallback"),
            result: InvariantResult::allow(),
            delay: Duration::from_millis(30),
        }),
        Arc::new(StaticPredicate {
            hash: "sem-second",
            kind: PredicateKind::Semantic,
            fallback_hash: Some("sem-fallback"),
            result: InvariantResult::allow(),
            delay: Duration::ZERO,
        }),
        Arc::new(StaticPredicate {
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
    let executor = PredicateExecutor::with_cheap_judge(config, Arc::new(PassingJudge));

    let slice_a_preds: Vec<Arc<dyn PredicateRunner>> = vec![
        Arc::new(StaticPredicate {
            hash: "a-slow",
            kind: PredicateKind::Semantic,
            fallback_hash: Some("a-fallback"),
            result: InvariantResult::allow(),
            delay: Duration::from_millis(30),
        }),
        Arc::new(StaticPredicate {
            hash: "a-second",
            kind: PredicateKind::Semantic,
            fallback_hash: Some("a-fallback"),
            result: InvariantResult::allow(),
            delay: Duration::ZERO,
        }),
        Arc::new(StaticPredicate {
            hash: "a-fallback",
            kind: PredicateKind::Deterministic,
            fallback_hash: None,
            result: InvariantResult::allow(),
            delay: Duration::ZERO,
        }),
    ];
    let slice_b_preds: Vec<Arc<dyn PredicateRunner>> = vec![
        Arc::new(StaticPredicate {
            hash: "b-fast",
            kind: PredicateKind::Semantic,
            fallback_hash: Some("b-fallback"),
            result: InvariantResult::allow(),
            delay: Duration::from_millis(2),
        }),
        Arc::new(StaticPredicate {
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
    let make_predicates = || -> Vec<Arc<dyn PredicateRunner>> {
        vec![
            Arc::new(StaticPredicate {
                hash: "z-last",
                kind: PredicateKind::Deterministic,
                fallback_hash: None,
                result: InvariantResult::allow(),
                delay: Duration::from_millis(15),
            }),
            Arc::new(StaticPredicate {
                hash: "a-first",
                kind: PredicateKind::Deterministic,
                fallback_hash: None,
                result: InvariantResult::allow(),
                delay: Duration::ZERO,
            }),
            Arc::new(StaticPredicate {
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
