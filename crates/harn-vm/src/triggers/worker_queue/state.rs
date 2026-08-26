use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::{TenantClaimScope, WorkerQueueClaimHandle, WorkerQueueJob, WorkerQueueResponseRecord};
use crate::triggers::scheduler::{self, SchedulableJob, SchedulerPolicy, SchedulerState};

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

    /// Select the next ready job under the active policy. Exclusions are
    /// operation-local and never alter durable queue state.
    pub(super) fn next_ready_job_with_scheduler(
        &self,
        scheduler_state: &mut SchedulerState,
        policy: &SchedulerPolicy,
        now_ms: i64,
        excluded_job_event_ids: &BTreeSet<u64>,
        tenant_scope: TenantClaimScope<'_>,
    ) -> Option<(&WorkerQueueJobState, scheduler::SchedulerSelection)> {
        let candidates: Vec<&WorkerQueueJobState> = self
            .jobs
            .iter()
            .filter(|job| {
                job.is_ready()
                    && !excluded_job_event_ids.contains(&job.job_event_id)
                    && match tenant_scope {
                        TenantClaimScope::Any => true,
                        TenantClaimScope::Untenanted => job.job.event.tenant_id.is_none(),
                        TenantClaimScope::Tenant(tenant_id) => {
                            job.job.event.tenant_id.as_ref() == Some(tenant_id)
                        }
                    }
            })
            .collect();
        if candidates.is_empty() {
            return None;
        }
        let views: Vec<SchedulableJob<'_>> = candidates
            .iter()
            .map(|state| SchedulableJob::from_state(state))
            .collect();

        let in_flight = scheduler::in_flight_by_key(&self.jobs, policy);
        scheduler_state.replace_in_flight(in_flight);

        let pick = scheduler_state.select(&views, policy, now_ms)?;
        candidates
            .into_iter()
            .find(|job| job.job_event_id == pick.job_event_id)
            .map(|job| (job, pick))
    }

    pub(super) fn active_claim_for(&self, job_event_id: u64) -> Option<&WorkerQueueClaimHandle> {
        self.jobs
            .iter()
            .find(|job| job.job_event_id == job_event_id)
            .and_then(|job| job.active_claim.as_ref())
    }
}
