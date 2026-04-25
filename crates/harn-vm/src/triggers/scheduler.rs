//! Fair-share scheduler for trigger dispatch and worker-queue claims.
//!
//! Implements a deterministic weighted round-robin (WRR) selection policy with
//! deficit accounting and starvation-age promotion. The scheduler groups
//! ready candidates by a fairness key (tenant, binding, trigger, or a
//! composite) and rotates through groups so a hot key cannot monopolise
//! shared capacity.
//!
//! The default policy (`SchedulerPolicy::fifo`) reproduces the previous
//! priority-then-FIFO behaviour, so single-tenant deployments and existing
//! callers see no change unless they opt in to deficit round-robin.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::worker_queue::{WorkerQueueJobState, WorkerQueuePriority};
use super::TenantId;

/// Default starvation-age promotion threshold (5 minutes).
pub const DEFAULT_STARVATION_AGE_MS: u64 = 5 * 60 * 1000;

/// What dimension to use when grouping ready candidates for fair-share.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FairnessKey {
    /// Group by `TriggerEvent.tenant_id` (events without a tenant share a
    /// single bucket).
    #[default]
    Tenant,
    /// Group by `WorkerQueueJob.binding_key` (binding-version aware).
    Binding,
    /// Group by `WorkerQueueJob.trigger_id`.
    TriggerId,
    /// Composite of tenant + binding so multi-binding tenants get fairness
    /// per-binding within their share.
    TenantAndBinding,
}

impl FairnessKey {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tenant => "tenant",
            Self::Binding => "binding",
            Self::TriggerId => "trigger-id",
            Self::TenantAndBinding => "tenant-and-binding",
        }
    }
}

/// Selection strategy used by the scheduler.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum SchedulerStrategy {
    /// Pure FIFO with priority + age promotion. Equivalent to historical
    /// behaviour.
    #[default]
    Fifo,
    /// Deficit / weighted round-robin across fairness keys.
    DeficitRoundRobin {
        /// Base credits granted to a fairness key per refill round (default 1).
        /// Higher quantum amortises rotation overhead at the cost of larger
        /// burst windows.
        #[serde(default = "default_quantum")]
        quantum: u32,
        /// Optional starvation-age promotion threshold in milliseconds. When
        /// set, any ready job older than the threshold is selected regardless
        /// of credits.
        #[serde(default)]
        starvation_age_ms: Option<u64>,
    },
}

fn default_quantum() -> u32 {
    1
}

/// Policy controlling scheduler selection.
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
pub struct SchedulerPolicy {
    #[serde(default)]
    pub strategy: SchedulerStrategy,
    #[serde(default)]
    pub fairness_key: FairnessKey,
    /// Per-fairness-key weights. Missing keys default to `default_weight`.
    #[serde(default)]
    pub weights: BTreeMap<String, u32>,
    #[serde(default = "default_weight")]
    pub default_weight: u32,
    /// Per-fairness-key max in-flight claims (0 = unlimited).
    #[serde(default)]
    pub max_concurrent_per_key: u32,
}

fn default_weight() -> u32 {
    1
}

impl SchedulerPolicy {
    pub fn fifo() -> Self {
        Self::default()
    }

    pub fn deficit_round_robin(fairness_key: FairnessKey) -> Self {
        Self {
            strategy: SchedulerStrategy::DeficitRoundRobin {
                quantum: 1,
                starvation_age_ms: Some(DEFAULT_STARVATION_AGE_MS),
            },
            fairness_key,
            weights: BTreeMap::new(),
            default_weight: 1,
            max_concurrent_per_key: 0,
        }
    }

    pub fn with_weight(mut self, key: impl Into<String>, weight: u32) -> Self {
        self.weights.insert(key.into(), weight.max(1));
        self
    }

    pub fn with_max_concurrent_per_key(mut self, max: u32) -> Self {
        self.max_concurrent_per_key = max;
        self
    }

    pub fn with_starvation_age_ms(mut self, age_ms: u64) -> Self {
        if let SchedulerStrategy::DeficitRoundRobin {
            starvation_age_ms, ..
        } = &mut self.strategy
        {
            *starvation_age_ms = Some(age_ms);
        }
        self
    }

    pub fn weight_for(&self, key: &str) -> u32 {
        self.weights
            .get(key)
            .copied()
            .unwrap_or(self.default_weight)
            .max(1)
    }

    /// Build a policy from `HARN_SCHEDULER_*` environment variables.
    ///
    /// Recognised variables:
    /// - `HARN_SCHEDULER_STRATEGY` — `fifo` (default) or `drr`.
    /// - `HARN_SCHEDULER_FAIRNESS_KEY` — `tenant` (default) | `binding` |
    ///   `trigger-id` | `tenant-and-binding`.
    /// - `HARN_SCHEDULER_QUANTUM` — positive integer (default 1).
    /// - `HARN_SCHEDULER_STARVATION_AGE_MS` — milliseconds (default 300000).
    ///   Set to `0` to disable starvation-age promotion.
    /// - `HARN_SCHEDULER_MAX_CONCURRENT_PER_KEY` — `0` for unlimited (default).
    /// - `HARN_SCHEDULER_WEIGHTS` — comma-separated `key:weight` pairs (e.g.
    ///   `tenant-a:3,tenant-b:1`). Unknown keys fall back to `default_weight`.
    /// - `HARN_SCHEDULER_DEFAULT_WEIGHT` — positive integer (default 1).
    ///
    /// Invalid values fall back to defaults rather than failing — the
    /// scheduler is best-effort and must not refuse to start.
    pub fn from_env() -> Self {
        Self::from_env_lookup(|name| std::env::var(name).ok())
    }

    /// Same as [`Self::from_env`] but driven by an explicit lookup function
    /// (useful for tests).
    pub fn from_env_lookup<F>(lookup: F) -> Self
    where
        F: Fn(&str) -> Option<String>,
    {
        let strategy_raw = lookup("HARN_SCHEDULER_STRATEGY")
            .map(|value| value.trim().to_ascii_lowercase())
            .unwrap_or_else(|| "fifo".to_string());
        let fairness_key = parse_fairness_key(
            lookup("HARN_SCHEDULER_FAIRNESS_KEY")
                .as_deref()
                .map(str::trim)
                .unwrap_or(""),
        );
        let default_weight = lookup("HARN_SCHEDULER_DEFAULT_WEIGHT")
            .as_deref()
            .and_then(|raw| raw.trim().parse::<u32>().ok())
            .filter(|n| *n >= 1)
            .unwrap_or(1);
        let weights = lookup("HARN_SCHEDULER_WEIGHTS")
            .map(|raw| parse_weights(&raw))
            .unwrap_or_default();
        let max_concurrent_per_key = lookup("HARN_SCHEDULER_MAX_CONCURRENT_PER_KEY")
            .as_deref()
            .and_then(|raw| raw.trim().parse::<u32>().ok())
            .unwrap_or(0);

        let strategy = match strategy_raw.as_str() {
            "drr" | "deficit-round-robin" | "fair-share" => {
                let quantum = lookup("HARN_SCHEDULER_QUANTUM")
                    .as_deref()
                    .and_then(|raw| raw.trim().parse::<u32>().ok())
                    .filter(|n| *n >= 1)
                    .unwrap_or(1);
                let starvation_age_ms = match lookup("HARN_SCHEDULER_STARVATION_AGE_MS").as_deref()
                {
                    Some(raw) => {
                        let parsed = raw.trim().parse::<u64>().ok();
                        match parsed {
                            Some(0) => None,
                            Some(n) => Some(n),
                            None => Some(DEFAULT_STARVATION_AGE_MS),
                        }
                    }
                    None => Some(DEFAULT_STARVATION_AGE_MS),
                };
                SchedulerStrategy::DeficitRoundRobin {
                    quantum,
                    starvation_age_ms,
                }
            }
            _ => SchedulerStrategy::Fifo,
        };

        Self {
            strategy,
            fairness_key,
            weights,
            default_weight,
            max_concurrent_per_key,
        }
    }

    pub fn fairness_key_of(&self, job: &SchedulableJob<'_>) -> String {
        match self.fairness_key {
            FairnessKey::Tenant => job
                .tenant_id
                .map(|t| t.0.clone())
                .unwrap_or_else(|| "_no_tenant".to_string()),
            FairnessKey::Binding => job.binding_key.to_string(),
            FairnessKey::TriggerId => job.trigger_id.to_string(),
            FairnessKey::TenantAndBinding => {
                let tenant = job.tenant_id.map(|t| t.0.as_str()).unwrap_or("_no_tenant");
                format!("{tenant}|{}", job.binding_key)
            }
        }
    }

    pub fn strategy_name(&self) -> &'static str {
        match self.strategy {
            SchedulerStrategy::Fifo => "fifo",
            SchedulerStrategy::DeficitRoundRobin { .. } => "drr",
        }
    }
}

/// Decoupled view over the bits of a job the scheduler needs.
#[derive(Clone, Copy, Debug)]
pub struct SchedulableJob<'a> {
    pub job_event_id: u64,
    pub enqueued_at_ms: i64,
    pub priority: WorkerQueuePriority,
    pub tenant_id: Option<&'a TenantId>,
    pub binding_key: &'a str,
    pub trigger_id: &'a str,
    pub queue: &'a str,
}

impl<'a> SchedulableJob<'a> {
    pub fn from_state(state: &'a WorkerQueueJobState) -> Self {
        Self {
            job_event_id: state.job_event_id,
            enqueued_at_ms: state.enqueued_at_ms,
            priority: state.job.priority,
            tenant_id: state.job.event.tenant_id.as_ref(),
            binding_key: state.job.binding_key.as_str(),
            trigger_id: state.job.trigger_id.as_str(),
            queue: state.job.queue.as_str(),
        }
    }
}

/// Owned identity of the selected job. Returned by [`SchedulerState::select`]
/// so callers can look the original job up in their own data structures
/// without grappling with the candidate lifetime.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SchedulerSelection {
    pub job_event_id: u64,
    pub fairness_key: String,
}

/// Mutable in-memory state. Self-correcting: deficits even out over time so
/// it is safe to lose this on process restart.
#[derive(Clone, Debug, Default)]
pub struct SchedulerState {
    /// Current credit balance per fairness key. Refills happen lazily when a
    /// round completes with no key holding credits.
    credits: BTreeMap<String, u32>,
    /// Currently-claimed job count per fairness key (used for
    /// `max_concurrent_per_key`).
    in_flight: BTreeMap<String, u32>,
    /// Last fairness key selected — drives round-robin progression.
    last_selected: Option<String>,
    /// Cumulative selection count per fairness key (metrics).
    selected_total: BTreeMap<String, u64>,
    /// Cumulative deferral count per fairness key (metrics).
    deferred_total: BTreeMap<String, u64>,
    /// Number of times the scheduler force-selected a job because its age
    /// exceeded the configured starvation threshold.
    starvation_promotions_total: u64,
    /// Number of complete round-robin rounds (refill events).
    rounds_completed: u64,
}

impl SchedulerState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Choose the next job to dispatch from `candidates`. Returns `None` if no
    /// candidate is eligible (all keys at their `max_concurrent_per_key` cap
    /// or `candidates` is empty).
    pub fn select(
        &mut self,
        candidates: &[SchedulableJob<'_>],
        policy: &SchedulerPolicy,
        now_ms: i64,
    ) -> Option<SchedulerSelection> {
        if candidates.is_empty() {
            return None;
        }
        match &policy.strategy {
            SchedulerStrategy::Fifo => fifo_select(candidates, policy, now_ms),
            SchedulerStrategy::DeficitRoundRobin {
                quantum,
                starvation_age_ms,
            } => self.drr_select(candidates, policy, *quantum, *starvation_age_ms, now_ms),
        }
    }

    /// Increment in-flight counter for a fairness key. Call when a claim
    /// commits successfully.
    pub fn note_claim_committed(&mut self, fairness_key: &str) {
        *self.in_flight.entry(fairness_key.to_string()).or_default() += 1;
    }

    /// Decrement in-flight counter. Call when a claim is released, ack'd, or
    /// expires.
    pub fn note_claim_released(&mut self, fairness_key: &str) {
        if let Some(count) = self.in_flight.get_mut(fairness_key) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.in_flight.remove(fairness_key);
            }
        }
    }

    /// Adopt an externally-observed claim count snapshot (used when the
    /// in-flight state is reconstructed from the event log on a fresh process).
    pub fn replace_in_flight(&mut self, snapshot: BTreeMap<String, u32>) {
        self.in_flight = snapshot.into_iter().filter(|(_, n)| *n > 0).collect();
    }

    pub fn rounds_completed(&self) -> u64 {
        self.rounds_completed
    }

    pub fn starvation_promotions_total(&self) -> u64 {
        self.starvation_promotions_total
    }

    pub fn deficit_for(&self, key: &str) -> i64 {
        self.credits.get(key).copied().unwrap_or(0) as i64
    }

    pub fn in_flight_for(&self, key: &str) -> u32 {
        self.in_flight.get(key).copied().unwrap_or(0)
    }

    pub fn selected_total_for(&self, key: &str) -> u64 {
        self.selected_total.get(key).copied().unwrap_or(0)
    }

    pub fn deferred_total_for(&self, key: &str) -> u64 {
        self.deferred_total.get(key).copied().unwrap_or(0)
    }

    pub fn snapshot(
        &self,
        policy: &SchedulerPolicy,
        ready_by_key: &BTreeMap<String, ReadyKeyStats>,
    ) -> SchedulerSnapshot {
        let mut all_keys: BTreeSet<String> = self.credits.keys().cloned().collect();
        all_keys.extend(self.selected_total.keys().cloned());
        all_keys.extend(self.deferred_total.keys().cloned());
        all_keys.extend(self.in_flight.keys().cloned());
        all_keys.extend(ready_by_key.keys().cloned());

        let keys = all_keys
            .into_iter()
            .map(|key| {
                let ready = ready_by_key
                    .get(&key)
                    .copied()
                    .unwrap_or(ReadyKeyStats::default());
                SchedulerKeyStat {
                    fairness_key: key.clone(),
                    weight: policy.weight_for(&key),
                    deficit: self.deficit_for(&key),
                    in_flight: self.in_flight_for(&key),
                    selected_total: self.selected_total_for(&key),
                    deferred_total: self.deferred_total_for(&key),
                    ready_jobs: ready.ready_jobs,
                    oldest_ready_age_ms: ready.oldest_ready_age_ms,
                }
            })
            .collect();

        SchedulerSnapshot {
            strategy: policy.strategy_name().to_string(),
            fairness_key: policy.fairness_key.as_str().to_string(),
            rounds_completed: self.rounds_completed,
            starvation_promotions_total: self.starvation_promotions_total,
            keys,
        }
    }

    fn drr_select(
        &mut self,
        candidates: &[SchedulableJob<'_>],
        policy: &SchedulerPolicy,
        quantum: u32,
        starvation_age_ms: Option<u64>,
        now_ms: i64,
    ) -> Option<SchedulerSelection> {
        // Group by fairness key, sorted within each group by
        // (priority, enqueue, id) so the selected head is deterministic.
        let mut groups: BTreeMap<String, Vec<&SchedulableJob<'_>>> = BTreeMap::new();
        for job in candidates {
            let key = policy.fairness_key_of(job);
            groups.entry(key).or_default().push(job);
        }
        for jobs in groups.values_mut() {
            jobs.sort_by_key(|j| {
                (
                    j.priority.effective_rank(j.enqueued_at_ms, now_ms),
                    j.enqueued_at_ms,
                    j.job_event_id,
                )
            });
        }

        // Starvation override: any eligible head whose age exceeds the
        // threshold wins, with the oldest head breaking ties.
        if let Some(threshold) = starvation_age_ms {
            let mut oldest: Option<(i64, String, u64)> = None;
            for (key, jobs) in &groups {
                if policy.max_concurrent_per_key > 0
                    && self.in_flight_for(key) >= policy.max_concurrent_per_key
                {
                    continue;
                }
                let head = match jobs.first() {
                    Some(job) => *job,
                    None => continue,
                };
                let age_ms = now_ms.saturating_sub(head.enqueued_at_ms).max(0);
                if (age_ms as u64) >= threshold
                    && oldest
                        .as_ref()
                        .map(|(prev, _, _)| head.enqueued_at_ms < *prev)
                        .unwrap_or(true)
                {
                    oldest = Some((head.enqueued_at_ms, key.clone(), head.job_event_id));
                }
            }
            if let Some((_, key, job_event_id)) = oldest {
                self.starvation_promotions_total += 1;
                self.commit_selection(&key);
                return Some(SchedulerSelection {
                    job_event_id,
                    fairness_key: key,
                });
            }
        }

        // Eligible keys (under per-key concurrency cap and with ready work).
        let mut eligible_keys: Vec<String> = groups
            .iter()
            .filter(|(key, jobs)| {
                !jobs.is_empty()
                    && (policy.max_concurrent_per_key == 0
                        || self.in_flight_for(key) < policy.max_concurrent_per_key)
            })
            .map(|(key, _)| key.clone())
            .collect();
        eligible_keys.sort();
        if eligible_keys.is_empty() {
            for key in groups.keys() {
                *self.deferred_total.entry(key.clone()).or_default() += 1;
            }
            return None;
        }

        let n = eligible_keys.len();
        let start = self.start_index(&eligible_keys);

        // Pass 1: find a key that already has credits.
        for offset in 0..n {
            let idx = (start + offset) % n;
            let key = eligible_keys[idx].clone();
            if self.credits.get(&key).copied().unwrap_or(0) >= 1 {
                let job_event_id = groups
                    .get(&key)
                    .and_then(|jobs| jobs.first())
                    .map(|job| job.job_event_id)?;
                self.spend_credit(&key);
                self.commit_selection(&key);
                return Some(SchedulerSelection {
                    job_event_id,
                    fairness_key: key,
                });
            }
        }

        // Pass 2: refill all eligible keys (one full round) and try again.
        for key in &eligible_keys {
            let credits = policy.weight_for(key) as u64 * quantum as u64;
            let credits = credits.min(u32::MAX as u64) as u32;
            *self.credits.entry(key.clone()).or_insert(0) += credits;
        }
        self.rounds_completed += 1;

        for offset in 0..n {
            let idx = (start + offset) % n;
            let key = eligible_keys[idx].clone();
            if self.credits.get(&key).copied().unwrap_or(0) >= 1 {
                let job_event_id = groups
                    .get(&key)
                    .and_then(|jobs| jobs.first())
                    .map(|job| job.job_event_id)?;
                self.spend_credit(&key);
                self.commit_selection(&key);
                return Some(SchedulerSelection {
                    job_event_id,
                    fairness_key: key,
                });
            }
        }
        None
    }

    fn start_index(&self, eligible_keys: &[String]) -> usize {
        match &self.last_selected {
            Some(last) => eligible_keys
                .iter()
                .position(|key| key.as_str() > last.as_str())
                .unwrap_or(0),
            None => 0,
        }
    }

    fn spend_credit(&mut self, key: &str) {
        if let Some(credits) = self.credits.get_mut(key) {
            *credits = credits.saturating_sub(1);
        }
    }

    fn commit_selection(&mut self, key: &str) {
        self.last_selected = Some(key.to_string());
        *self.selected_total.entry(key.to_string()).or_default() += 1;
    }
}

fn fifo_select(
    candidates: &[SchedulableJob<'_>],
    policy: &SchedulerPolicy,
    now_ms: i64,
) -> Option<SchedulerSelection> {
    let pick = candidates.iter().min_by_key(|job| {
        (
            job.priority.effective_rank(job.enqueued_at_ms, now_ms),
            job.enqueued_at_ms,
            job.job_event_id,
        )
    })?;
    Some(SchedulerSelection {
        job_event_id: pick.job_event_id,
        fairness_key: policy.fairness_key_of(pick),
    })
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct ReadyKeyStats {
    pub ready_jobs: u32,
    pub oldest_ready_age_ms: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct SchedulerKeyStat {
    pub fairness_key: String,
    pub weight: u32,
    pub deficit: i64,
    pub in_flight: u32,
    pub selected_total: u64,
    pub deferred_total: u64,
    pub ready_jobs: u32,
    pub oldest_ready_age_ms: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct SchedulerSnapshot {
    pub strategy: String,
    pub fairness_key: String,
    pub rounds_completed: u64,
    pub starvation_promotions_total: u64,
    pub keys: Vec<SchedulerKeyStat>,
}

/// Aggregate per-fairness-key stats from the candidates currently in a queue
/// state. Used to render the inspect snapshot.
pub fn ready_stats_by_key(
    jobs: &[WorkerQueueJobState],
    policy: &SchedulerPolicy,
    now_ms: i64,
) -> BTreeMap<String, ReadyKeyStats> {
    let mut out: BTreeMap<String, ReadyKeyStats> = BTreeMap::new();
    for state in jobs.iter().filter(|j| j.is_ready()) {
        let view = SchedulableJob::from_state(state);
        let key = policy.fairness_key_of(&view);
        let entry = out.entry(key).or_default();
        entry.ready_jobs += 1;
        let age = now_ms.saturating_sub(state.enqueued_at_ms).max(0) as u64;
        if age > entry.oldest_ready_age_ms {
            entry.oldest_ready_age_ms = age;
        }
    }
    out
}

fn parse_fairness_key(raw: &str) -> FairnessKey {
    match raw.to_ascii_lowercase().as_str() {
        "binding" => FairnessKey::Binding,
        "trigger-id" | "trigger_id" => FairnessKey::TriggerId,
        "tenant-and-binding" | "tenant_and_binding" | "composite" => FairnessKey::TenantAndBinding,
        _ => FairnessKey::Tenant,
    }
}

fn parse_weights(raw: &str) -> BTreeMap<String, u32> {
    let mut out = BTreeMap::new();
    for entry in raw.split(',') {
        let trimmed = entry.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some((key, value)) = trimmed.rsplit_once(':') {
            let key = key.trim().to_string();
            if key.is_empty() {
                continue;
            }
            if let Ok(weight) = value.trim().parse::<u32>() {
                if weight >= 1 {
                    out.insert(key, weight);
                }
            }
        }
    }
    out
}

/// Aggregate active claim counts by fairness key from the queue state.
pub fn in_flight_by_key(
    jobs: &[WorkerQueueJobState],
    policy: &SchedulerPolicy,
) -> BTreeMap<String, u32> {
    let mut out: BTreeMap<String, u32> = BTreeMap::new();
    for state in jobs {
        if state.acked || state.purged || state.active_claim.is_none() {
            continue;
        }
        let view = SchedulableJob::from_state(state);
        let key = policy.fairness_key_of(&view);
        *out.entry(key).or_default() += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::triggers::event::{
        GenericWebhookPayload, KnownProviderPayload, ProviderId, ProviderPayload, SignatureStatus,
        TraceId, TriggerEvent, TriggerEventId,
    };
    use crate::triggers::worker_queue::{WorkerQueueJob, WorkerQueueJobState};
    use std::collections::BTreeMap as Map;

    fn event(id: &str, tenant: Option<&str>) -> TriggerEvent {
        TriggerEvent {
            id: TriggerEventId(id.to_string()),
            provider: ProviderId::from("test"),
            kind: "test.event".to_string(),
            trace_id: TraceId("trace-x".to_string()),
            dedupe_key: id.to_string(),
            tenant_id: tenant.map(TenantId::new),
            headers: Map::new(),
            batch: None,
            raw_body: None,
            provider_payload: ProviderPayload::Known(KnownProviderPayload::Webhook(
                GenericWebhookPayload {
                    source: None,
                    content_type: None,
                    raw: serde_json::json!({}),
                },
            )),
            signature_status: SignatureStatus::Verified,
            received_at: time::OffsetDateTime::now_utc(),
            occurred_at: None,
            dedupe_claimed: false,
        }
    }

    fn state(
        job_event_id: u64,
        enqueued_at_ms: i64,
        tenant: Option<&str>,
        trigger_id: &str,
        priority: WorkerQueuePriority,
    ) -> WorkerQueueJobState {
        WorkerQueueJobState {
            job_event_id,
            enqueued_at_ms,
            job: WorkerQueueJob {
                queue: "q".to_string(),
                trigger_id: trigger_id.to_string(),
                binding_key: format!("{trigger_id}@v1"),
                binding_version: 1,
                event: event(&format!("evt-{job_event_id}"), tenant),
                replay_of_event_id: None,
                priority,
            },
            active_claim: None,
            acked: false,
            purged: false,
        }
    }

    fn snapshot_views<'a>(states: &'a [WorkerQueueJobState]) -> Vec<SchedulableJob<'a>> {
        states.iter().map(SchedulableJob::from_state).collect()
    }

    #[test]
    fn fifo_strategy_matches_priority_and_age() {
        let jobs = vec![
            state(1, 100, Some("a"), "t-low", WorkerQueuePriority::Low),
            state(2, 50, Some("a"), "t-high", WorkerQueuePriority::High),
            state(3, 200, Some("a"), "t-normal", WorkerQueuePriority::Normal),
        ];
        let mut sched = SchedulerState::new();
        let policy = SchedulerPolicy::fifo();
        let pick = sched
            .select(&snapshot_views(&jobs), &policy, 1_000)
            .unwrap();
        assert_eq!(pick.job_event_id, 2, "high priority always wins under FIFO");
    }

    #[test]
    fn drr_alternates_across_tenants_when_one_tenant_is_hot() {
        // Tenant A has 100 jobs, tenant B has 1. With pure FIFO, A starves B.
        // Under DRR with equal weights, we expect strict alternation.
        let mut jobs = Vec::new();
        for idx in 0..100 {
            jobs.push(state(
                100 + idx,
                100 + idx as i64,
                Some("tenant-a"),
                "trigger",
                WorkerQueuePriority::Normal,
            ));
        }
        jobs.push(state(
            5,
            500,
            Some("tenant-b"),
            "trigger",
            WorkerQueuePriority::Normal,
        ));
        let mut sched = SchedulerState::new();
        let policy = SchedulerPolicy::deficit_round_robin(FairnessKey::Tenant);

        let first = sched
            .select(&snapshot_views(&jobs), &policy, 10_000)
            .unwrap();
        let second_jobs: Vec<_> = jobs
            .iter()
            .filter(|j| j.job_event_id != first.job_event_id)
            .cloned()
            .collect();
        let second = sched
            .select(&snapshot_views(&second_jobs), &policy, 10_001)
            .unwrap();
        let tenants = [first.fairness_key.clone(), second.fairness_key.clone()];
        assert!(
            tenants.contains(&"tenant-a".to_string()) && tenants.contains(&"tenant-b".to_string()),
            "expected both tenants represented in first two picks, got {tenants:?}",
        );
    }

    #[test]
    fn drr_respects_weighted_share_two_to_one() {
        // weight a=2, b=1 → over many rounds, a should be selected ~2x b.
        let now_ms = 1_000_000;
        let mut sched = SchedulerState::new();
        let policy = SchedulerPolicy::deficit_round_robin(FairnessKey::Tenant)
            .with_weight("tenant-a", 2)
            .with_weight("tenant-b", 1);

        let mut acount = 0;
        let mut bcount = 0;
        for _ in 0..120 {
            // Always provide one fresh ready job per tenant.
            let jobs = vec![
                state(
                    1,
                    now_ms,
                    Some("tenant-a"),
                    "trigger",
                    WorkerQueuePriority::Normal,
                ),
                state(
                    2,
                    now_ms,
                    Some("tenant-b"),
                    "trigger",
                    WorkerQueuePriority::Normal,
                ),
            ];
            let pick = sched
                .select(&snapshot_views(&jobs), &policy, now_ms)
                .unwrap();
            match pick.fairness_key.as_str() {
                "tenant-a" => acount += 1,
                "tenant-b" => bcount += 1,
                _ => unreachable!(),
            }
        }
        // Allow ±10% drift due to round boundary effects.
        let ratio = acount as f64 / bcount as f64;
        assert!(
            (1.8..=2.2).contains(&ratio),
            "expected ~2:1 selection ratio, got a={acount} b={bcount} ratio={ratio:.3}",
        );
    }

    #[test]
    fn drr_starvation_promotion_picks_old_job_when_threshold_exceeded() {
        let mut sched = SchedulerState::new();
        let policy =
            SchedulerPolicy::deficit_round_robin(FairnessKey::Tenant).with_starvation_age_ms(1_000);

        // First, drain credits for tenant-a.
        for _ in 0..3 {
            let jobs = vec![state(
                1,
                100,
                Some("tenant-a"),
                "trigger",
                WorkerQueuePriority::Normal,
            )];
            sched.select(&snapshot_views(&jobs), &policy, 200).unwrap();
        }

        // Now cold tenant-b has an ancient job; tenant-a has a fresh job. Even
        // though scheduler would normally rotate, the starvation rule should
        // pick the ancient tenant-b job.
        let jobs = vec![
            state(
                2,
                10,
                Some("tenant-b"),
                "trigger",
                WorkerQueuePriority::Normal,
            ),
            state(
                3,
                10_000,
                Some("tenant-a"),
                "trigger",
                WorkerQueuePriority::Normal,
            ),
        ];
        let pick = sched
            .select(&snapshot_views(&jobs), &policy, 20_000)
            .unwrap();
        assert_eq!(pick.fairness_key, "tenant-b");
        assert_eq!(sched.starvation_promotions_total(), 1);
    }

    #[test]
    fn drr_max_concurrent_per_key_blocks_hot_tenant() {
        let mut sched = SchedulerState::new();
        let policy = SchedulerPolicy::deficit_round_robin(FairnessKey::Tenant)
            .with_max_concurrent_per_key(1);

        // Pretend tenant-a is already at its cap.
        sched.note_claim_committed("tenant-a");

        // Both tenants have jobs ready; tenant-a is older (would normally win
        // under priority/FIFO). Scheduler must skip tenant-a and pick b.
        let jobs = vec![
            state(
                1,
                10,
                Some("tenant-a"),
                "trigger",
                WorkerQueuePriority::Normal,
            ),
            state(
                2,
                500,
                Some("tenant-b"),
                "trigger",
                WorkerQueuePriority::Normal,
            ),
        ];
        let pick = sched
            .select(&snapshot_views(&jobs), &policy, 1_000)
            .unwrap();
        assert_eq!(pick.fairness_key, "tenant-b");
    }

    #[test]
    fn drr_returns_none_when_all_keys_capped() {
        let mut sched = SchedulerState::new();
        let policy = SchedulerPolicy::deficit_round_robin(FairnessKey::Tenant)
            .with_max_concurrent_per_key(1);
        sched.note_claim_committed("tenant-a");
        sched.note_claim_committed("tenant-b");

        let jobs = vec![
            state(
                1,
                10,
                Some("tenant-a"),
                "trigger",
                WorkerQueuePriority::Normal,
            ),
            state(
                2,
                500,
                Some("tenant-b"),
                "trigger",
                WorkerQueuePriority::Normal,
            ),
        ];
        assert!(sched
            .select(&snapshot_views(&jobs), &policy, 1_000)
            .is_none());
        assert_eq!(sched.deferred_total_for("tenant-a"), 1);
        assert_eq!(sched.deferred_total_for("tenant-b"), 1);
    }

    #[test]
    fn drr_priority_within_a_tenant_still_holds() {
        let mut sched = SchedulerState::new();
        let policy = SchedulerPolicy::deficit_round_robin(FairnessKey::Tenant);
        let jobs = vec![
            state(
                1,
                100,
                Some("tenant-a"),
                "trigger-low",
                WorkerQueuePriority::Low,
            ),
            state(
                2,
                100,
                Some("tenant-a"),
                "trigger-high",
                WorkerQueuePriority::High,
            ),
        ];
        let pick = sched
            .select(&snapshot_views(&jobs), &policy, 1_000)
            .unwrap();
        assert_eq!(
            pick.job_event_id, 2,
            "high priority should win within a tenant"
        );
    }

    #[test]
    fn snapshot_includes_fairness_state_per_key() {
        let mut sched = SchedulerState::new();
        let policy =
            SchedulerPolicy::deficit_round_robin(FairnessKey::Tenant).with_weight("tenant-a", 3);

        for _ in 0..5 {
            let jobs = vec![
                state(
                    1,
                    100,
                    Some("tenant-a"),
                    "trigger",
                    WorkerQueuePriority::Normal,
                ),
                state(
                    2,
                    200,
                    Some("tenant-b"),
                    "trigger",
                    WorkerQueuePriority::Normal,
                ),
            ];
            sched.select(&snapshot_views(&jobs), &policy, 300).unwrap();
        }

        let states = vec![
            state(
                3,
                100,
                Some("tenant-a"),
                "trigger",
                WorkerQueuePriority::Normal,
            ),
            state(
                4,
                100,
                Some("tenant-b"),
                "trigger",
                WorkerQueuePriority::Normal,
            ),
        ];
        let ready = ready_stats_by_key(&states, &policy, 5_000);
        let snap = sched.snapshot(&policy, &ready);
        assert_eq!(snap.strategy, "drr");
        assert_eq!(snap.fairness_key, "tenant");
        let a = snap
            .keys
            .iter()
            .find(|k| k.fairness_key == "tenant-a")
            .unwrap();
        assert_eq!(a.weight, 3);
        let b = snap
            .keys
            .iter()
            .find(|k| k.fairness_key == "tenant-b")
            .unwrap();
        assert!(a.selected_total > b.selected_total);
        assert!(a.ready_jobs >= 1 && b.ready_jobs >= 1);
    }
}
