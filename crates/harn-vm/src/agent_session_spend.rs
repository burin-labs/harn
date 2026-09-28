//! A session's cumulative LLM spend, durable across prompts and processes.
//!
//! A spend ceiling is a promise about a session, but the runtime enforces it
//! per cost scope ([`crate::install_llm_cost_budget_seeded`]), and a host opens
//! a new scope for every prompt and every process that resumes the session.
//! The canonical store's `usage_cost_usd_micros` is what carries the total
//! between them: a host seeds each scope from [`load_session_llm_spend_usd`]
//! and writes the scope's total back with [`record_session_llm_spend_usd`] when
//! the prompt ends. `session/list` reports the same field.
//!
//! Sessions recorded before that field was maintained hold 0 there. For those
//! the total is recovered from the costs on their recorded `llm_call` events,
//! so resuming an older session does not hand it a fresh allowance.

use std::path::Path;

use harn_session_store::{ReadRange, SessionStore, StoreError, UpdateSession};

use crate::agent_sessions::event_facts;
use crate::value::VmError;

const MICROS_PER_USD: f64 = 1_000_000.0;
const READ_PAGE: usize = 512;

/// What `session_id` has spent on LLM calls so far, in USD, per the canonical
/// store under `project_root`. A project with no store, or a store with no such
/// session, has spent nothing.
pub async fn load_session_llm_spend_usd(
    project_root: &Path,
    session_id: &str,
) -> Result<f64, VmError> {
    let Some(store) = crate::stdlib::session_store::open_existing_canonical_store(project_root)?
    else {
        return Ok(0.0);
    };
    load_session_llm_spend_usd_from_store(&store, session_id).await
}

/// Store-injected form of [`load_session_llm_spend_usd`].
pub async fn load_session_llm_spend_usd_from_store(
    store: &dyn SessionStore,
    session_id: &str,
) -> Result<f64, VmError> {
    let meta = match store.describe(session_id).await {
        Ok(meta) => meta,
        Err(StoreError::NotFound(_)) => return Ok(0.0),
        Err(error) => return Err(spend_error("describe", session_id, error)),
    };
    if meta.usage_cost_usd_micros > 0 || meta.event_count == 0 {
        return Ok(meta.usage_cost_usd_micros as f64 / MICROS_PER_USD);
    }
    recorded_llm_call_cost_usd(store, session_id).await
}

/// Persist `spent_usd` as `session_id`'s cumulative LLM spend. A session the
/// store does not hold has nothing to resume, so there is nothing to record.
pub async fn record_session_llm_spend_usd(
    project_root: &Path,
    session_id: &str,
    spent_usd: f64,
) -> Result<(), VmError> {
    let store = crate::stdlib::session_store::open_canonical_store(project_root)?;
    record_session_llm_spend_usd_in_store(&store, session_id, spent_usd).await
}

/// Store-injected form of [`record_session_llm_spend_usd`].
pub async fn record_session_llm_spend_usd_in_store(
    store: &dyn SessionStore,
    session_id: &str,
    spent_usd: f64,
) -> Result<(), VmError> {
    let micros = if spent_usd.is_finite() && spent_usd > 0.0 {
        (spent_usd * MICROS_PER_USD).round() as u64
    } else {
        0
    };
    let update = UpdateSession {
        usage_cost_usd_micros: Some(micros),
        ..UpdateSession::default()
    };
    match store.update(session_id, update).await {
        Ok(_) | Err(StoreError::NotFound(_)) => Ok(()),
        Err(error) => Err(spend_error("update", session_id, error)),
    }
}

async fn recorded_llm_call_cost_usd(
    store: &dyn SessionStore,
    session_id: &str,
) -> Result<f64, VmError> {
    let mut total = 0.0;
    let mut from = None;
    loop {
        let page = store
            .read(
                session_id,
                ReadRange {
                    from_event_id: from,
                    limit: Some(READ_PAGE),
                    ..ReadRange::default()
                },
            )
            .await
            .map_err(|error| spend_error("read", session_id, error))?;
        total += page
            .events
            .iter()
            .filter(|event| event.kind.discriminator() == "llm_call")
            .filter_map(|event| event_facts::f64_at(&event.payload, event_facts::COST_USD))
            .filter(|cost| cost.is_finite() && *cost > 0.0)
            .sum::<f64>();
        match page.next_cursor {
            Some(cursor) => from = Some(cursor),
            None => return Ok(total),
        }
    }
}

fn spend_error(operation: &str, session_id: &str, error: StoreError) -> VmError {
    VmError::Runtime(format!(
        "session LLM spend: canonical store {operation} {session_id}: {error}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use harn_session_store::{AppendEvent, CreateSession, SessionEventKind, SqliteSessionStore};

    fn llm_call(cost_usd: f64) -> AppendEvent {
        AppendEvent::new(
            SessionEventKind::Custom {
                custom_type: "llm_call".to_string(),
            },
            serde_json::json!({
                "transcript_event": {"kind": "llm_call", "metadata": {"cost_usd": cost_usd}}
            }),
        )
    }

    async fn store_with_session(id: &str) -> SqliteSessionStore {
        let store = SqliteSessionStore::open_in_memory().expect("store");
        store
            .create(CreateSession {
                id: Some(id.to_string()),
                ..CreateSession::default()
            })
            .await
            .expect("create");
        store
    }

    #[tokio::test]
    async fn recorded_spend_round_trips_through_the_session_row() {
        let store = store_with_session("s").await;
        record_session_llm_spend_usd_in_store(&store, "s", 1.27)
            .await
            .expect("record");
        assert_eq!(
            store.describe("s").await.unwrap().usage_cost_usd_micros,
            1_270_000
        );
        let spent = load_session_llm_spend_usd_from_store(&store, "s")
            .await
            .unwrap();
        assert!((spent - 1.27).abs() < 1e-9, "{spent}");
    }

    #[tokio::test]
    async fn a_session_without_a_recorded_total_is_charged_its_llm_call_costs() {
        // The session every pre-fix process left behind: calls recorded, the
        // row's total still 0. Reading 0 would reopen the whole allowance.
        let store = store_with_session("old").await;
        for cost in [0.25, 0.5] {
            store.append("old", llm_call(cost)).await.expect("append");
        }
        let spent = load_session_llm_spend_usd_from_store(&store, "old")
            .await
            .unwrap();
        assert!((spent - 0.75).abs() < 1e-9, "{spent}");
    }

    #[tokio::test]
    async fn an_unknown_session_has_spent_nothing_and_records_nothing() {
        let store = SqliteSessionStore::open_in_memory().expect("store");
        assert_eq!(
            load_session_llm_spend_usd_from_store(&store, "missing")
                .await
                .unwrap(),
            0.0
        );
        record_session_llm_spend_usd_in_store(&store, "missing", 1.0)
            .await
            .expect("a missing row is not an error");
    }
}
