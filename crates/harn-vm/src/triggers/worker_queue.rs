use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration as StdDuration;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::event_log::{
    sanitize_topic_component, AnyEventLog, EventLog, LogError, LogEvent, Topic,
};

use super::scheduler::{self, SchedulerPolicy, SchedulerSnapshot, SchedulerState};
use super::{DispatchOutcome, TriggerEvent};
use crate::TenantId;

#[cfg(test)]
mod exclusion_tests;
mod scheduling;
mod state;

pub use scheduling::{
    WorkerQueuePriority, WorkerQueueSchedulingDecision, WorkerQueueSchedulingReceipt,
    DEFERRABLE_PROMOTION_AGE_MS,
};
pub use state::{WorkerQueueJobState, WorkerQueueState, WorkerQueueSummary};

pub const WORKER_QUEUE_CATALOG_TOPIC: &str = "worker.queues";
const WORKER_QUEUE_CLAIMS_SUFFIX: &str = ".claims";
const WORKER_QUEUE_RESPONSES_SUFFIX: &str = ".responses";

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
    pub scheduling: WorkerQueueSchedulingReceipt,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerQueueResponseRecord {
    pub queue: String,
    pub job_event_id: u64,
    pub consumer_id: String,
    pub handled_at_ms: i64,
    pub outcome: Option<DispatchOutcome>,
    pub error: Option<String>,
}

#[derive(Clone)]
pub struct WorkerQueue {
    event_log: Arc<AnyEventLog>,
    /// Active scheduler policy. Reads on every claim so it can be hot-swapped
    /// at runtime without rebuilding the queue.
    policy: Arc<RwLock<SchedulerPolicy>>,
    /// Per-queue ephemeral scheduler state. Keyed by queue name; entries are
    /// created lazily on first claim. Self-correcting — safe to lose on
    /// process restart.
    scheduler_states: Arc<Mutex<BTreeMap<String, SchedulerState>>>,
}

#[derive(Clone, Copy)]
enum TenantClaimScope<'a> {
    Any,
    Untenanted,
    Tenant(&'a TenantId),
}

#[derive(Clone, Debug, Serialize)]
pub struct WorkerQueueInspectSnapshot {
    pub summary: WorkerQueueSummary,
    pub scheduler: SchedulerSnapshot,
}

impl WorkerQueue {
    /// Construct a `WorkerQueue` using the policy derived from the
    /// `HARN_SCHEDULER_*` environment variables (see
    /// [`SchedulerPolicy::from_env`]). Defaults to FIFO so single-tenant
    /// deployments behave exactly as before unless they opt in.
    pub fn new(event_log: Arc<AnyEventLog>) -> Self {
        Self::with_policy(event_log, SchedulerPolicy::from_env())
    }

    pub fn with_policy(event_log: Arc<AnyEventLog>, policy: SchedulerPolicy) -> Self {
        Self {
            event_log,
            policy: Arc::new(RwLock::new(policy)),
            scheduler_states: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    /// Replace the active scheduler policy. Existing per-queue state is
    /// preserved (deficits self-correct against the new weights).
    pub fn set_policy(&self, policy: SchedulerPolicy) {
        *self.policy.write().expect("scheduler policy poisoned") = policy;
    }

    pub fn policy(&self) -> SchedulerPolicy {
        self.policy
            .read()
            .expect("scheduler policy poisoned")
            .clone()
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
        if let Some(metrics) = crate::active_metrics_registry() {
            if let Ok(state) = self.queue_state(&queue_name).await {
                let summary = state.summary(now_ms());
                metrics.set_worker_queue_depth(
                    &queue_name,
                    (summary.ready + summary.in_flight) as u64,
                );
            }
        }
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
        self.claim_next_matching(
            queue,
            consumer_id,
            ttl,
            &BTreeSet::new(),
            TenantClaimScope::Any,
        )
        .await
    }

    /// Claim the next ready job that has no tenant identity.
    pub async fn claim_next_untenanted(
        &self,
        queue: &str,
        consumer_id: &str,
        ttl: StdDuration,
    ) -> Result<Option<ClaimedWorkerJob>, LogError> {
        self.claim_next_matching(
            queue,
            consumer_id,
            ttl,
            &BTreeSet::new(),
            TenantClaimScope::Untenanted,
        )
        .await
    }

    /// Claim the next ready job whose event belongs to `tenant_id`.
    ///
    /// Foreign and unscoped jobs remain unclaimed so another worker can
    /// process them under the correct authority.
    pub async fn claim_next_for_tenant(
        &self,
        queue: &str,
        consumer_id: &str,
        ttl: StdDuration,
        tenant_id: &TenantId,
    ) -> Result<Option<ClaimedWorkerJob>, LogError> {
        self.claim_next_matching(
            queue,
            consumer_id,
            ttl,
            &BTreeSet::new(),
            TenantClaimScope::Tenant(tenant_id),
        )
        .await
    }

    /// Claim the next eligible job except those already attempted by this
    /// consumer operation. Exclusions affect selection only; a later caller
    /// can reclaim an unacknowledged job through [`Self::claim_next`].
    pub async fn claim_next_excluding(
        &self,
        queue: &str,
        consumer_id: &str,
        ttl: StdDuration,
        excluded_job_event_ids: &BTreeSet<u64>,
    ) -> Result<Option<ClaimedWorkerJob>, LogError> {
        self.claim_next_matching(
            queue,
            consumer_id,
            ttl,
            excluded_job_event_ids,
            TenantClaimScope::Any,
        )
        .await
    }

    async fn claim_next_matching(
        &self,
        queue: &str,
        consumer_id: &str,
        ttl: StdDuration,
        excluded_job_event_ids: &BTreeSet<u64>,
        tenant_scope: TenantClaimScope<'_>,
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
        let policy = self.policy();
        for _ in 0..8 {
            let now_ms = now_ms();
            let state = self.queue_state(queue_name).await?;
            let (job, selection) = {
                let mut states = self
                    .scheduler_states
                    .lock()
                    .expect("scheduler state poisoned");
                let scheduler_state = states.entry(queue_name.to_string()).or_default();
                let Some((job, selection)) = state.next_ready_job_with_scheduler(
                    scheduler_state,
                    &policy,
                    now_ms,
                    excluded_job_event_ids,
                    tenant_scope,
                ) else {
                    return Ok(None);
                };
                (job.clone(), selection)
            };
            let scheduling = WorkerQueueSchedulingReceipt {
                selected_at_ms: now_ms,
                enqueued_at_ms: job.enqueued_at_ms,
                waited_ms: now_ms.saturating_sub(job.enqueued_at_ms).max(0) as u64,
                priority: job.job.priority,
                decision: selection.decision,
                promotion_deadline_at_ms: selection.promotion_deadline_at_ms,
                fairness_key: selection.fairness_key.clone(),
            };
            let claim = WorkerQueueClaimRecord {
                job_event_id: job.job_event_id,
                claim_id: Uuid::new_v4().to_string(),
                consumer_id: consumer_id.to_string(),
                claimed_at_ms: now_ms,
                expires_at_ms: expiry_ms(now_ms, ttl),
                scheduling: Some(scheduling.clone()),
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
                {
                    let mut states = self
                        .scheduler_states
                        .lock()
                        .expect("scheduler state poisoned");
                    let scheduler_state = states.entry(queue_name.to_string()).or_default();
                    scheduler_state.note_claim_committed(&selection.fairness_key);
                }
                if let Some(metrics) = crate::active_metrics_registry() {
                    let summary = refreshed.summary(now_ms);
                    metrics.record_worker_queue_claim_age(
                        queue_name,
                        now_ms.saturating_sub(job.enqueued_at_ms) as f64 / 1000.0,
                    );
                    metrics.set_worker_queue_depth(
                        queue_name,
                        (summary.ready + summary.in_flight) as u64,
                    );
                    metrics.record_scheduler_selection(
                        queue_name,
                        policy.fairness_key.as_str(),
                        &selection.fairness_key,
                    );
                    if let Ok(snap) = self.inspect_queue(queue_name).await {
                        for stat in &snap.scheduler.keys {
                            metrics.set_scheduler_deficit(
                                queue_name,
                                policy.fairness_key.as_str(),
                                &stat.fairness_key,
                                stat.deficit,
                            );
                            metrics.set_scheduler_oldest_eligible_age(
                                queue_name,
                                policy.fairness_key.as_str(),
                                &stat.fairness_key,
                                stat.oldest_ready_age_ms,
                            );
                        }
                    }
                }
                return Ok(Some(ClaimedWorkerJob {
                    handle: WorkerQueueClaimHandle {
                        queue: queue_name.to_string(),
                        job_event_id: claim.job_event_id,
                        claim_id: claim.claim_id,
                        consumer_id: claim.consumer_id,
                        expires_at_ms: claim.expires_at_ms,
                    },
                    job: job.job,
                    scheduling,
                }));
            }
        }
        Ok(None)
    }

    /// Build a fairness-aware inspect snapshot for `queue` that includes
    /// scheduler state alongside the standard summary.
    pub async fn inspect_queue(&self, queue: &str) -> Result<WorkerQueueInspectSnapshot, LogError> {
        let queue_name = queue.trim();
        if queue_name.is_empty() {
            return Err(LogError::Config(
                "worker queue name cannot be empty".to_string(),
            ));
        }
        let now_ms = now_ms();
        let state = self.queue_state(queue_name).await?;
        let summary = state.summary(now_ms);
        let policy = self.policy();
        let ready = scheduler::ready_stats_by_key(&state.jobs, &policy, now_ms);
        // Make sure in-flight stays authoritative against the rebuilt log.
        let in_flight = scheduler::in_flight_by_key(&state.jobs, &policy);
        let scheduler_snapshot = {
            let mut states = self
                .scheduler_states
                .lock()
                .expect("scheduler state poisoned");
            let scheduler_state = states.entry(queue_name.to_string()).or_default();
            scheduler_state.replace_in_flight(in_flight);
            scheduler_state.snapshot(&policy, &ready)
        };
        Ok(WorkerQueueInspectSnapshot {
            summary,
            scheduler: scheduler_snapshot,
        })
    }

    /// Inspect snapshots for every known queue.
    pub async fn inspect_all_queues(&self) -> Result<Vec<WorkerQueueInspectSnapshot>, LogError> {
        let mut snapshots = Vec::new();
        for queue in self.known_queues().await? {
            snapshots.push(self.inspect_queue(&queue).await?);
        }
        snapshots.sort_by(|left, right| left.summary.queue.cmp(&right.summary.queue));
        Ok(snapshots)
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

    pub async fn ack_job(
        &self,
        queue: &str,
        job_event_id: u64,
        consumer_id: &str,
    ) -> Result<bool, LogError> {
        let queue_name = queue.trim();
        if queue_name.is_empty() {
            return Err(LogError::Config(
                "worker queue name cannot be empty".to_string(),
            ));
        }
        let state = self.queue_state(queue_name).await?;
        let Some(job) = state
            .jobs
            .iter()
            .find(|job| job.job_event_id == job_event_id)
        else {
            return Ok(false);
        };
        if job.acked || job.purged {
            return Ok(false);
        }
        self.event_log
            .append(
                &claims_topic(queue_name)?,
                LogEvent::new(
                    "job_acked",
                    serde_json::to_value(WorkerQueueAckRecord {
                        job_event_id,
                        claim_id: String::new(),
                        consumer_id: consumer_id.to_string(),
                        acked_at_ms: now_ms(),
                    })
                    .map_err(|error| LogError::Serde(error.to_string()))?,
                ),
            )
            .await?;
        Ok(true)
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scheduling: Option<WorkerQueueSchedulingReceipt>,
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
mod tests;
