use std::collections::{HashMap, VecDeque};
use std::hash::Hash;
use std::sync::{Arc, Mutex};
use std::time::Duration as StdDuration;

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex as AsyncMutex;

use crate::connectors::{ConnectorError, MetricsRegistry};
use crate::event_log::{AnyEventLog, EventLog, LogEvent, Topic};

use super::{TRIGGER_INBOX_CLAIMS_TOPIC, TRIGGER_INBOX_LEGACY_TOPIC};

pub const DEFAULT_INBOX_RETENTION_DAYS: u32 = 7;
const CLAIM_EVENT_KIND: &str = "dedupe_claim";
const HOT_CACHE_LIMIT: usize = 4096;

#[derive(Clone)]
pub struct InboxIndex {
    event_log: Arc<AnyEventLog>,
    topic: Topic,
    metrics: Arc<MetricsRegistry>,
    entries: Arc<Mutex<HashMap<InboxKey, InboxEntry>>>,
    hot: Arc<Mutex<HotCache>>,
    claim_lock: Arc<AsyncMutex<()>>,
}

impl InboxIndex {
    pub async fn new(
        event_log: Arc<AnyEventLog>,
        metrics: Arc<MetricsRegistry>,
    ) -> Result<Self, ConnectorError> {
        let topic =
            Topic::new(TRIGGER_INBOX_CLAIMS_TOPIC).expect("trigger inbox claims topic is valid");
        let records = event_log
            .read_range(&topic, None, usize::MAX)
            .await
            .map_err(ConnectorError::from)?;
        let legacy_topic =
            Topic::new(TRIGGER_INBOX_LEGACY_TOPIC).expect("legacy trigger inbox topic is valid");
        let legacy_records = event_log
            .read_range(&legacy_topic, None, usize::MAX)
            .await
            .map_err(ConnectorError::from)?;
        let now_ms = now_ms();
        let mut entries = HashMap::new();
        let mut expired = 0u64;
        rehydrate_claims(records, now_ms, &mut entries, &mut expired)?;
        // Soft migration for pre-split logs that still hold claim records in
        // the legacy mixed inbox topic.
        rehydrate_claims(legacy_records, now_ms, &mut entries, &mut expired)?;
        metrics.record_inbox_expired_entries(expired);
        metrics.set_inbox_active_entries(entries.len());
        Ok(Self {
            event_log,
            topic,
            metrics,
            entries: Arc::new(Mutex::new(entries)),
            hot: Arc::new(Mutex::new(HotCache::default())),
            claim_lock: Arc::new(AsyncMutex::new(())),
        })
    }

    pub async fn insert_if_new(
        &self,
        binding_id: &str,
        dedupe_key: &str,
        ttl: StdDuration,
    ) -> Result<bool, ConnectorError> {
        let now_ms = now_ms();
        let expires_at_ms = expiry_ms(now_ms, ttl);
        let key = InboxKey::new(binding_id.to_string(), dedupe_key.to_string());

        if self
            .hot
            .lock()
            .expect("inbox hot cache poisoned")
            .contains(&key, now_ms)
        {
            self.metrics.record_inbox_duplicate_fast_path();
            return Ok(false);
        }

        let _guard = self.claim_lock.lock().await;
        self.sweep_expired(now_ms);

        if self
            .hot
            .lock()
            .expect("inbox hot cache poisoned")
            .contains(&key, now_ms)
        {
            self.metrics.record_inbox_duplicate_fast_path();
            return Ok(false);
        }

        {
            let entries = self.entries.lock().expect("inbox entries poisoned");
            if entries
                .get(&key)
                .is_some_and(|entry| entry.expires_at_ms > now_ms)
            {
                drop(entries);
                self.hot
                    .lock()
                    .expect("inbox hot cache poisoned")
                    .insert(key, expires_at_ms);
                self.metrics.record_inbox_duplicate_durable();
                return Ok(false);
            }
        }

        let payload = serde_json::to_value(InboxClaimRecord {
            binding_id: binding_id.to_string(),
            dedupe_key: dedupe_key.to_string(),
            expires_at_ms,
        })
        .map_err(ConnectorError::from)?;
        self.event_log
            .append(
                &self.topic,
                crate::event_log::LogEvent::new(CLAIM_EVENT_KIND, payload),
            )
            .await
            .map_err(ConnectorError::from)?;

        self.entries
            .lock()
            .expect("inbox entries poisoned")
            .insert(key.clone(), InboxEntry { expires_at_ms });
        self.hot
            .lock()
            .expect("inbox hot cache poisoned")
            .insert(key, expires_at_ms);
        let active_entries = self.entries.lock().expect("inbox entries poisoned").len();
        self.metrics.record_inbox_claim();
        self.metrics.set_inbox_active_entries(active_entries);
        Ok(true)
    }

    fn sweep_expired(&self, now_ms: i64) {
        let mut expired = 0u64;
        {
            let mut entries = self.entries.lock().expect("inbox entries poisoned");
            entries.retain(|_, entry| {
                let keep = entry.expires_at_ms > now_ms;
                if !keep {
                    expired += 1;
                }
                keep
            });
            self.metrics.set_inbox_active_entries(entries.len());
        }
        self.hot
            .lock()
            .expect("inbox hot cache poisoned")
            .retain_active(now_ms);
        self.metrics.record_inbox_expired_entries(expired);
    }
}

fn rehydrate_claims(
    records: Vec<(u64, LogEvent)>,
    now_ms: i64,
    entries: &mut HashMap<InboxKey, InboxEntry>,
    expired: &mut u64,
) -> Result<(), ConnectorError> {
    for (_, record) in records {
        if record.kind != CLAIM_EVENT_KIND {
            continue;
        }
        let claim: InboxClaimRecord =
            serde_json::from_value(record.payload).map_err(ConnectorError::from)?;
        let key = InboxKey::new(claim.binding_id, claim.dedupe_key);
        if claim.expires_at_ms <= now_ms {
            *expired += 1;
            continue;
        }
        entries.insert(
            key,
            InboxEntry {
                expires_at_ms: claim.expires_at_ms,
            },
        );
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct InboxKey {
    binding_id: String,
    dedupe_key: String,
}

impl InboxKey {
    fn new(binding_id: String, dedupe_key: String) -> Self {
        Self {
            binding_id,
            dedupe_key,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct InboxEntry {
    expires_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct InboxClaimRecord {
    binding_id: String,
    dedupe_key: String,
    expires_at_ms: i64,
}

#[derive(Default)]
struct HotCache {
    entries: HashMap<InboxKey, i64>,
    order: VecDeque<InboxKey>,
}

impl HotCache {
    fn contains(&self, key: &InboxKey, now_ms: i64) -> bool {
        self.entries
            .get(key)
            .is_some_and(|expires_at_ms| *expires_at_ms > now_ms)
    }

    fn insert(&mut self, key: InboxKey, expires_at_ms: i64) {
        if !self.entries.contains_key(&key) {
            self.order.push_back(key.clone());
        }
        self.entries.insert(key, expires_at_ms);
        while self.entries.len() > HOT_CACHE_LIMIT {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            self.entries.remove(&oldest);
        }
    }

    fn retain_active(&mut self, now_ms: i64) {
        while let Some(key) = self.order.front().cloned() {
            let remove = self
                .entries
                .get(&key)
                .is_none_or(|expires_at_ms| *expires_at_ms <= now_ms);
            if !remove {
                break;
            }
            self.order.pop_front();
            self.entries.remove(&key);
        }
        self.entries
            .retain(|_, expires_at_ms| *expires_at_ms > now_ms);
        self.order.retain(|key| self.entries.contains_key(key));
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

fn expiry_ms(now_ms: i64, ttl: StdDuration) -> i64 {
    now_ms.saturating_add(ttl.as_millis().min(i64::MAX as u128) as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::event_log::{AnyEventLog, EventLog, FileEventLog, MemoryEventLog};

    #[tokio::test(flavor = "current_thread")]
    async fn restart_rehydrates_durable_claims() {
        let tmp = tempfile::tempdir().unwrap();
        let metrics = Arc::new(MetricsRegistry::default());
        let first_log = Arc::new(AnyEventLog::File(
            FileEventLog::open(tmp.path().to_path_buf(), 32).unwrap(),
        ));
        let first = InboxIndex::new(first_log, metrics.clone()).await.unwrap();
        assert!(first
            .insert_if_new("binding", "delivery-1", StdDuration::from_secs(60))
            .await
            .unwrap());

        let second_log = Arc::new(AnyEventLog::File(
            FileEventLog::open(tmp.path().to_path_buf(), 32).unwrap(),
        ));
        let second = InboxIndex::new(second_log, metrics.clone()).await.unwrap();
        assert!(!second
            .insert_if_new("binding", "delivery-1", StdDuration::from_secs(60))
            .await
            .unwrap());

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.inbox_claims_written, 1);
        assert_eq!(snapshot.inbox_duplicates_rejected, 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ttl_expiry_allows_reclaim() {
        let log = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(32)));
        let metrics = Arc::new(MetricsRegistry::default());
        let index = InboxIndex::new(log.clone(), metrics.clone()).await.unwrap();
        assert!(index
            .insert_if_new("binding", "delivery-1", StdDuration::from_millis(10))
            .await
            .unwrap());
        tokio::time::sleep(StdDuration::from_millis(25)).await;
        assert!(index
            .insert_if_new("binding", "delivery-1", StdDuration::from_millis(10))
            .await
            .unwrap());

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.inbox_claims_written, 2);
        assert!(snapshot.inbox_expired_entries >= 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn hot_cache_rejects_immediate_duplicate_without_extra_event() {
        let log = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(32)));
        let metrics = Arc::new(MetricsRegistry::default());
        let index = InboxIndex::new(log.clone(), metrics.clone()).await.unwrap();
        assert!(index
            .insert_if_new("binding", "delivery-1", StdDuration::from_secs(60))
            .await
            .unwrap());
        assert!(!index
            .insert_if_new("binding", "delivery-1", StdDuration::from_secs(60))
            .await
            .unwrap());

        let topic = Topic::new(TRIGGER_INBOX_CLAIMS_TOPIC).unwrap();
        let events = log.read_range(&topic, None, usize::MAX).await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(metrics.snapshot().inbox_fast_path_hits, 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn restart_rehydrates_legacy_claims_from_mixed_topic() {
        let log = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(32)));
        let metrics = Arc::new(MetricsRegistry::default());
        let legacy_topic = Topic::new(TRIGGER_INBOX_LEGACY_TOPIC).unwrap();
        let now_ms = now_ms();
        log.append(
            &legacy_topic,
            LogEvent::new(
                CLAIM_EVENT_KIND,
                serde_json::json!({
                    "binding_id": "binding",
                    "dedupe_key": "delivery-1",
                    "expires_at_ms": now_ms + 60_000,
                }),
            ),
        )
        .await
        .unwrap();

        let index = InboxIndex::new(log.clone(), metrics.clone()).await.unwrap();
        assert!(!index
            .insert_if_new("binding", "delivery-1", StdDuration::from_secs(60))
            .await
            .unwrap());

        let new_claims_topic = Topic::new(TRIGGER_INBOX_CLAIMS_TOPIC).unwrap();
        let new_claims = log
            .read_range(&new_claims_topic, None, usize::MAX)
            .await
            .unwrap();
        assert!(new_claims.is_empty());
        assert_eq!(metrics.snapshot().inbox_duplicates_rejected, 1);
    }
}
