use super::*;

use crate::event_log::{AnyEventLog, MemoryEventLog};
use crate::triggers::{
    event::{GenericWebhookPayload, KnownProviderPayload},
    scheduler::{self, SchedulerStrategy},
    ProviderId, ProviderPayload, SignatureStatus, TraceId, TriggerEvent,
};

fn test_event(id: &str) -> TriggerEvent {
    TriggerEvent {
        id: crate::triggers::TriggerEventId(id.to_string()),
        provider: ProviderId::from("github"),
        kind: "issues.opened".to_string(),
        trace_id: TraceId("trace-test".to_string()),
        dedupe_key: id.to_string(),
        tenant_id: None,
        headers: BTreeMap::new(),
        batch: None,
        raw_body: None,
        provider_payload: ProviderPayload::Known(KnownProviderPayload::Webhook(
            GenericWebhookPayload {
                source: Some("worker-queue-test".to_string()),
                content_type: Some("application/json".to_string()),
                raw: serde_json::json!({"id": id}),
            },
        )),
        signature_status: SignatureStatus::Verified,
        received_at: time::OffsetDateTime::now_utc(),
        occurred_at: None,
        dedupe_claimed: false,
    }
}

fn test_job(
    queue: &str,
    trigger_id: &str,
    event_id: &str,
    priority: WorkerQueuePriority,
) -> WorkerQueueJob {
    WorkerQueueJob {
        queue: queue.to_string(),
        trigger_id: trigger_id.to_string(),
        binding_key: format!("{trigger_id}@v1"),
        binding_version: 1,
        event: test_event(event_id),
        replay_of_event_id: None,
        priority,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn enqueue_and_summarize_queue() {
    let log = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(32)));
    let queue = WorkerQueue::new(log);
    queue
        .enqueue(&test_job(
            "triage",
            "incoming-review-task",
            "evt-1",
            WorkerQueuePriority::Normal,
        ))
        .await
        .unwrap();
    let summaries = queue.queue_summaries().await.unwrap();
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].queue, "triage");
    assert_eq!(summaries[0].ready, 1);
    assert_eq!(summaries[0].in_flight, 0);
}

#[tokio::test(flavor = "current_thread")]
async fn claim_and_ack_remove_job_from_ready_pool() {
    let log = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(32)));
    let queue = WorkerQueue::new(log);
    queue
        .enqueue(&test_job(
            "triage",
            "incoming-review-task",
            "evt-1",
            WorkerQueuePriority::Normal,
        ))
        .await
        .unwrap();
    let claimed = queue
        .claim_next("triage", "consumer-a", StdDuration::from_mins(1))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        claimed.scheduling.decision,
        WorkerQueueSchedulingDecision::Priority
    );
    assert_eq!(claimed.scheduling.priority, WorkerQueuePriority::Normal);
    assert_eq!(claimed.scheduling.fairness_key, "_no_tenant");
    assert!(claimed.scheduling.promotion_deadline_at_ms > Some(claimed.scheduling.enqueued_at_ms));
    let before_ack = queue.queue_state("triage").await.unwrap();
    assert_eq!(before_ack.summary(now_ms()).ready, 0);
    assert_eq!(before_ack.summary(now_ms()).in_flight, 1);
    queue
        .append_response(
            "triage",
            &WorkerQueueResponseRecord {
                queue: "triage".to_string(),
                job_event_id: claimed.handle.job_event_id,
                consumer_id: "consumer-a".to_string(),
                handled_at_ms: now_ms(),
                outcome: Some(DispatchOutcome {
                    trigger_id: "incoming-review-task".to_string(),
                    binding_key: "incoming-review-task@v1".to_string(),
                    event_id: "evt-1".to_string(),
                    attempt_count: 1,
                    status: super::super::DispatchStatus::Succeeded,
                    handler_kind: "local".to_string(),
                    target_uri: "handlers::on_review".to_string(),
                    replay_of_event_id: None,
                    result: Some(serde_json::json!({"ok": true})),
                    error: None,
                }),
                error: None,
            },
        )
        .await
        .unwrap();
    queue.ack_claim(&claimed.handle).await.unwrap();
    let after_ack = queue.queue_state("triage").await.unwrap();
    let summary = after_ack.summary(now_ms());
    assert_eq!(summary.ready, 0);
    assert_eq!(summary.in_flight, 0);
    assert_eq!(summary.acked, 1);
    assert_eq!(summary.responses, 1);
}

#[tokio::test(flavor = "current_thread")]
async fn claim_exclusion_skips_attempted_job_without_hiding_other_ready_work() {
    let log = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(32)));
    let queue = WorkerQueue::new(log);
    let first = queue
        .enqueue(&test_job(
            "triage",
            "incoming-review-task",
            "evt-1",
            WorkerQueuePriority::Normal,
        ))
        .await
        .unwrap();
    let second = queue
        .enqueue(&test_job(
            "triage",
            "incoming-review-task",
            "evt-2",
            WorkerQueuePriority::Normal,
        ))
        .await
        .unwrap();

    let excluded = BTreeSet::from([first.job_event_id]);
    let eligible = queue
        .claim_next_excluding("triage", "consumer-a", StdDuration::from_mins(1), &excluded)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(eligible.handle.job_event_id, second.job_event_id);

    let previously_excluded = queue
        .claim_next("triage", "consumer-b", StdDuration::from_mins(1))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(previously_excluded.handle.job_event_id, first.job_event_id);
}

#[tokio::test(flavor = "current_thread")]
async fn ack_job_acknowledges_without_active_claim() {
    let log = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(32)));
    let queue = WorkerQueue::new(log);
    let receipt = queue
        .enqueue(&test_job(
            "triage",
            "incoming-review-task",
            "evt-1",
            WorkerQueuePriority::Normal,
        ))
        .await
        .unwrap();

    assert!(queue
        .ack_job("triage", receipt.job_event_id, "pipeline_lifecycle")
        .await
        .unwrap());
    let state = queue.queue_state("triage").await.unwrap();
    let summary = state.summary(now_ms());
    assert_eq!(summary.ready, 0);
    assert_eq!(summary.acked, 1);
    assert!(
        !queue
            .ack_job("triage", receipt.job_event_id, "pipeline_lifecycle")
            .await
            .unwrap(),
        "already acknowledged jobs should not produce a second settlement"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn expired_claim_allows_reclaim() {
    let log = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(32)));
    let queue = WorkerQueue::new(log.clone());
    let receipt = queue
        .enqueue(&test_job(
            "triage",
            "incoming-review-task",
            "evt-1",
            WorkerQueuePriority::Normal,
        ))
        .await
        .unwrap();
    let expired_claim = WorkerQueueClaimRecord {
        job_event_id: receipt.job_event_id,
        claim_id: "expired-claim".to_string(),
        consumer_id: "consumer-a".to_string(),
        claimed_at_ms: now_ms().saturating_sub(2),
        expires_at_ms: now_ms().saturating_sub(1),
        scheduling: None,
    };
    log.append(
        &claims_topic("triage").unwrap(),
        LogEvent::new("job_claimed", serde_json::to_value(&expired_claim).unwrap()),
    )
    .await
    .unwrap();
    let second = queue
        .claim_next("triage", "consumer-b", StdDuration::from_mins(1))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(second.job.event.id.0, "evt-1");
    assert_ne!(second.handle.claim_id, expired_claim.claim_id);
    assert_eq!(second.handle.consumer_id, "consumer-b");
}

#[tokio::test(flavor = "current_thread")]
async fn high_priority_and_aged_normal_are_selected_first() {
    let log = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(32)));
    let queue = WorkerQueue::new(log.clone());

    let catalog_topic = Topic::new(WORKER_QUEUE_CATALOG_TOPIC).unwrap();
    log.append(
        &catalog_topic,
        LogEvent::new("queue_seen", serde_json::json!({"queue":"triage"})),
    )
    .await
    .unwrap();

    let topic = job_topic("triage").unwrap();
    let mut old_normal = LogEvent::new(
        "trigger_dispatch",
        serde_json::to_value(test_job(
            "triage",
            "incoming-review-task",
            "evt-old-normal",
            WorkerQueuePriority::Normal,
        ))
        .unwrap(),
    );
    old_normal.occurred_at_ms = now_ms() - scheduling::NORMAL_PROMOTION_AGE_MS - 1_000;
    log.append(&topic, old_normal).await.unwrap();

    let high = LogEvent::new(
        "trigger_dispatch",
        serde_json::to_value(test_job(
            "triage",
            "incoming-review-task",
            "evt-high",
            WorkerQueuePriority::High,
        ))
        .unwrap(),
    );
    log.append(&topic, high).await.unwrap();

    let claimed = queue
        .claim_next("triage", "consumer-a", StdDuration::from_mins(1))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed.job.event.id.0, "evt-old-normal");
}

fn tenant_event(id: &str, tenant: &str) -> TriggerEvent {
    let mut event = test_event(id);
    event.tenant_id = Some(crate::triggers::TenantId::new(tenant));
    event
}

fn tenant_job(
    queue: &str,
    trigger_id: &str,
    event_id: &str,
    tenant: &str,
    priority: WorkerQueuePriority,
) -> WorkerQueueJob {
    WorkerQueueJob {
        queue: queue.to_string(),
        trigger_id: trigger_id.to_string(),
        binding_key: format!("{trigger_id}@v1"),
        binding_version: 1,
        event: tenant_event(event_id, tenant),
        replay_of_event_id: None,
        priority,
    }
}

async fn ack_and_respond(queue: &WorkerQueue, queue_name: &str, claim: &ClaimedWorkerJob) {
    queue
        .append_response(
            queue_name,
            &WorkerQueueResponseRecord {
                queue: queue_name.to_string(),
                job_event_id: claim.handle.job_event_id,
                consumer_id: claim.handle.consumer_id.clone(),
                handled_at_ms: now_ms(),
                outcome: Some(DispatchOutcome {
                    trigger_id: claim.job.trigger_id.clone(),
                    binding_key: claim.job.binding_key.clone(),
                    event_id: claim.job.event.id.0.clone(),
                    attempt_count: 1,
                    status: super::super::DispatchStatus::Succeeded,
                    handler_kind: "local".to_string(),
                    target_uri: "test::handler".to_string(),
                    replay_of_event_id: None,
                    result: None,
                    error: None,
                }),
                error: None,
            },
        )
        .await
        .unwrap();
    queue.ack_claim(&claim.handle).await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn drr_policy_rotates_across_tenants_through_claim_next() {
    let log = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(256)));
    let queue = WorkerQueue::with_policy(
        log,
        SchedulerPolicy::deficit_round_robin(scheduler::FairnessKey::Tenant),
    );

    // Tenant A enqueues 8 jobs before tenant B enqueues a single job.
    for idx in 0..8 {
        queue
            .enqueue(&tenant_job(
                "triage",
                "trigger",
                &format!("a-{idx}"),
                "tenant-a",
                WorkerQueuePriority::Normal,
            ))
            .await
            .unwrap();
    }
    queue
        .enqueue(&tenant_job(
            "triage",
            "trigger",
            "b-1",
            "tenant-b",
            WorkerQueuePriority::Normal,
        ))
        .await
        .unwrap();

    // Claim+ack 4 jobs back-to-back. Under FIFO, tenant B would never be
    // touched. Under fair-share, B must be served within the first two
    // claims.
    let mut tenants_seen = Vec::new();
    for n in 0..4 {
        let consumer = format!("c-{n}");
        let claim = queue
            .claim_next("triage", &consumer, StdDuration::from_mins(1))
            .await
            .unwrap()
            .expect("queue should still have ready jobs");
        tenants_seen.push(
            claim
                .job
                .event
                .tenant_id
                .as_ref()
                .map(|t| t.0.clone())
                .unwrap_or_default(),
        );
        ack_and_respond(&queue, "triage", &claim).await;
    }

    let saw_b = tenants_seen.iter().any(|t| t == "tenant-b");
    assert!(
        saw_b,
        "tenant-b should have been served within the first 4 claims, got {tenants_seen:?}",
    );
}

#[tokio::test(flavor = "current_thread")]
async fn fifo_policy_preserves_legacy_behavior() {
    let log = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(64)));
    let queue = WorkerQueue::with_policy(log, SchedulerPolicy::fifo());

    // Fill queue with tenant-a jobs first, then a single tenant-b job.
    for idx in 0..4 {
        queue
            .enqueue(&tenant_job(
                "triage",
                "trigger",
                &format!("a-{idx}"),
                "tenant-a",
                WorkerQueuePriority::Normal,
            ))
            .await
            .unwrap();
    }
    queue
        .enqueue(&tenant_job(
            "triage",
            "trigger",
            "b-1",
            "tenant-b",
            WorkerQueuePriority::Normal,
        ))
        .await
        .unwrap();

    // FIFO must drain all of tenant-a before touching tenant-b.
    let first = queue
        .claim_next("triage", "c-0", StdDuration::from_mins(1))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.job.event.tenant_id.unwrap().0, "tenant-a");
}

#[tokio::test(flavor = "current_thread")]
async fn inspect_queue_reports_per_tenant_fairness_state() {
    let log = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(64)));
    let queue = WorkerQueue::with_policy(
        log,
        SchedulerPolicy::deficit_round_robin(scheduler::FairnessKey::Tenant)
            .with_weight("tenant-a", 2)
            .with_weight("tenant-b", 1),
    );

    for idx in 0..3 {
        queue
            .enqueue(&tenant_job(
                "triage",
                "trigger",
                &format!("a-{idx}"),
                "tenant-a",
                WorkerQueuePriority::Normal,
            ))
            .await
            .unwrap();
    }
    queue
        .enqueue(&tenant_job(
            "triage",
            "trigger",
            "b-1",
            "tenant-b",
            WorkerQueuePriority::Normal,
        ))
        .await
        .unwrap();

    for n in 0..2 {
        let consumer = format!("c-{n}");
        let claim = queue
            .claim_next("triage", &consumer, StdDuration::from_mins(1))
            .await
            .unwrap()
            .unwrap();
        ack_and_respond(&queue, "triage", &claim).await;
    }

    let snap = queue.inspect_queue("triage").await.unwrap();
    assert_eq!(snap.scheduler.strategy, "drr");
    assert_eq!(snap.scheduler.fairness_key, "tenant");
    assert!(snap
        .scheduler
        .keys
        .iter()
        .any(|k| k.fairness_key == "tenant-a"));
    let weights: BTreeMap<String, u32> = snap
        .scheduler
        .keys
        .iter()
        .map(|k| (k.fairness_key.clone(), k.weight))
        .collect();
    assert_eq!(weights.get("tenant-a").copied(), Some(2));
    assert_eq!(weights.get("tenant-b").copied(), Some(1));
}

#[tokio::test(flavor = "current_thread")]
async fn drr_with_max_concurrent_per_key_throttles_hot_tenant() {
    let log = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(128)));
    let queue = WorkerQueue::with_policy(
        log,
        SchedulerPolicy::deficit_round_robin(scheduler::FairnessKey::Tenant)
            .with_max_concurrent_per_key(1),
    );

    for idx in 0..4 {
        queue
            .enqueue(&tenant_job(
                "triage",
                "trigger",
                &format!("a-{idx}"),
                "tenant-a",
                WorkerQueuePriority::Normal,
            ))
            .await
            .unwrap();
    }
    queue
        .enqueue(&tenant_job(
            "triage",
            "trigger",
            "b-1",
            "tenant-b",
            WorkerQueuePriority::Normal,
        ))
        .await
        .unwrap();

    let first = queue
        .claim_next("triage", "consumer-a", StdDuration::from_mins(1))
        .await
        .unwrap()
        .unwrap();
    // Without releasing the first claim, the second pick must skip the
    // capped tenant-a and serve tenant-b instead.
    let second = queue
        .claim_next("triage", "consumer-b", StdDuration::from_mins(1))
        .await
        .unwrap()
        .unwrap();
    let pair = [
        first.job.event.tenant_id.clone().unwrap().0,
        second.job.event.tenant_id.unwrap().0,
    ];
    assert!(
        pair.contains(&"tenant-a".to_string()) && pair.contains(&"tenant-b".to_string()),
        "max_concurrent_per_key=1 must release tenant-b within two claims, got {pair:?}",
    );
}

#[test]
fn from_env_parses_drr_policy_from_lookup() {
    let lookup = |name: &str| -> Option<String> {
        match name {
            "HARN_SCHEDULER_STRATEGY" => Some("drr".to_string()),
            "HARN_SCHEDULER_FAIRNESS_KEY" => Some("tenant-and-binding".to_string()),
            "HARN_SCHEDULER_QUANTUM" => Some("3".to_string()),
            "HARN_SCHEDULER_STARVATION_AGE_MS" => Some("750".to_string()),
            "HARN_SCHEDULER_MAX_CONCURRENT_PER_KEY" => Some("4".to_string()),
            "HARN_SCHEDULER_DEFAULT_WEIGHT" => Some("2".to_string()),
            "HARN_SCHEDULER_WEIGHTS" => Some("tenant-a:5,tenant-b:1, : ,bad:abc".to_string()),
            _ => None,
        }
    };
    let policy = SchedulerPolicy::from_env_lookup(lookup);
    match policy.strategy {
        SchedulerStrategy::DeficitRoundRobin {
            quantum,
            starvation_age_ms,
        } => {
            assert_eq!(quantum, 3);
            assert_eq!(starvation_age_ms, Some(750));
        }
        other => panic!("expected DRR strategy, got {other:?}"),
    }
    assert_eq!(
        policy.fairness_key,
        scheduler::FairnessKey::TenantAndBinding
    );
    assert_eq!(policy.max_concurrent_per_key, 4);
    assert_eq!(policy.default_weight, 2);
    assert_eq!(policy.weight_for("tenant-a"), 5);
    assert_eq!(policy.weight_for("tenant-b"), 1);
    // Unknown key falls back to default_weight.
    assert_eq!(policy.weight_for("tenant-c"), 2);
}

#[test]
fn from_env_defaults_to_fifo_when_missing() {
    let policy = SchedulerPolicy::from_env_lookup(|_| None);
    assert!(matches!(policy.strategy, SchedulerStrategy::Fifo));
}
