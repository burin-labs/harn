use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::SessionRecapSnapshot;

pub const SESSION_RECAP_ENRICHMENT_EXTENSION: &str = "harn.dev/session-recap-enrichment/v1";

const MAX_SUMMARY_BYTES: usize = 4_096;
const MAX_TURN_HEADLINES: usize = 64;
const MAX_TURN_HEADLINE_BYTES: usize = 256;
const MAX_EXTENSION_BYTES: usize = 16 * 1_024;

/// Optional presentation copy derived from one exact deterministic recap.
///
/// This value is decorative: it never replaces the recap's source facts and
/// is never included in the base projection hash.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionRecapEnrichment {
    pub source_projection_hash: String,
    pub summary: String,
    #[serde(default)]
    pub turn_headlines: Vec<SessionRecapTurnHeadline>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionRecapTurnHeadline {
    pub turn_id: String,
    pub headline: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionRecapEnrichmentFallbackReason {
    NotRequested,
    ProjectionMismatch,
    InvalidContent,
    BoundExceeded,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SessionRecapEnrichmentDisposition {
    Applied,
    DeterministicFallback {
        reason: SessionRecapEnrichmentFallbackReason,
    },
}

pub(super) fn apply_optional_enrichment(
    snapshot: &mut SessionRecapSnapshot,
    enrichment: Option<SessionRecapEnrichment>,
) -> SessionRecapEnrichmentDisposition {
    let Some(mut enrichment) = enrichment else {
        return fallback(SessionRecapEnrichmentFallbackReason::NotRequested);
    };
    if enrichment.source_projection_hash != snapshot.projection_hash {
        return fallback(SessionRecapEnrichmentFallbackReason::ProjectionMismatch);
    }

    let policy = crate::redact::current_policy();
    enrichment.summary = policy.redact_string(&enrichment.summary).into_owned();
    for item in &mut enrichment.turn_headlines {
        item.headline = policy.redact_string(&item.headline).into_owned();
    }
    if enrichment.summary.trim().is_empty()
        || enrichment
            .turn_headlines
            .iter()
            .any(|item| item.turn_id.trim().is_empty() || item.headline.trim().is_empty())
    {
        return fallback(SessionRecapEnrichmentFallbackReason::InvalidContent);
    }
    if enrichment.summary.len() > MAX_SUMMARY_BYTES
        || enrichment.turn_headlines.len() > MAX_TURN_HEADLINES
        || enrichment
            .turn_headlines
            .iter()
            .any(|item| item.headline.len() > MAX_TURN_HEADLINE_BYTES)
    {
        return fallback(SessionRecapEnrichmentFallbackReason::BoundExceeded);
    }

    let known_turns = snapshot
        .turns
        .iter()
        .map(|turn| turn.turn_id.as_str())
        .collect::<BTreeSet<_>>();
    let mut seen_turns = BTreeSet::new();
    if enrichment.turn_headlines.iter().any(|item| {
        !known_turns.contains(item.turn_id.as_str()) || !seen_turns.insert(item.turn_id.as_str())
    }) {
        return fallback(SessionRecapEnrichmentFallbackReason::InvalidContent);
    }

    let value = serde_json::to_value(&enrichment).expect("recap enrichment must serialize");
    if crate::canonical_json::to_vec(&value).len() > MAX_EXTENSION_BYTES {
        return fallback(SessionRecapEnrichmentFallbackReason::BoundExceeded);
    }
    snapshot
        .extensions
        .insert(SESSION_RECAP_ENRICHMENT_EXTENSION.to_string(), value);
    SessionRecapEnrichmentDisposition::Applied
}

const fn fallback(
    reason: SessionRecapEnrichmentFallbackReason,
) -> SessionRecapEnrichmentDisposition {
    SessionRecapEnrichmentDisposition::DeterministicFallback { reason }
}
