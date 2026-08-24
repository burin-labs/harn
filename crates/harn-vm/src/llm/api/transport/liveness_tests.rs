//! Deadline behaviour for `StreamLiveness`.
//!
//! Split out of `liveness.rs` rather than kept as an inline `mod tests`: the
//! repository's wall-clock lint classifies files by name, so a
//! `tokio::time::Instant` constructed for a virtual-time test would otherwise
//! read as a runtime wall-clock read in a runtime file.

use super::liveness::{StreamDeadlinePolicy, StreamLiveness};
use crate::value::{ProviderStreamDeadline, ProviderStreamPhase};
use std::time::Duration;

#[tokio::test(start_paused = true)]
async fn expired_total_deadline_beats_an_immediately_ready_read() {
    let mut liveness = StreamLiveness::new(
        "fixture",
        StreamDeadlinePolicy::for_test(
            Duration::from_secs(2),
            Duration::from_secs(10),
            Duration::from_secs(10),
        ),
        tokio::time::Instant::now(),
    );
    tokio::time::advance(Duration::from_secs(2)).await;

    let error = liveness
        .next_line(async { Ok(Some("data: ready".to_string())) })
        .await
        .expect_err("an already-expired total deadline must win");
    let failure = error
        .provider_stream_failure()
        .expect("typed stream failure");
    assert_eq!(failure.deadline, Some(ProviderStreamDeadline::Total));
    assert_eq!(failure.phase, ProviderStreamPhase::AwaitingFirstChunk);
}
