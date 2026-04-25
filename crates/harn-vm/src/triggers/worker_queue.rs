use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration as StdDuration;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::event_log::{
    sanitize_topic_component, AnyEventLog, EventLog, LogError, LogEvent, Topic,
};

use super::scheduler::{self, SchedulableJob, SchedulerPolicy, SchedulerSnapshot, SchedulerState};
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

    pub fn effective_rank(self, enqueued_at_ms: i64, now_ms: i64) -> u8 {
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

    /// Select the next ready job by consulting `scheduler` under `policy`.
    ///
    /// Under `Fifo` this is equivalent to picking the job with the lowest
    /// `(priority_rank, enqueued_at_ms, job_event_id)` — the historical
    /// behaviour. Under `DeficitRoundRobin`, candidates are grouped by the
    /// configured fairness key and the scheduler rotates so a hot
    /// tenant/binding cannot monopolise the queue.
    fn next_ready_job_with_scheduler(
        &self,
        scheduler_state: &mut SchedulerState,
        policy: &SchedulerPolicy,
        now_ms: i64,
    ) -> Option<&WorkerQueueJobState> {
        let candidates: Vec<&WorkerQueueJobState> =
            self.jobs.iter().filter(|job| job.is_ready()).collect();
        if candidates.is_empty() {
            return None;
        }
        let views: Vec<SchedulableJob<'_>> = candidates
            .iter()
            .map(|state| SchedulableJob::from_state(state))
            .collect();

        // Refresh authoritative in-flight count from the rebuilt queue state.
        let in_flight = scheduler::in_flight_by_key(&self.jobs, policy);
        scheduler_state.replace_in_flight(in_flight);

        let pick = scheduler_state.select(&views, policy, now_ms)?;
        candidates
            .into_iter()
            .find(|job| job.job_event_id == pick.job_event_id)
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
    /// Active scheduler policy. Reads on every claim so it can be hot-swapped
    /// at runtime without rebuilding the queue.
    policy: Arc<RwLock<SchedulerPolicy>>,
    /// Per-queue ephemeral scheduler state. Keyed by queue name; entries are
    /// created lazily on first claim. Self-correcting — safe to lose on
    /// process restart.
    scheduler_states: Arc<Mutex<BTreeMap<String, SchedulerState>>>,
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
            let (job, fairness_key) = {
                let mut states = self
                    .scheduler_states
                    .lock()
                    .expect("scheduler state poisoned");
                let scheduler_state = states.entry(queue_name.to_string()).or_default();
                let Some(job) =
                    state.next_ready_job_with_scheduler(scheduler_state, &policy, now_ms)
                else {
                    return Ok(None);
                };
                let job = job.clone();
                let fairness_key = policy.fairness_key_of(&SchedulableJob::from_state(&job));
                (job, fairness_key)
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
                {
                    let mut states = self
                        .scheduler_states
                        .lock()
                        .expect("scheduler state poisoned");
                    let scheduler_state = states.entry(queue_name.to_string()).or_default();
                    scheduler_state.note_claim_committed(&fairness_key);
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
                        &fairness_key,
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
        };
        log.append(
            &claims_topic("triage").unwrap(),
            LogEvent::new("job_claimed", serde_json::to_value(&expired_claim).unwrap()),
        )
        .await
        .unwrap();
        let second = queue
            .claim_next("triage", "consumer-b", StdDuration::from_secs(60))
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
                .claim_next("triage", &consumer, StdDuration::from_secs(60))
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
            .claim_next("triage", "c-0", StdDuration::from_secs(60))
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
                .claim_next("triage", &consumer, StdDuration::from_secs(60))
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
            .claim_next("triage", "consumer-a", StdDuration::from_secs(60))
            .await
            .unwrap()
            .unwrap();
        // Without releasing the first claim, the second pick must skip the
        // capped tenant-a and serve tenant-b instead.
        let second = queue
            .claim_next("triage", "consumer-b", StdDuration::from_secs(60))
            .await
            .unwrap()
            .unwrap();
        let pair = [
            first.job.event.tenant_id.clone().unwrap().0,
            second.job.event.tenant_id.clone().unwrap().0,
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
}
