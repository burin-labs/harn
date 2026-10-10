//! Content-free diagnostic of the existing canonical publication replay.
//!
//! This reads a completed product run without launching another agent. Private
//! prose remains in the journal; only source linkage and owner decisions leave
//! this seam. Missing metadata is explicit, never inferred from visible text.

use std::path::Path;

use harn_session_store::{SessionStore, StoreError, StoreResult};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub use crate::llm::assistant_publication::Publication as AssistantPublicationDisposition;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AssistantPublicationRecord {
    pub source_event_id: Option<String>,
    pub call_role: Option<String>,
    pub call_stage: Option<String>,
    pub stage_metadata_present: bool,
    pub stage_metadata_valid: bool,
    pub metadata_present: bool,
    pub disposition: Option<AssistantPublicationDisposition>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SessionPublicationEvidence {
    pub schema: String,
    pub session_id: String,
    pub last_event_id: Option<u64>,
    pub chain_root_hash: Option<String>,
    pub stored_event_count: usize,
    pub assistant_message_count: usize,
    pub missing_source_count: usize,
    pub missing_call_role_count: usize,
    pub missing_call_stage_metadata_count: usize,
    pub unrecognized_call_stage_metadata_count: usize,
    pub missing_metadata_count: usize,
    pub unrecognized_metadata_count: usize,
    pub pending_count: usize,
    pub published_count: usize,
    pub withheld_count: usize,
    pub records: Vec<AssistantPublicationRecord>,
}

/// Open only an existing canonical store. Absence remains `None`; it cannot
/// create a database and then report a measured empty result.
pub async fn read_project_session_publication_evidence(
    project_root: &Path,
    session_id: &str,
) -> StoreResult<Option<SessionPublicationEvidence>> {
    let Some(store) = crate::stdlib::session_store::open_existing_canonical_store(project_root)
        .map_err(|error| StoreError::Backend(error.to_string()))?
    else {
        return Ok(None);
    };
    read_session_publication_evidence(&store, session_id).await
}

/// Replay through the transcript owner, with complete stable read coverage.
pub async fn read_session_publication_evidence(
    store: &dyn SessionStore,
    session_id: &str,
) -> StoreResult<Option<SessionPublicationEvidence>> {
    if session_id.trim().is_empty() {
        return Err(StoreError::Conflict(
            "publication evidence requires a nonempty session identity".into(),
        ));
    }
    let before = match store.describe(session_id).await {
        Ok(meta) => meta,
        Err(StoreError::NotFound(_)) => return Ok(None),
        Err(error) => return Err(error),
    };
    let events = store.read_all(session_id).await?;
    let after = store.describe(session_id).await?;
    validate_coverage(session_id, &before, &after, &events)?;
    let hydrated = crate::agent_session_journal::hydrate_events(events);
    let records: Vec<_> = hydrated
        .messages
        .iter()
        .enumerate()
        .filter(|(_, message)| {
            message.get("role").and_then(serde_json::Value::as_str) == Some("assistant")
        })
        .map(|(index, message)| {
            let (metadata_present, disposition) =
                crate::llm::assistant_publication::diagnostic_disposition(message);
            AssistantPublicationRecord {
                source_event_id: hydrated
                    .source_event_ids
                    .get(index)
                    .cloned()
                    .flatten()
                    .filter(|source| !source.trim().is_empty()),
                call_role: message
                    .get("_harn")
                    .filter(|facts| {
                        facts.get("kind").and_then(serde_json::Value::as_str) == Some("assistant")
                    })
                    .and_then(|facts| facts.get("call_role"))
                    .and_then(serde_json::Value::as_str)
                    .filter(|role| !role.trim().is_empty())
                    .map(str::to_owned),
                call_stage: message
                    .get("_harn")
                    .and_then(|facts| facts.get("call_stage"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned),
                stage_metadata_present: message
                    .get("_harn")
                    .is_some_and(|facts| facts.get("call_stage").is_some()),
                stage_metadata_valid: message.get("_harn").is_some_and(|facts| {
                    facts.get("kind").and_then(serde_json::Value::as_str) == Some("assistant")
                        && facts.get("call_stage").is_some_and(|stage| {
                            stage.is_null()
                                || stage.as_str().is_some_and(|stage| !stage.trim().is_empty())
                        })
                }),
                metadata_present,
                disposition,
            }
        })
        .collect();
    Ok(Some(SessionPublicationEvidence {
        schema: "harn.session_publication_evidence.v1".into(),
        session_id: session_id.into(),
        last_event_id: after.last_event_id,
        chain_root_hash: after.chain_root_hash,
        stored_event_count: after.event_count,
        assistant_message_count: records.len(),
        missing_source_count: records
            .iter()
            .filter(|record| record.source_event_id.is_none())
            .count(),
        missing_call_role_count: records
            .iter()
            .filter(|record| record.call_role.is_none())
            .count(),
        missing_call_stage_metadata_count: records
            .iter()
            .filter(|record| !record.stage_metadata_present)
            .count(),
        unrecognized_call_stage_metadata_count: records
            .iter()
            .filter(|record| record.stage_metadata_present && !record.stage_metadata_valid)
            .count(),
        missing_metadata_count: records
            .iter()
            .filter(|record| !record.metadata_present)
            .count(),
        unrecognized_metadata_count: records
            .iter()
            .filter(|record| record.metadata_present && record.disposition.is_none())
            .count(),
        pending_count: records
            .iter()
            .filter(|record| record.disposition == Some(AssistantPublicationDisposition::Pending))
            .count(),
        published_count: records
            .iter()
            .filter(|record| record.disposition == Some(AssistantPublicationDisposition::Published))
            .count(),
        withheld_count: records
            .iter()
            .filter(|record| record.disposition == Some(AssistantPublicationDisposition::Withheld))
            .count(),
        records,
    }))
}

fn validate_coverage(
    session_id: &str,
    before: &harn_session_store::SessionMeta,
    after: &harn_session_store::SessionMeta,
    events: &[harn_session_store::StoredEvent],
) -> StoreResult<()> {
    if before.id != session_id
        || after.id != session_id
        || events.iter().any(|event| event.session_id != session_id)
        || before.event_count != after.event_count
        || before.last_event_id != after.last_event_id
        || before.chain_root_hash != after.chain_root_hash
        || events.len() != after.event_count
        || events.last().map(|event| event.event_id) != after.last_event_id
    {
        return Err(StoreError::Conflict(
            "canonical publication evidence has incomplete or changing event coverage".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
