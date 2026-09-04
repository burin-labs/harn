use std::sync::{Arc, Mutex};

use super::{BoundaryFailure, BoundaryFailureKind, BoundaryId};
use crate::agent_events::{
    reset_all_sinks, reset_wildcard_sinks, unregister_wildcard_sink, AgentEvent, AgentEventSink,
    WildcardSinkHandle,
};

/// Captures every emitted event regardless of session, which is what a boundary
/// funnel needs: most drop sites are deep runtime code with no session in hand,
/// so their failures carry an empty session id.
pub(crate) struct CapturedEvents {
    events: Arc<Mutex<Vec<AgentEvent>>>,
    handle: WildcardSinkHandle,
}

struct CapturingSink(Arc<Mutex<Vec<AgentEvent>>>);

impl AgentEventSink for CapturingSink {
    fn handle_event(&self, event: &AgentEvent) {
        self.0
            .lock()
            .expect("capture mutex poisoned")
            .push(event.clone());
    }
}

impl CapturedEvents {
    pub(crate) fn install() -> Self {
        reset_all_sinks();
        reset_wildcard_sinks();
        let events: Arc<Mutex<Vec<AgentEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let handle =
            crate::agent_events::register_wildcard_sink(Arc::new(CapturingSink(events.clone())));
        Self { events, handle }
    }

    pub(crate) fn boundary_failures(&self) -> Vec<AgentEvent> {
        self.events
            .lock()
            .expect("capture mutex poisoned")
            .iter()
            .filter(|event| matches!(event, AgentEvent::BoundaryFailure { .. }))
            .cloned()
            .collect()
    }
}

impl Drop for CapturedEvents {
    fn drop(&mut self) {
        unregister_wildcard_sink(self.handle);
    }
}

/// Destructure a captured failure into the fields a test asserts on.
fn parts(event: &AgentEvent) -> (BoundaryId, BoundaryFailureKind, &str, &str, usize, bool) {
    match event {
        AgentEvent::BoundaryFailure {
            boundary,
            kind,
            owner,
            detail,
            dropped_count,
            unreported,
            ..
        } => (
            *boundary,
            *kind,
            owner.as_str(),
            detail.as_str(),
            *dropped_count,
            *unreported,
        ),
        other => panic!("expected a BoundaryFailure, got {other:?}"),
    }
}

#[test]
fn report_puts_a_typed_failure_on_the_bus_with_a_derived_owner() {
    let captured = CapturedEvents::install();
    BoundaryFailure::new(
        BoundaryId::TextToolParse,
        BoundaryFailureKind::Dropped,
        "swallowed a <tool> block",
    )
    .with_excerpt("<tool>read_file</tool>")
    .with_count(3)
    .in_session("s1")
    .report();

    let events = captured.boundary_failures();
    assert_eq!(events.len(), 1, "got: {events:?}");
    let (boundary, kind, owner, detail, count, unreported) = parts(&events[0]);
    assert_eq!(boundary, BoundaryId::TextToolParse);
    assert_eq!(kind, BoundaryFailureKind::Dropped);
    assert_eq!(owner, "harness");
    assert_eq!(detail, "swallowed a <tool> block");
    assert_eq!(count, 3);
    assert!(!unreported);
    assert_eq!(events[0].session_id(), "s1");
    match &events[0] {
        AgentEvent::BoundaryFailure {
            excerpt,
            dropped_bytes,
            ..
        } => {
            assert_eq!(excerpt.as_deref(), Some("<tool>read_file</tool>"));
            assert_eq!(*dropped_bytes, "<tool>read_file</tool>".len());
        }
        other => panic!("expected a BoundaryFailure, got {other:?}"),
    }
}

/// The load-bearing property: building the record and then losing it does not
/// produce silence. In a debug build the omission also fails the test, which is
/// what makes "forgot to report" a bug caught in CI rather than in production.
#[test]
fn a_failure_dropped_without_report_still_reaches_the_bus() {
    let captured = CapturedEvents::install();
    let outcome = crate::test_panic_silence::catch_unwind_silently(|| {
        let _forgotten = BoundaryFailure::new(
            BoundaryId::VisibleTextSanitize,
            BoundaryFailureKind::Truncated,
            "stripped narration",
        );
    });

    assert_eq!(
        outcome.is_err(),
        cfg!(debug_assertions),
        "the backstop must fail a debug build and stay quiet in release",
    );
    let events = captured.boundary_failures();
    assert_eq!(events.len(), 1, "got: {events:?}");
    let (boundary, _, _, detail, _, unreported) = parts(&events[0]);
    assert_eq!(boundary, BoundaryId::VisibleTextSanitize);
    assert_eq!(detail, "stripped narration");
    assert!(
        unreported,
        "a backstopped failure must be distinguishable from a reported one",
    );
}

/// A boundary that discovers several losses in one pass collects them in a
/// plain `Vec` and reports them in discovery order. No wrapper type is needed:
/// each element carries its own backstop, so losing the vec is still loud.
#[test]
fn a_batch_of_failures_reports_in_discovery_order() {
    let captured = CapturedEvents::install();
    let batch: Vec<BoundaryFailure> = ["first", "second", "third"]
        .into_iter()
        .map(|detail| {
            BoundaryFailure::new(
                BoundaryId::ResponseContentExtraction,
                BoundaryFailureKind::Unrecognized,
                detail,
            )
        })
        .collect();
    for failure in batch {
        failure.report();
    }

    let events = captured.boundary_failures();
    let details: Vec<String> = events
        .iter()
        .map(|event| parts(event).3.to_string())
        .collect();
    assert_eq!(details, vec!["first", "second", "third"]);
    assert!(events.iter().all(|event| !parts(event).5));
}

#[test]
fn an_abandoned_batch_still_emits_through_the_per_failure_backstop() {
    let captured = CapturedEvents::install();
    let _ = crate::test_panic_silence::catch_unwind_silently(|| {
        let _abandoned = [BoundaryFailure::new(
            BoundaryId::HostEventIngest,
            BoundaryFailureKind::Unrecognized,
            "abandoned",
        )];
    });

    let events = captured.boundary_failures();
    assert_eq!(events.len(), 1, "got: {events:?}");
    assert!(parts(&events[0]).5, "expected the backstop flag");
}

#[test]
fn every_boundary_id_has_a_stable_snake_case_wire_string() {
    for boundary in BoundaryId::ALL {
        let wire = boundary.as_str();
        let encoded = serde_json::to_string(&boundary).expect("boundary serializes");
        assert_eq!(encoded, format!("\"{wire}\""));
        let decoded: BoundaryId = serde_json::from_str(&encoded).expect("boundary round-trips");
        assert_eq!(decoded, boundary);
        assert_eq!(boundary.to_string(), wire);
    }
    // `ALL` must actually be all of them: the registry gate reads this list to
    // decide which boundaries need an entry, so a variant missing from it would
    // be a boundary the gate never checks.
    let distinct: std::collections::BTreeSet<&str> =
        BoundaryId::ALL.iter().map(|id| id.as_str()).collect();
    assert_eq!(distinct.len(), BoundaryId::ALL.len());
}

#[test]
fn every_kind_has_a_stable_wire_string_and_attributes_away_from_the_model() {
    for kind in BoundaryFailureKind::ALL {
        let wire = kind.as_str();
        let encoded = serde_json::to_string(&kind).expect("kind serializes");
        assert_eq!(encoded, format!("\"{wire}\""));
        let decoded: BoundaryFailureKind =
            serde_json::from_str(&encoded).expect("kind round-trips");
        assert_eq!(decoded, kind);
        // A boundary failure is by definition something the harness or a policy
        // did to the model's output. Attributing one to `agent` would recreate
        // the misattribution #5142 exists to end.
        assert!(
            matches!(kind.owner(), "harness" | "policy"),
            "{kind} attributes to {}",
            kind.owner(),
        );
    }
    assert_eq!(BoundaryFailureKind::Capped.owner(), "policy");
}

#[test]
fn a_long_excerpt_is_truncated_but_the_full_size_is_kept() {
    let captured = CapturedEvents::install();
    let long = "x".repeat(5000);
    BoundaryFailure::new(
        BoundaryId::ProviderAdmissionGate,
        BoundaryFailureKind::Capped,
        "cap",
    )
    .with_excerpt(&long)
    .report();

    let events = captured.boundary_failures();
    match &events[0] {
        AgentEvent::BoundaryFailure {
            excerpt,
            dropped_bytes,
            ..
        } => {
            assert_eq!(
                excerpt.as_deref().map(str::len),
                Some(super::MAX_EXCERPT_CHARS)
            );
            assert_eq!(*dropped_bytes, 5000, "the untruncated size must survive");
        }
        other => panic!("expected a BoundaryFailure, got {other:?}"),
    }
}
