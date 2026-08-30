use serde_json::json;

use super::*;

fn snapshot() -> SessionRecapSnapshot {
    SessionRecapSnapshot {
        schema_version: SESSION_RECAP_SCHEMA_VERSION,
        session_id: "session-1".to_string(),
        query: SessionRecapQuery::for_session("session-1"),
        cursor: SessionRecapCursor::default(),
        coverage: SessionRecapCoverage::default(),
        source: SessionRecapSource::default(),
        content_hash: "sha256:content".to_string(),
        projection_hash: "sha256:projection".to_string(),
        turns: vec![PromptTurnRecap {
            turn_id: "turn-1".to_string(),
            run_id: "run-1".to_string(),
            state: RecapCompletionState::Complete,
            prompts: Vec::new(),
            iterations: Vec::new(),
            terminal: None,
            source_event_ids: Vec::new(),
        }],
        extensions: Default::default(),
    }
}

fn candidate(source_projection_hash: &str) -> SessionRecapEnrichment {
    SessionRecapEnrichment {
        source_projection_hash: source_projection_hash.to_string(),
        summary: "One completed prompt turn.".to_string(),
        turn_headlines: vec![SessionRecapTurnHeadline {
            turn_id: "turn-1".to_string(),
            headline: "Finished the requested work".to_string(),
        }],
    }
}

#[test]
fn matching_bounded_enrichment_attaches_without_changing_projection_identity() {
    let mut recap = snapshot();
    let base_hash = recap.projection_hash.clone();

    assert_eq!(
        recap.apply_optional_enrichment(Some(candidate(&base_hash))),
        SessionRecapEnrichmentDisposition::Applied
    );
    assert_eq!(recap.projection_hash, base_hash);
    assert_eq!(
        recap.extensions[SESSION_RECAP_ENRICHMENT_EXTENSION]["sourceProjectionHash"],
        json!(base_hash)
    );
    assert_eq!(
        recap.extensions[SESSION_RECAP_ENRICHMENT_EXTENSION]["turnHeadlines"][0]["turnId"],
        json!("turn-1")
    );
}

#[test]
fn stale_enrichment_falls_back_to_the_unchanged_deterministic_recap() {
    let mut recap = snapshot();
    let before = recap.clone();

    assert_eq!(
        recap.apply_optional_enrichment(Some(candidate("sha256:stale"))),
        SessionRecapEnrichmentDisposition::DeterministicFallback {
            reason: SessionRecapEnrichmentFallbackReason::ProjectionMismatch,
        }
    );
    assert_eq!(recap, before);
}

#[test]
fn malformed_or_unbounded_enrichment_cannot_displace_the_base_recap() {
    let cases = [
        (
            SessionRecapEnrichment {
                summary: " ".to_string(),
                ..candidate("sha256:projection")
            },
            SessionRecapEnrichmentFallbackReason::InvalidContent,
        ),
        (
            SessionRecapEnrichment {
                summary: "x".repeat(4_097),
                ..candidate("sha256:projection")
            },
            SessionRecapEnrichmentFallbackReason::BoundExceeded,
        ),
        (
            SessionRecapEnrichment {
                turn_headlines: vec![SessionRecapTurnHeadline {
                    turn_id: "unknown-turn".to_string(),
                    headline: "Invented grouping".to_string(),
                }],
                ..candidate("sha256:projection")
            },
            SessionRecapEnrichmentFallbackReason::InvalidContent,
        ),
        (
            SessionRecapEnrichment {
                turn_headlines: vec![
                    SessionRecapTurnHeadline {
                        turn_id: "turn-1".to_string(),
                        headline: "First headline".to_string(),
                    },
                    SessionRecapTurnHeadline {
                        turn_id: "turn-1".to_string(),
                        headline: "Conflicting headline".to_string(),
                    },
                ],
                ..candidate("sha256:projection")
            },
            SessionRecapEnrichmentFallbackReason::InvalidContent,
        ),
    ];

    for (candidate, reason) in cases {
        let mut recap = snapshot();
        let before = recap.clone();
        assert_eq!(
            recap.apply_optional_enrichment(Some(candidate)),
            SessionRecapEnrichmentDisposition::DeterministicFallback { reason }
        );
        assert_eq!(recap, before);
    }
}

#[test]
fn missing_enrichment_is_an_explicit_spend_free_fallback() {
    let mut recap = snapshot();
    let before = recap.clone();
    assert_eq!(
        recap.apply_optional_enrichment(None),
        SessionRecapEnrichmentDisposition::DeterministicFallback {
            reason: SessionRecapEnrichmentFallbackReason::NotRequested,
        }
    );
    assert_eq!(recap, before);
}

#[test]
fn enrichment_text_is_redacted_before_it_becomes_public_recap_data() {
    let mut recap = snapshot();
    let projection_hash = recap.projection_hash.clone();
    let mut enrichment = candidate(&projection_hash);
    enrichment.summary = "Used sk-proj-abcdefghijklmnopqrstuvwxyz1234567890 to finish.".to_string();
    enrichment.turn_headlines[0].headline =
        "Token sk-proj-abcdefghijklmnopqrstuvwxyz1234567890 worked".to_string();

    assert_eq!(
        recap.apply_optional_enrichment(Some(enrichment)),
        SessionRecapEnrichmentDisposition::Applied
    );
    let public = &recap.extensions[SESSION_RECAP_ENRICHMENT_EXTENSION];
    assert!(!public.to_string().contains("abcdefghijklmnopqrstuvwxyz"));
    assert!(public.to_string().contains("<redacted:openai_key:"));
}
