use super::*;

use crate::event_log::{AnyEventLog, MemoryEventLog};
use crate::triggers::{
    event::{GenericWebhookPayload, KnownProviderPayload},
    scheduler::{FairnessKey, SchedulerPolicy},
    ProviderId, ProviderPayload, SignatureStatus, TenantId, TraceId, TriggerEvent,
};

fn test_job(event_id: &str, tenant: &str) -> WorkerQueueJob {
    WorkerQueueJob {
        queue: "triage".to_string(),
        trigger_id: "incoming-review-task".to_string(),
        binding_key: "incoming-review-task@v1".to_string(),
        binding_version: 1,
        event: TriggerEvent {
            id: crate::triggers::TriggerEventId(event_id.to_string()),
            provider: ProviderId::from("github"),
            kind: "issues.opened".to_string(),
            trace_id: TraceId(format!("trace-{event_id}")),
            dedupe_key: event_id.to_string(),
            tenant_id: Some(TenantId::new(tenant)),
            headers: BTreeMap::new(),
            batch: None,
            raw_body: None,
            provider_payload: ProviderPayload::Known(KnownProviderPayload::Webhook(
                GenericWebhookPayload {
                    source: Some("worker-queue-exclusion-test".to_string()),
                    content_type: Some("application/json".to_string()),
                    raw: serde_json::json!({"id": event_id}),
                },
            )),
            signature_status: SignatureStatus::Verified,
            received_at: time::OffsetDateTime::now_utc(),
            occurred_at: None,
            dedupe_claimed: false,
        },
        replay_of_event_id: None,
        priority: WorkerQueuePriority::Normal,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn exclusions_preserve_fairness_and_later_reclaim() {
    let log = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(64)));
    let queue = WorkerQueue::with_policy(
        log,
        SchedulerPolicy::deficit_round_robin(FairnessKey::Tenant),
    );
    let attempted = queue
        .enqueue(&test_job("attempted", "tenant-a"))
        .await
        .unwrap();
    let other_a = queue
        .enqueue(&test_job("other-a", "tenant-a"))
        .await
        .unwrap();
    let other_b = queue
        .enqueue(&test_job("other-b", "tenant-b"))
        .await
        .unwrap();
    let excluded = BTreeSet::from([attempted.job_event_id, other_a.job_event_id]);

    let eligible = queue
        .claim_next_excluding("triage", "same-drain", StdDuration::from_mins(1), &excluded)
        .await
        .unwrap()
        .expect("another fairness key remains eligible");
    assert_eq!(eligible.handle.job_event_id, other_b.job_event_id);
    queue.ack_claim(&eligible.handle).await.unwrap();

    let later = queue
        .claim_next("triage", "later-drain", StdDuration::from_mins(1))
        .await
        .unwrap()
        .expect("exclusion must not alter durable queue eligibility");
    assert!(excluded.contains(&later.handle.job_event_id));
}

#[tokio::test(flavor = "current_thread")]
async fn released_attempt_is_not_reclaimed_by_the_same_drain() {
    let log = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(32)));
    let queue = WorkerQueue::new(log);
    let receipt = queue
        .enqueue(&test_job("deferred", "tenant-a"))
        .await
        .unwrap();
    let first = queue
        .claim_next("triage", "same-drain", StdDuration::from_mins(1))
        .await
        .unwrap()
        .unwrap();
    queue
        .release_claim(&first.handle, "deferred")
        .await
        .unwrap();

    let excluded = BTreeSet::from([receipt.job_event_id]);
    assert!(
        queue
            .claim_next_excluding("triage", "same-drain", StdDuration::from_mins(1), &excluded,)
            .await
            .unwrap()
            .is_none(),
        "one drain invocation must not retry an attempted job"
    );
    assert!(
        queue
            .claim_next("triage", "later-drain", StdDuration::from_mins(1))
            .await
            .unwrap()
            .is_some(),
        "a later drain must retain reclaim eligibility"
    );
}
