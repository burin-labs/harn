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

use crate::agent_sessions::session_event::{self, TranscriptKind};
use crate::value::VmError;

const MICROS_PER_USD: f64 = 1_000_000.0;
const READ_PAGE: usize = 512;
const RECORDED_THROUGH: &str = "harn.llm_spend_recorded_through";

/// Measured session cost; an unknown total retains its priced subtotal.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionLlmCost {
    pub known_cost_usd: f64,
    pub cost_usd: Option<f64>,
}

/// A hash-bound prefix fold that reads only newly appended session events.
#[derive(Clone, Debug, Default)]
pub struct SessionLlmCostCursor {
    session_id: Option<String>,
    event_id: Option<harn_session_store::EventId>,
    record_hash: Option<String>,
    event_count: usize,
    cost: SessionLlmCost,
}

impl SessionLlmCostCursor {
    pub async fn refresh(
        &mut self,
        project_root: &Path,
        session_id: &str,
    ) -> Result<SessionLlmCost, VmError> {
        let Some(store) =
            crate::stdlib::session_store::open_existing_canonical_store(project_root)?
        else {
            return Ok(SessionLlmCost::default());
        };
        self.refresh_from_store(&store, session_id).await
    }

    pub async fn refresh_from_store(
        &mut self,
        store: &dyn SessionStore,
        session_id: &str,
    ) -> Result<SessionLlmCost, VmError> {
        let meta = match store.describe(session_id).await {
            Ok(meta) => meta,
            Err(StoreError::NotFound(_)) => return Ok(SessionLlmCost::default()),
            Err(error) => return Err(spend_error("describe", session_id, error)),
        };
        let mut next = self.clone();
        if next.session_id.as_deref() != Some(session_id)
            || meta.last_event_id < next.event_id
            || meta.event_count < next.event_count
            || (meta.last_event_id == next.event_id && meta.chain_root_hash != next.record_hash)
        {
            next = Self {
                session_id: Some(session_id.to_string()),
                cost: SessionLlmCost {
                    known_cost_usd: 0.0,
                    cost_usd: Some(0.0),
                },
                ..Default::default()
            };
        }
        if meta.last_event_id != next.event_id {
            let mut from = next.event_id.map(|id| id.saturating_add(1));
            loop {
                let page = store
                    .read(
                        session_id,
                        ReadRange {
                            from_event_id: from,
                            to_event_id: meta.last_event_id,
                            limit: Some(READ_PAGE),
                        },
                    )
                    .await
                    .map_err(|error| spend_error("read", session_id, error))?;
                let received = !page.events.is_empty();
                for event in page.events {
                    if event.prev_hash != next.record_hash
                        || event.event_id != next.event_id.unwrap_or(0).saturating_add(1)
                    {
                        return Err(VmError::Runtime(
                            "session LLM cost: event prefix changed".into(),
                        ));
                    }
                    if let Some(record) = session_event::transcript_record(&event)
                        .filter(|record| record.kind() == Some(&TranscriptKind::LlmCall))
                    {
                        let cost = record.metadata().recorded_call_cost();
                        next.cost.known_cost_usd += cost.known_cost_usd;
                        next.cost.cost_usd = next
                            .cost
                            .cost_usd
                            .zip(cost.cost_usd)
                            .map(|(total, cost)| total + cost);
                    }
                    next.event_id = Some(event.event_id);
                    next.record_hash = Some(event.source_record_hash().to_string());
                    next.event_count += 1;
                }
                match page.next_cursor {
                    Some(_) if next.event_id == meta.last_event_id => break,
                    Some(cursor)
                        if received
                            && Some(cursor) == next.event_id.and_then(|id| id.checked_add(1)) =>
                    {
                        from = Some(cursor);
                    }
                    Some(_) => {
                        return Err(VmError::Runtime(
                            "session LLM cost: invalid event cursor".into(),
                        ))
                    }
                    None => break,
                }
            }
        }
        if next.event_id != meta.last_event_id
            || next.event_count != meta.event_count
            || next.record_hash != meta.chain_root_hash
        {
            return Err(VmError::Runtime(
                "session LLM cost: incomplete event prefix".into(),
            ));
        }
        let mut cost = next.cost.clone();
        // A budget charge without its supporting records cannot prove an exact price.
        if meta.usage_cost_usd_micros as f64 > cost.known_cost_usd * MICROS_PER_USD + 0.5 {
            cost.cost_usd = None;
        }
        *self = next;
        Ok(cost)
    }
}

pub async fn load_session_llm_cost(
    project_root: &Path,
    session_id: &str,
) -> Result<SessionLlmCost, VmError> {
    SessionLlmCostCursor::default()
        .refresh(project_root, session_id)
        .await
}

pub async fn load_session_llm_cost_from_store(
    store: &dyn SessionStore,
    session_id: &str,
) -> Result<SessionLlmCost, VmError> {
    SessionLlmCostCursor::default()
        .refresh_from_store(store, session_id)
        .await
}

/// What `session_id` has spent on LLM calls so far, in USD, per the canonical
/// store under `project_root`. A store with no such session has spent nothing.
pub async fn load_session_llm_spend_usd(
    project_root: &Path,
    session_id: &str,
) -> Result<f64, VmError> {
    // The writable open, not the read-only one: it waits on the store's init
    // lock, so a store another process is still creating is read once it is
    // initialized instead of failing the turn's admission.
    crate::stdlib::session_store::load_transient_session_spend(project_root, session_id).await
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
    let recorded_zero = meta.chain_root_hash.as_deref().is_some_and(|hash| {
        meta.attributes
            .get(RECORDED_THROUGH)
            .and_then(serde_json::Value::as_str)
            == Some(hash)
    });
    if meta.usage_cost_usd_micros > 0 || meta.event_count == 0 || recorded_zero {
        return Ok(meta.usage_cost_usd_micros as f64 / MICROS_PER_USD);
    }
    Ok(load_session_llm_cost_from_store(store, session_id)
        .await?
        .known_cost_usd)
}

/// Persist `spent_usd` as `session_id`'s cumulative LLM spend. A session the
/// store does not hold has nothing to resume, so there is nothing to record.
pub async fn record_session_llm_spend_usd(
    project_root: &Path,
    session_id: &str,
    spent_usd: f64,
) -> Result<(), VmError> {
    crate::stdlib::session_store::record_transient_session_spend(
        project_root,
        session_id,
        spent_usd,
    )
    .await
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
    let meta = match store.describe(session_id).await {
        Ok(meta) => meta,
        Err(StoreError::NotFound(_)) => return Ok(()),
        Err(error) => return Err(spend_error("describe", session_id, error)),
    };
    let update = UpdateSession {
        usage_cost_usd_micros: Some(micros),
        attributes: meta
            .chain_root_hash
            .map(|hash| (RECORDED_THROUGH.into(), serde_json::json!(hash)))
            .into_iter()
            .collect(),
        ..UpdateSession::default()
    };
    match store.update(session_id, update).await {
        Ok(_) | Err(StoreError::NotFound(_)) => Ok(()),
        Err(error) => Err(spend_error("update", session_id, error)),
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
    async fn recorded_zero_is_bound_to_the_history_it_measured() {
        let store = store_with_session("zero").await;
        store.append("zero", llm_call(0.0)).await.unwrap();
        record_session_llm_spend_usd_in_store(&store, "zero", 0.0)
            .await
            .unwrap();
        let meta = store.describe("zero").await.unwrap();
        let hash = meta
            .chain_root_hash
            .as_deref()
            .expect("recorded event chain");
        assert_eq!(meta.attributes[RECORDED_THROUGH].as_str(), Some(hash));
        assert_eq!(
            load_session_llm_spend_usd_from_store(&store, "zero")
                .await
                .unwrap(),
            0.0
        );
        store.append("zero", llm_call(0.25)).await.unwrap();
        assert_eq!(
            load_session_llm_spend_usd_from_store(&store, "zero")
                .await
                .unwrap(),
            0.25
        );
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

    #[tokio::test]
    async fn exact_cost_distinguishes_missing_measured_zero_and_unpriced_calls() {
        let store = store_with_session("priced").await;
        let mut cursor = SessionLlmCostCursor::default();
        assert_eq!(
            cursor.refresh_from_store(&store, "missing").await.unwrap(),
            SessionLlmCost::default()
        );
        assert_eq!(
            cursor.refresh_from_store(&store, "priced").await.unwrap(),
            SessionLlmCost {
                known_cost_usd: 0.0,
                cost_usd: Some(0.0)
            }
        );
        store.append("priced", llm_call(0.25)).await.unwrap();
        let priced = cursor.refresh_from_store(&store, "priced").await.unwrap();
        assert_eq!(priced.cost_usd, Some(0.25));
        // Refreshing the same prefix must not charge the call twice.
        assert_eq!(
            cursor.refresh_from_store(&store, "priced").await.unwrap(),
            priced
        );
        let mut unknown = llm_call(0.0);
        unknown.payload["transcript_event"]["metadata"] = serde_json::json!({
            "cost_usd": null, "known_cost_usd": 0.125,
            "accounting_status": "unknown", "unpriced_calls": 1,
        });
        store.append("priced", unknown).await.unwrap();
        let partial = cursor.refresh_from_store(&store, "priced").await.unwrap();
        assert_eq!(partial.known_cost_usd, 0.375);
        assert_eq!(partial.cost_usd, None);
        assert_eq!(
            load_session_llm_cost_from_store(&store, "priced")
                .await
                .unwrap(),
            partial
        );
        store.append("priced", llm_call(0.5)).await.unwrap();
        let later = cursor.refresh_from_store(&store, "priced").await.unwrap();
        assert_eq!(later.known_cost_usd, 0.875);
        assert_eq!(later.cost_usd, None);
    }

    #[tokio::test]
    async fn a_budget_charge_is_not_a_measured_price() {
        let store = store_with_session("charged").await;
        record_session_llm_spend_usd_in_store(&store, "charged", 1.25)
            .await
            .unwrap();
        let cost = load_session_llm_cost_from_store(&store, "charged")
            .await
            .unwrap();
        assert_eq!(cost.known_cost_usd, 0.0);
        assert_eq!(cost.cost_usd, None);
        assert_eq!(
            load_session_llm_spend_usd_from_store(&store, "charged")
                .await
                .unwrap(),
            1.25
        );
    }
}
