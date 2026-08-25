use crate::value::{
    ProviderStreamDeadline, ProviderStreamFailure, ProviderStreamFailureReason,
    ProviderStreamPhase, VmError,
};
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone, Copy, Debug)]
pub(super) struct StreamDeadlinePolicy {
    pub(super) total: Duration,
    pub(super) first_chunk: Duration,
    pub(super) idle: Duration,
}

impl StreamDeadlinePolicy {
    pub(super) fn from_payload(opts: &super::super::LlmRequestPayload) -> Self {
        let idle_secs = opts.idle_timeout.unwrap_or_else(|| {
            crate::stdlib::process::session_env_var("HARN_LLM_IDLE_TIMEOUT")
                .ok()
                .flatten()
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(30)
        });
        let first_chunk_secs =
            crate::stdlib::process::session_env_var("HARN_LLM_FIRST_TOKEN_TIMEOUT")
                .ok()
                .flatten()
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or_else(|| idle_secs.saturating_mul(4).max(120));
        Self {
            total: Duration::from_secs(opts.resolve_timeout()),
            first_chunk: Duration::from_secs(first_chunk_secs),
            idle: Duration::from_secs(idle_secs),
        }
    }

    #[cfg(test)]
    pub(super) fn for_test(total: Duration, first_chunk: Duration, idle: Duration) -> Self {
        Self {
            total,
            first_chunk,
            idle,
        }
    }
}

pub(super) struct StreamLiveness {
    provider: String,
    policy: StreamDeadlinePolicy,
    clock: Arc<dyn harn_clock::Clock>,
    started_ms: i64,
    last_activity_ms: i64,
    saw_chunk: bool,
    partial_output: bool,
    /// Instant the HTTP request was dispatched, threaded in so the first-frame
    /// stamp shares an origin with `client_wall_ms` and the two are
    /// subtractable. Deliberately separate from `started_ms`, which begins when
    /// body consumption starts and must keep owning the deadlines unchanged.
    request_origin: tokio::time::Instant,
    first_frame_ms: Option<u64>,
}

impl StreamLiveness {
    pub(super) fn new(
        provider: &str,
        policy: StreamDeadlinePolicy,
        request_origin: tokio::time::Instant,
    ) -> Self {
        let clock = harn_clock::RealClock::arc();
        let now_ms = clock.monotonic_ms();
        Self {
            provider: provider.to_string(),
            policy,
            clock,
            started_ms: now_ms,
            last_activity_ms: now_ms,
            saw_chunk: false,
            partial_output: false,
            request_origin,
            first_frame_ms: None,
        }
    }

    /// Stamp the arrival of the first well-formed provider frame. Idempotent:
    /// later frames do not move it.
    ///
    /// Called from the same place as `mark_partial_output`, which is reached
    /// only after a frame has parsed. That placement is load-bearing. Stamping
    /// on any received line would also stamp SSE comments, `event:` name lines,
    /// and gateway keepalives, so a keepalive arriving during prefill would
    /// report a first-frame latency near zero — a fabricated number wearing a
    /// measurement's clothes.
    pub(super) fn mark_first_frame(&mut self) {
        if self.first_frame_ms.is_none() {
            self.first_frame_ms = Some(crate::llm::first_token::duration_ms(
                self.request_origin.elapsed(),
            ));
        }
    }

    /// Latency to the first well-formed provider frame, or `None` when no frame
    /// ever parsed.
    pub(super) fn first_frame_ms(&self) -> Option<u64> {
        self.first_frame_ms
    }

    pub(super) fn phase(&self) -> ProviderStreamPhase {
        if self.saw_chunk {
            ProviderStreamPhase::Streaming
        } else {
            ProviderStreamPhase::AwaitingFirstChunk
        }
    }

    pub(super) fn mark_partial_output(&mut self) {
        self.partial_output = true;
    }

    pub(super) async fn next_line<F>(&mut self, read: F) -> Result<Option<String>, VmError>
    where
        F: Future<Output = std::io::Result<Option<String>>>,
    {
        let now_ms = self.clock.monotonic_ms();
        let total_elapsed = elapsed_since(self.started_ms, now_ms);
        let total_remaining = self.policy.total.saturating_sub(total_elapsed);
        let active_elapsed = if self.saw_chunk {
            elapsed_since(self.last_activity_ms, now_ms)
        } else {
            total_elapsed
        };
        let active_budget = if self.saw_chunk {
            self.policy.idle
        } else {
            self.policy.first_chunk
        };
        let active_remaining = active_budget.saturating_sub(active_elapsed);
        if total_remaining.is_zero() {
            return Err(self.failure(
                ProviderStreamFailureReason::Deadline,
                Some(ProviderStreamDeadline::Total),
                "total deadline elapsed before provider termination".to_string(),
            ));
        }
        if active_remaining.is_zero() {
            let deadline = if self.saw_chunk {
                ProviderStreamDeadline::Idle
            } else {
                ProviderStreamDeadline::FirstChunk
            };
            return Err(self.failure(
                ProviderStreamFailureReason::Deadline,
                Some(deadline),
                format!(
                    "{} deadline elapsed before provider termination",
                    deadline.as_str()
                ),
            ));
        }
        let (wait, deadline) = if total_remaining <= active_remaining {
            (total_remaining, ProviderStreamDeadline::Total)
        } else if self.saw_chunk {
            (active_remaining, ProviderStreamDeadline::Idle)
        } else {
            (active_remaining, ProviderStreamDeadline::FirstChunk)
        };
        let clock = self.clock.clone();
        tokio::pin!(read);
        tokio::select! {
            biased;
            result = &mut read => match result {
                Ok(Some(line)) => {
                    self.saw_chunk = true;
                    self.last_activity_ms = self.clock.monotonic_ms();
                    Ok(Some(line))
                }
                Ok(None) => Ok(None),
                Err(error) => {
                    let timed_out = error.kind() == std::io::ErrorKind::TimedOut;
                    Err(self.failure(
                        if timed_out {
                            ProviderStreamFailureReason::Deadline
                        } else {
                            ProviderStreamFailureReason::Read
                        },
                        timed_out.then_some(ProviderStreamDeadline::Total),
                        crate::egress::redact_diagnostic_text(&error.to_string()),
                    ))
                },
            },
            _ = clock.sleep(wait) => Err(self.failure(
                ProviderStreamFailureReason::Deadline,
                Some(deadline),
                format!("{} deadline elapsed before provider termination", deadline.as_str()),
            )),
        }
    }

    pub(super) fn premature_eof(&self, expected: &str) -> VmError {
        self.failure(
            ProviderStreamFailureReason::PrematureEof,
            None,
            format!("stream ended before {expected}"),
        )
    }

    fn failure(
        &self,
        reason: ProviderStreamFailureReason,
        deadline: Option<ProviderStreamDeadline>,
        detail: String,
    ) -> VmError {
        VmError::ProviderStreamFailure(Box::new(ProviderStreamFailure {
            provider: self.provider.clone(),
            phase: self.phase(),
            reason,
            deadline,
            partial: self.partial_output,
            detail,
        }))
    }
}

fn elapsed_since(earlier_ms: i64, later_ms: i64) -> Duration {
    Duration::from_millis(later_ms.saturating_sub(earlier_ms).max(0) as u64)
}
