use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration as StdDuration;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::event_log::{
    sanitize_topic_component, AnyEventLog, EventLog, LogError, LogEvent, Topic,
};

use super::{DispatchOutcome, TriggerEvent};

pub const WORKER_QUEUE_CATALOG_TOPIC: &str = "worker.queues";
const WORKER_QUEUE_CLAIMS_SUFFIX: &str = ".claims";
const WORKER_QUEUE_RESPONSES_SUFFIX: &str = ".responses";
const NORMAL_PROMOTION_AGE_MS: i64 = 15 * 60 * 1000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkerQueuePriority {
    High,
    #[default]
    Normal,
    Low,
}

impl WorkerQueuePriority {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Normal => "normal",
            Self::Low => "low",
        }
    }

    fn effective_rank(self, enqueued_at_ms: i64, now_ms: i64) -> u8 {
        match self {
            Self::High => 0,
            Self::Normal if now_ms.saturating_sub(enqueued_at_ms) >= NORMAL_PROMOTION_AGE_MS => 0,
            Self::Normal => 1,
            Self::Low => 2,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkerQueueJob {
    pub queue: String,
    pub trigger_id: String,
    pub binding_key: String,
    pub binding_version: u32,
    pub event: TriggerEvent,
    #[serde(default)]
    pub replay_of_event_id: Option<String>,
    #[serde(default)]
    pub priority: WorkerQueuePriority,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerQueueEnqueueReceipt {
    pub queue: String,
    pub job_event_id: u64,
    pub response_topic: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerQueueClaimHandle {
    pub queue: String,
    pub job_event_id: u64,
    pub claim_id: String,
    pub consumer_id: String,
    pub expires_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClaimedWorkerJob {
    pub handle: WorkerQueueClaimHandle,
    pub job: WorkerQueueJob,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkerQueueResponseRecord {
    pub queue: String,
    pub job_event_id: u64,
    pub consumer_id: String,
    pub handled_at_ms: i64,
    pub outcome: Option<DispatchOutcome>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerQueueSummary {
    pub queue: String,
    pub ready: usize,
    pub in_flight: usize,
    pub acked: usize,
    pub purged: usize,
    pub responses: usize,
    pub oldest_unclaimed_age_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkerQueueJobState {
    pub job_event_id: u64,
    pub enqueued_at_ms: i64,
    pub job: WorkerQueueJob,
    pub active_claim: Option<WorkerQueueClaimHandle>,
    pub acked: bool,
    pub purged: bool,
}

impl WorkerQueueJobState {
    pub fn is_ready(&self) -> bool {
        !self.acked && !self.purged && self.active_claim.is_none()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WorkerQueueState {
    pub queue: String,
    pub responses: Vec<WorkerQueueResponseRecord>,
    pub jobs: Vec<WorkerQueueJobState>,
}

impl WorkerQueueState {
    pub fn summary(&self, now_ms: i64) -> WorkerQueueSummary {
        let ready = self.jobs.iter().filter(|job| job.is_ready()).count();
        let in_flight = self
            .jobs
            .iter()
            .filter(|job| !job.acked && !job.purged && job.active_claim.is_some())
            .count();
        let acked = self.jobs.iter().filter(|job| job.acked).count();
        let purged = self.jobs.iter().filter(|job| job.purged).count();
        let oldest_unclaimed_age_ms = self
            .jobs
            .iter()
            .filter(|job| job.is_ready())
            .map(|job| now_ms.saturating_sub(job.enqueued_at_ms).max(0) as u64)
            .max();
        WorkerQueueSummary {
            queue: self.queue.clone(),
            ready,
            in_flight,
            acked,
            purged,
            responses: self.responses.len(),
            oldest_unclaimed_age_ms,
        }
    }

    fn next_ready_job(&self, now_ms: i64) -> Option<&WorkerQueueJobState> {
        self.jobs
            .iter()
            .filter(|job| job.is_ready())
            .min_by_key(|job| {
                (
                    job.job.priority.effective_rank(job.enqueued_at_ms, now_ms),
                    job.enqueued_at_ms,
                    job.job_event_id,
                )
            })
    }

    fn active_claim_for(&self, job_event_id: u64) -> Option<&WorkerQueueClaimHandle> {
        self.jobs
            .iter()
            .find(|job| job.job_event_id == job_event_id)
            .and_then(|job| job.active_claim.as_ref())
    }
}

#[derive(Clone)]
pub struct WorkerQueue {
    event_log: Arc<AnyEventLog>,
}

impl WorkerQueue {
    pub fn new(event_log: Arc<AnyEventLog>) -> Self {
        Self { event_log }
    }

    pub async fn enqueue(
        &self,
        job: &WorkerQueueJob,
    ) -> Result<WorkerQueueEnqueueReceipt, LogError> {
        let queue = job.queue.trim();
        if queue.is_empty() {
            return Err(LogError::Config(
                "worker queue name cannot be empty".to_string(),
            ));
        }
        let queue_name = queue.to_string();
        let catalog_topic = Topic::new(WORKER_QUEUE_CATALOG_TOPIC)
            .expect("static worker queue catalog topic should always be valid");
        self.event_log
            .append(
                &catalog_topic,
                LogEvent::new(
                    "queue_seen",
                    serde_json::to_value(WorkerQueueCatalogRecord {
                        queue: queue_name.clone(),
                    })
                    .map_err(|error| LogError::Serde(error.to_string()))?,
                ),
            )
            .await?;

        let job_topic = job_topic(&queue_name)?;
        let mut headers = BTreeMap::new();
        headers.insert("queue".to_string(), queue_name.clone());
        headers.insert("trigger_id".to_string(), job.trigger_id.clone());
        headers.insert("binding_key".to_string(), job.binding_key.clone());
        headers.insert("event_id".to_string(), job.event.id.0.clone());
        headers.insert("priority".to_string(), job.priority.as_str().to_string());
        let job_event_id = self
            .event_log
            .append(
                &job_topic,
                LogEvent::new(
                    "trigger_dispatch",
                    serde_json::to_value(job)
                        .map_err(|error| LogError::Serde(error.to_string()))?,
                )
                .with_headers(headers),
            )
            .await?;
        Ok(WorkerQueueEnqueueReceipt {
            queue: queue_name.clone(),
            job_event_id,
            response_topic: response_topic_name(&queue_name),
        })
    }

    pub async fn known_queues(&self) -> Result<Vec<String>, LogError> {
        let topic = Topic::new(WORKER_QUEUE_CATALOG_TOPIC)
            .expect("static worker queue catalog topic should always be valid");
        let events = self.event_log.read_range(&topic, None, usize::MAX).await?;
        let mut queues = BTreeSet::new();
        for (_, event) in events {
            if event.kind != "queue_seen" {
                continue;
            }
            let record: WorkerQueueCatalogRecord = serde_json::from_value(event.payload)
                .map_err(|error| LogError::Serde(error.to_string()))?;
            if !record.queue.trim().is_empty() {
                queues.insert(record.queue);
            }
        }
        Ok(queues.into_iter().collect())
    }

    pub async fn queue_state(&self, queue: &str) -> Result<WorkerQueueState, LogError> {
        let queue_name = queue.trim();
        if queue_name.is_empty() {
            return Err(LogError::Config(
                "worker queue name cannot be empty".to_string(),
            ));
        }
        let now_ms = now_ms();
        let job_events = self
            .event_log
            .read_range(&job_topic(queue_name)?, None, usize::MAX)
            .await?;
        let claim_events = self
            .event_log
            .read_range(&claims_topic(queue_name)?, None, usize::MAX)
            .await?;
        let response_events = self
            .event_log
            .read_range(&responses_topic(queue_name)?, None, usize::MAX)
            .await?;

        let mut jobs = BTreeMap::<u64, WorkerQueueJobStateInternal>::new();
        for (job_event_id, event) in job_events {
            if event.kind != "trigger_dispatch" {
                continue;
            }
            let job: WorkerQueueJob = serde_json::from_value(event.payload)
                .map_err(|error| LogError::Serde(error.to_string()))?;
            jobs.insert(
                job_event_id,
                WorkerQueueJobStateInternal {
                    job_event_id,
                    enqueued_at_ms: event.occurred_at_ms,
                    job,
                    active_claim: None,
                    acked: false,
                    purged: false,
                    seen_claim_ids: BTreeSet::new(),
                },
            );
        }

        for (_, event) in claim_events {
            match event.kind.as_str() {
                "job_claimed" => {
                    let claim: WorkerQueueClaimRecord = serde_json::from_value(event.payload)
                        .map_err(|error| LogError::Serde(error.to_string()))?;
                    let Some(job) = jobs.get_mut(&claim.job_event_id) else {
                        continue;
                    };
                    if job.acked || job.purged {
                        continue;
                    }
                    job.seen_claim_ids.insert(claim.claim_id.clone());
                    let can_take = job
                        .active_claim
                        .as_ref()
                        .is_none_or(|active| active.expires_at_ms <= claim.claimed_at_ms);
                    if can_take {
                        job.active_claim = Some(WorkerQueueClaimHandle {
                            queue: queue_name.to_string(),
                            job_event_id: claim.job_event_id,
                            claim_id: claim.claim_id,
                            consumer_id: claim.consumer_id,
                            expires_at_ms: claim.expires_at_ms,
                        });
                    }
                }
                "claim_renewed" => {
                    let renewal: WorkerQueueClaimRenewalRecord =
                        serde_json::from_value(event.payload)
                            .map_err(|error| LogError::Serde(error.to_string()))?;
                    let Some(job) = jobs.get_mut(&renewal.job_event_id) else {
                        continue;
                    };
                    if let Some(active) = job.active_claim.as_mut() {
                        if active.claim_id == renewal.claim_id {
                            active.expires_at_ms = renewal.expires_at_ms;
                        }
                    }
                }
                "job_released" => {
                    let release: WorkerQueueReleaseRecord =
                        serde_json::from_value(event.payload)
                            .map_err(|error| LogError::Serde(error.to_string()))?;
                    let Some(job) = jobs.get_mut(&release.job_event_id) else {
                        continue;
                    };
                    if job
                        .active_claim
                        .as_ref()
                        .is_some_and(|active| active.claim_id == release.claim_id)
                    {
                        job.active_claim = None;
                    }
                }
                "job_acked" => {
                    let ack: WorkerQueueAckRecord = serde_json::from_value(event.payload)
                        .map_err(|error| LogError::Serde(error.to_string()))?;
                    let Some(job) = jobs.get_mut(&ack.job_event_id) else {
                        continue;
                    };
                    if ack.claim_id.is_empty() || job.seen_claim_ids.contains(&ack.claim_id) {
                        job.acked = true;
                        job.active_claim = None;
                    }
                }
                "job_purged" => {
                    let purge: WorkerQueuePurgeRecord = serde_json::from_value(event.payload)
                        .map_err(|error| LogError::Serde(error.to_string()))?;
                    let Some(job) = jobs.get_mut(&purge.job_event_id) else {
                        continue;
                    };
                    if !job.acked {
                        job.purged = true;
                        job.active_claim = None;
                    }
                }
                _ => {}
            }
        }

        let responses = response_events
            .into_iter()
            .filter(|(_, event)| event.kind == "job_response")
            .map(|(_, event)| {
                serde_json::from_value::<WorkerQueueResponseRecord>(event.payload)
                    .map_err(|error| LogError::Serde(error.to_string()))
            })
            .collect::<Result<Vec<_>, _>>()?;

        let mut queue_state = WorkerQueueState {
            queue: queue_name.to_string(),
            responses,
            jobs: jobs
                .into_values()
                .map(|mut job| {
                    if job
                        .active_claim
                        .as_ref()
                        .is_some_and(|active| active.expires_at_ms <= now_ms)
                    {
                        job.active_claim = None;
                    }
                    WorkerQueueJobState {
                        job_event_id: job.job_event_id,
                        enqueued_at_ms: job.enqueued_at_ms,
                        job: job.job,
                        active_claim: job.active_claim,
                        acked: job.acked,
                        purged: job.purged,
                    }
                })
                .collect(),
        };
        queue_state
            .jobs
            .sort_by_key(|job| (job.enqueued_at_ms, job.job_event_id));
        Ok(queue_state)
    }

    pub async fn queue_summaries(&self) -> Result<Vec<WorkerQueueSummary>, LogError> {
        let now_ms = now_ms();
        let mut summaries = Vec::new();
        for queue in self.known_queues().await? {
            let state = self.queue_state(&queue).await?;
            summaries.push(state.summary(now_ms));
        }
        summaries.sort_by(|left, right| left.queue.cmp(&right.queue));
        Ok(summaries)
    }

    pub async fn claim_next(
        &self,
        queue: &str,
        consumer_id: &str,
        ttl: StdDuration,
    ) -> Result<Option<ClaimedWorkerJob>, LogError> {
        let queue_name = queue.trim();
        if queue_name.is_empty() {
            return Err(LogError::Config(
                "worker queue name cannot be empty".to_string(),
            ));
        }
        if consumer_id.trim().is_empty() {
            return Err(LogError::InvalidConsumer(
                "worker queue consumer id cannot be empty".to_string(),
            ));
        }
        for _ in 0..8 {
            let now_ms = now_ms();
            let state = self.queue_state(queue_name).await?;
            let Some(job) = state.next_ready_job(now_ms).cloned() else {
                return Ok(None);
            };
            let claim = WorkerQueueClaimRecord {
                job_event_id: job.job_event_id,
                claim_id: Uuid::new_v4().to_string(),
                consumer_id: consumer_id.to_string(),
                claimed_at_ms: now_ms,
                expires_at_ms: expiry_ms(now_ms, ttl),
            };
            self.event_log
                .append(
                    &claims_topic(queue_name)?,
                    LogEvent::new(
                        "job_claimed",
                        serde_json::to_value(&claim)
                            .map_err(|error| LogError::Serde(error.to_string()))?,
                    ),
                )
                .await?;
            let refreshed = self.queue_state(queue_name).await?;
            if refreshed
                .active_claim_for(job.job_event_id)
                .is_some_and(|active| active.claim_id == claim.claim_id)
            {
                return Ok(Some(ClaimedWorkerJob {
                    handle: WorkerQueueClaimHandle {
                        queue: queue_name.to_string(),
                        job_event_id: claim.job_event_id,
                        claim_id: claim.claim_id,
                        consumer_id: claim.consumer_id,
                        expires_at_ms: claim.expires_at_ms,
                    },
                    job: job.job,
                }));
            }
        }
        Ok(None)
    }

    pub async fn renew_claim(
        &self,
        handle: &WorkerQueueClaimHandle,
        ttl: StdDuration,
    ) -> Result<bool, LogError> {
        let now_ms = now_ms();
        let renewal = WorkerQueueClaimRenewalRecord {
            job_event_id: handle.job_event_id,
            claim_id: handle.claim_id.clone(),
            consumer_id: handle.consumer_id.clone(),
            renewed_at_ms: now_ms,
            expires_at_ms: expiry_ms(now_ms, ttl),
        };
        self.event_log
            .append(
                &claims_topic(&handle.queue)?,
                LogEvent::new(
                    "claim_renewed",
                    serde_json::to_value(&renewal)
                        .map_err(|error| LogError::Serde(error.to_string()))?,
                ),
            )
            .await?;
        let refreshed = self.queue_state(&handle.queue).await?;
        Ok(refreshed
            .active_claim_for(handle.job_event_id)
            .is_some_and(|active| active.claim_id == handle.claim_id))
    }

    pub async fn release_claim(
        &self,
        handle: &WorkerQueueClaimHandle,
        reason: &str,
    ) -> Result<(), LogError> {
        let release = WorkerQueueReleaseRecord {
            job_event_id: handle.job_event_id,
            claim_id: handle.claim_id.clone(),
            consumer_id: handle.consumer_id.clone(),
            released_at_ms: now_ms(),
            reason: if reason.trim().is_empty() {
                None
            } else {
                Some(reason.to_string())
            },
        };
        self.event_log
            .append(
                &claims_topic(&handle.queue)?,
                LogEvent::new(
                    "job_released",
                    serde_json::to_value(&release)
                        .map_err(|error| LogError::Serde(error.to_string()))?,
                ),
            )
            .await?;
        Ok(())
    }

    pub async fn append_response(
        &self,
        queue: &str,
        response: &WorkerQueueResponseRecord,
    ) -> Result<u64, LogError> {
        self.event_log
            .append(
                &responses_topic(queue)?,
                LogEvent::new(
                    "job_response",
                    serde_json::to_value(response)
                        .map_err(|error| LogError::Serde(error.to_string()))?,
                ),
            )
            .await
    }

    pub async fn ack_claim(&self, handle: &WorkerQueueClaimHandle) -> Result<u64, LogError> {
        self.event_log
            .append(
                &claims_topic(&handle.queue)?,
                LogEvent::new(
                    "job_acked",
                    serde_json::to_value(WorkerQueueAckRecord {
                        job_event_id: handle.job_event_id,
                        claim_id: handle.claim_id.clone(),
                        consumer_id: handle.consumer_id.clone(),
                        acked_at_ms: now_ms(),
                    })
                    .map_err(|error| LogError::Serde(error.to_string()))?,
                ),
            )
            .await
    }

    pub async fn purge_unclaimed(
        &self,
        queue: &str,
        purged_by: &str,
        reason: Option<&str>,
    ) -> Result<usize, LogError> {
        let state = self.queue_state(queue).await?;
        let ready_jobs: Vec<_> = state
            .jobs
            .into_iter()
            .filter(|job| job.is_ready())
            .map(|job| job.job_event_id)
            .collect();
        for job_event_id in &ready_jobs {
            self.event_log
                .append(
                    &claims_topic(queue)?,
                    LogEvent::new(
                        "job_purged",
                        serde_json::to_value(WorkerQueuePurgeRecord {
                            job_event_id: *job_event_id,
                            purged_by: purged_by.to_string(),
                            purged_at_ms: now_ms(),
                            reason: reason
                                .filter(|value| !value.trim().is_empty())
                                .map(|value| value.to_string()),
                        })
                        .map_err(|error| LogError::Serde(error.to_string()))?,
                    ),
                )
                .await?;
        }
        Ok(ready_jobs.len())
    }
}

#[derive(Clone, Debug)]
struct WorkerQueueJobStateInternal {
    job_event_id: u64,
    enqueued_at_ms: i64,
    job: WorkerQueueJob,
    active_claim: Option<WorkerQueueClaimHandle>,
    acked: bool,
    purged: bool,
    seen_claim_ids: BTreeSet<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct WorkerQueueCatalogRecord {
    queue: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct WorkerQueueClaimRecord {
    job_event_id: u64,
    claim_id: String,
    consumer_id: String,
    claimed_at_ms: i64,
    expires_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct WorkerQueueClaimRenewalRecord {
    job_event_id: u64,
    claim_id: String,
    consumer_id: String,
    renewed_at_ms: i64,
    expires_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct WorkerQueueReleaseRecord {
    job_event_id: u64,
    claim_id: String,
    consumer_id: String,
    released_at_ms: i64,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct WorkerQueueAckRecord {
    job_event_id: u64,
    claim_id: String,
    consumer_id: String,
    acked_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct WorkerQueuePurgeRecord {
    job_event_id: u64,
    purged_by: String,
    purged_at_ms: i64,
    #[serde(default)]
    reason: Option<String>,
}

pub fn job_topic_name(queue: &str) -> String {
    format!("worker.{}", sanitize_topic_component(queue))
}

pub fn claims_topic_name(queue: &str) -> String {
    format!("{}{}", job_topic_name(queue), WORKER_QUEUE_CLAIMS_SUFFIX)
}

pub fn response_topic_name(queue: &str) -> String {
    format!("{}{}", job_topic_name(queue), WORKER_QUEUE_RESPONSES_SUFFIX)
}

fn job_topic(queue: &str) -> Result<Topic, LogError> {
    Topic::new(job_topic_name(queue))
}

fn claims_topic(queue: &str) -> Result<Topic, LogError> {
    Topic::new(claims_topic_name(queue))
}

fn responses_topic(queue: &str) -> Result<Topic, LogError> {
    Topic::new(response_topic_name(queue))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

fn expiry_ms(now_ms: i64, ttl: StdDuration) -> i64 {
    now_ms.saturating_add(ttl.as_millis().min(i64::MAX as u128) as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::event_log::{AnyEventLog, MemoryEventLog};
    use crate::triggers::{
        event::{GenericWebhookPayload, KnownProviderPayload},
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
            .claim_next("triage", "consumer-a", StdDuration::from_secs(60))
            .await
            .unwrap()
            .unwrap();
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
    async fn expired_claim_allows_reclaim() {
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
        let first = queue
            .claim_next("triage", "consumer-a", StdDuration::from_millis(15))
            .await
            .unwrap()
            .unwrap();
        tokio::time::sleep(StdDuration::from_millis(30)).await;
        let second = queue
            .claim_next("triage", "consumer-b", StdDuration::from_secs(60))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first.job.event.id.0, second.job.event.id.0);
        assert_ne!(first.handle.claim_id, second.handle.claim_id);
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
        old_normal.occurred_at_ms = now_ms() - NORMAL_PROMOTION_AGE_MS - 1_000;
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
            .claim_next("triage", "consumer-a", StdDuration::from_secs(60))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(claimed.job.event.id.0, "evt-old-normal");
    }
}
