//! How a branch run ended, as one machine-readable verdict.
//!
//! [`start_branch`](super::start_branch) is the preflight: it refuses a tape,
//! K or state root it cannot honor before anything executes. The host then
//! runs the program through [`BranchScope::run_bounded`], which also ends on
//! a stop or a timeout, and reads [`BranchGuard::verdict`].

use std::future::Future;
use std::time::Duration;

use serde::Serialize;

use super::{BranchGuard, BranchReport, BranchScope};

pub const BRANCH_VERDICT_SCHEMA: &str = "harn.branch_verdict.v1";

/// What ended a bounded branch run.
pub enum Bounded<T> {
    Finished(T),
    Stopped,
    TimedOut,
}

impl BranchScope {
    /// Run `future` in the branch until it finishes, `stop` resolves, or
    /// `timeout` passes. A stopped or timed-out run is dropped, so nothing it
    /// started continues.
    pub async fn run_bounded<F: Future>(
        &self,
        future: F,
        stop: impl Future<Output = ()>,
        timeout: Option<Duration>,
    ) -> Bounded<F::Output> {
        let deadline = async {
            match timeout {
                Some(timeout) => tokio::time::sleep(timeout).await,
                None => std::future::pending().await,
            }
        };
        // A stop is a control event: it wins over work that is also ready.
        tokio::select! {
            biased;
            () = stop => Bounded::Stopped,
            () = deadline => Bounded::TimedOut,
            output = self.run(future) => Bounded::Finished(output),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BranchStatus {
    /// The prefix replayed and the live tail finished.
    Completed,
    /// The prefix stopped matching its recording.
    Diverged,
    /// The run failed in its live tail.
    Failed,
    Stopped,
    TimedOut,
}

/// How faithfully one model call was reproduced.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StepFidelity {
    /// Answered from the recording, with its tool calls.
    Served,
    /// Sent to the provider.
    Live,
    /// Where the prefix stopped matching.
    Diverged,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BranchStep {
    /// 1-based model call.
    pub call: u64,
    pub fidelity: StepFidelity,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BranchVerdict {
    pub schema: &'static str,
    pub status: BranchStatus,
    pub steps: Vec<BranchStep>,
    pub report: BranchReport,
}

impl BranchGuard {
    /// The verdict for a run that ended as `ended`, where `Finished` holds
    /// whether the program succeeded. A stopped or timed-out run is not an
    /// under-run: it reports how far it got.
    pub fn verdict(&self, ended: Bounded<bool>) -> BranchVerdict {
        let report = match ended {
            Bounded::Finished(_) => self.finish(),
            Bounded::Stopped | Bounded::TimedOut => self.report(),
        };
        let status = match (&report.divergence, ended) {
            (Some(_), _) => BranchStatus::Diverged,
            (None, Bounded::Stopped) => BranchStatus::Stopped,
            (None, Bounded::TimedOut) => BranchStatus::TimedOut,
            (None, Bounded::Finished(true)) => BranchStatus::Completed,
            (None, Bounded::Finished(false)) => BranchStatus::Failed,
        };
        BranchVerdict {
            schema: BRANCH_VERDICT_SCHEMA,
            status,
            steps: steps(&report),
            report,
        }
    }
}

fn steps(report: &BranchReport) -> Vec<BranchStep> {
    let served = report.served_calls;
    let mut steps: Vec<BranchStep> = (1..=served)
        .map(|call| BranchStep {
            call,
            fidelity: StepFidelity::Served,
        })
        .chain((served + 1..=served + report.live_calls).map(|call| BranchStep {
            call,
            fidelity: StepFidelity::Live,
        }))
        .collect();
    if let Some(divergence) = &report.divergence {
        // A tool divergence names the served call that requested the tool.
        match steps.iter_mut().find(|step| step.call == divergence.call) {
            Some(step) => step.fidelity = StepFidelity::Diverged,
            None => steps.push(BranchStep {
                call: divergence.call,
                fidelity: StepFidelity::Diverged,
            }),
        }
    }
    steps
}
