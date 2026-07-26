//! The rate governor's admission gate is loud (harn#5142, `provider_admission_gate`).
//!
//! When the circuit stays open through the admission-wait cap, the call
//! proceeds *without* the reservation the governor's back-pressure assumes is
//! in force. That used to happen with no event, no record, and no log, so the
//! 429 storm that follows read as a provider problem rather than as the
//! governor having given up.

use crate::agent_events::AgentEvent;
use crate::boundary::tests::CapturedEvents;
use crate::boundary::{BoundaryFailureKind, BoundaryId};
use crate::llm::rate_governor::{
    self,
    test_support::{GovernorDisabledGuard, GovernorEnabledGuard},
    GovernorOutcome, ThrottleSignal,
};

use super::provider_errors::await_governor_admission;

/// Drive the provider into an OPEN circuit with a long Retry-After window, so
/// every admission wait in the cap loop still sees it open.
///
/// The window is wall-clock; the test runs on tokio's paused clock, which
/// auto-advances *virtual* time through the gate's sleeps without moving the
/// wall clock the circuit is measured against. That is what makes the loop
/// finish instantly and still exhaust the cap.
fn wedge_circuit_open(provider: &str, org_key: &str) {
    for _ in 0..64 {
        rate_governor::gate(provider, org_key, 0);
        rate_governor::record_outcome(
            provider,
            org_key,
            GovernorOutcome::Throttled {
                signal: ThrottleSignal::RateLimit429,
                retry_after_ms: Some(600_000),
            },
        );
    }
    assert!(
        rate_governor::circuit_is_open(provider, org_key),
        "test setup failed to open the circuit",
    );
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn a_wedged_admission_gate_reports_before_proceeding_unreserved() {
    let _governor = GovernorEnabledGuard::on();
    wedge_circuit_open("acme", "org-1");
    let captured = CapturedEvents::install();

    let reserved = await_governor_admission("acme", "org-1", 1_000).await;
    assert!(
        !reserved,
        "an open circuit must not hand out a reservation the outcome path would release",
    );

    let events = captured.boundary_failures();
    assert_eq!(events.len(), 1, "got: {events:?}");
    match &events[0] {
        AgentEvent::BoundaryFailure {
            boundary,
            kind,
            owner,
            detail,
            ..
        } => {
            assert_eq!(*boundary, BoundaryId::ProviderAdmissionGate);
            assert_eq!(*kind, BoundaryFailureKind::Capped);
            assert_eq!(
                owner, "policy",
                "giving up on back-pressure is a policy call"
            );
            assert!(detail.contains("acme"), "{detail}");
            assert!(detail.contains("unreserved"), "{detail}");
        }
        other => panic!("expected a BoundaryFailure, got {other:?}"),
    }
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn a_healthy_gate_reserves_and_reports_nothing() {
    let _governor = GovernorEnabledGuard::on();
    let captured = CapturedEvents::install();

    assert!(
        await_governor_admission("acme", "org-1", 1_000).await,
        "a closed circuit must reserve a slot",
    );
    rate_governor::record_outcome("acme", "org-1", GovernorOutcome::Served);
    assert!(
        captured.boundary_failures().is_empty(),
        "a served call must not emit boundary noise",
    );
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn a_disabled_governor_reports_nothing() {
    // Shares the arming lock with every other governor test: the flag is
    // process-global, so reading it unsynchronized would race a sibling test
    // that arms it.
    let _governor = GovernorDisabledGuard::off();
    let captured = CapturedEvents::install();
    // With the governor off there is no back-pressure to abandon, so the early
    // `false` is not a boundary failure and must not look like one.
    assert!(!await_governor_admission("acme", "org-1", 1_000).await);
    assert!(captured.boundary_failures().is_empty());
}
