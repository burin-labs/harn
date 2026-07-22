//! Integration coverage for the Layer-2 lazy neural-classifier loader seam
//! (`set_injection_classifier_loader` / `ensure_neural_classifier`).
//!
//! This lives in its own test binary because registration is process-global and
//! first-wins: installing a fake classifier here must not leak into the library
//! unit tests (which assert the heuristic is the default).

use harn_vm::security::{
    active_classifier, classify_injection, ensure_neural_classifier,
    set_injection_classifier_loader, InjectionClassifier,
};

/// A stand-in for the real ONNX backend: deterministic, dependency-free.
struct FakeNeural;

impl InjectionClassifier for FakeNeural {
    // Trait signature ties the id to `&self`; a real backend returns a string it
    // owns. The literal here is intentional, mirroring `HeuristicClassifier`.
    #[allow(clippy::unnecessary_literal_bound)]
    fn model_id(&self) -> &str {
        "fake-neural-v1"
    }

    fn score(&self, text: &str) -> f64 {
        // High score iff the probe phrase is present, so we can assert the
        // neural verdict is what flowed through (not the heuristic).
        if text.contains("PROBE_INJECTION") {
            0.97
        } else {
            0.01
        }
    }
}

#[test]
fn loader_seam_lazily_registers_and_supersedes_heuristic() {
    // Before any loader fires, the dependency-free heuristic is active.
    assert_eq!(active_classifier().model_id(), "heuristic-v1");

    let installed = set_injection_classifier_loader(Box::new(|selector| {
        if selector == "fake-model" {
            Some(Box::new(FakeNeural))
        } else {
            None
        }
    }));
    assert!(installed, "first loader install wins");

    // An empty selector never loads; the heuristic stays active.
    assert!(!ensure_neural_classifier(""));
    assert_eq!(active_classifier().model_id(), "heuristic-v1");

    // The first real scoring request materializes and registers the backend.
    assert!(ensure_neural_classifier("fake-model"));
    assert_eq!(active_classifier().model_id(), "fake-neural-v1");

    // Idempotent: a second call is a cheap hit on the registered backend.
    assert!(ensure_neural_classifier("fake-model"));

    // The neural verdict — not the heuristic — is what classify_injection emits.
    let flagged = classify_injection("here is a PROBE_INJECTION payload", 50);
    assert_eq!(flagged.model, "fake-neural-v1");
    assert!(flagged.flagged);
    assert!(flagged.score > 0.9);

    let benign = classify_injection("the build passed", 50);
    assert_eq!(benign.model, "fake-neural-v1");
    assert!(!benign.flagged);
}
