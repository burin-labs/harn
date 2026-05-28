//! Per-tenant retention policy: lifecycle windows + archive sink hooks.
//!
//! The policy is declarative — a sweep job calls
//! [`crate::sessions::store::SessionStore::sweep_retention`] periodically and
//! the predicates here decide which sessions to archive, soft-delete,
//! or hard-delete. The sweep wires an optional [`ArchiveSink`] from
//! `StoreHooks` into the lifecycle so closed sessions land in
//! durable storage before they leave the store.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::event::{EventId, StoredEvent};
use super::store::{SessionMeta, SessionStatus};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetentionPolicy {
    /// Max wall-clock age for any session. Past this point the session
    /// is hard-deleted (subject to `grace_seconds` after soft-delete).
    #[serde(default)]
    pub max_age_seconds: Option<u64>,
    /// Min age before a closed/idle session is archived. When archiving
    /// is enabled, sessions older than this are emitted to the sink and
    /// soft-deleted.
    #[serde(default)]
    pub min_age_before_archive_seconds: Option<u64>,
    /// Grace window between soft-delete and hard-delete.
    #[serde(default = "RetentionPolicy::default_grace")]
    pub grace_seconds: u64,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            max_age_seconds: None,
            min_age_before_archive_seconds: None,
            grace_seconds: Self::default_grace(),
        }
    }
}

impl RetentionPolicy {
    /// 24-hour grace by default. Short enough to keep tombstones from
    /// piling up indefinitely, long enough to let an operator undo a
    /// soft-delete in normal working hours.
    pub const fn default_grace() -> u64 {
        24 * 60 * 60
    }

    pub fn should_soft_delete(&self, session: &SessionMeta, now_ms: i64) -> bool {
        if !matches!(session.status, SessionStatus::Open | SessionStatus::Closed) {
            return false;
        }
        let age_ms = now_ms.saturating_sub(session.created_at_ms);
        if let Some(max_age_seconds) = self.max_age_seconds {
            if age_ms as u64 >= max_age_seconds.saturating_mul(1_000) {
                return true;
            }
        }
        if let Some(min_age) = self.min_age_before_archive_seconds {
            if matches!(session.status, SessionStatus::Closed)
                && age_ms as u64 >= min_age.saturating_mul(1_000)
            {
                return true;
            }
        }
        false
    }

    pub fn should_hard_delete(&self, session: &SessionMeta, now_ms: i64) -> bool {
        if !matches!(session.status, SessionStatus::SoftDeleted) {
            return false;
        }
        let Some(soft_deleted_at) = session.soft_deleted_at_ms else {
            return false;
        };
        let elapsed_ms = now_ms.saturating_sub(soft_deleted_at);
        elapsed_ms as u64 >= self.grace_seconds.saturating_mul(1_000)
    }

    /// Returns true when a Closed session has crossed
    /// `min_age_before_archive_seconds`. The sweep emits the session
    /// to the [`ArchiveSink`] (if configured) before soft-deleting,
    /// so durable storage receives the events before they leave the
    /// primary store.
    pub fn should_archive(&self, session: &SessionMeta, now_ms: i64) -> bool {
        if !matches!(session.status, SessionStatus::Closed) {
            return false;
        }
        let Some(min_age) = self.min_age_before_archive_seconds else {
            return false;
        };
        let age_ms = now_ms.saturating_sub(session.created_at_ms);
        age_ms as u64 >= min_age.saturating_mul(1_000)
    }
}

/// Snapshot persisted by the sweep when a session is finally
/// hard-deleted. The audit pipeline keeps tombstones so a verifier
/// can prove a session existed and was destroyed at a known time,
/// even after every event row is gone.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tombstone {
    pub session_id: String,
    pub tenant_id: Option<String>,
    pub deleted_at_ms: i64,
    pub deleted_at: String,
    pub final_chain_root_hash: Option<String>,
    pub final_event_id: Option<EventId>,
}

/// Sink that receives sessions on archival. Production wires this to
/// the A.10 audit pipeline (S3/datadog/elastic via the same exporter
/// every other transcript persistence path uses).
#[async_trait::async_trait]
pub trait ArchiveSink: Send + Sync {
    /// Persist a session and its event chain to durable storage.
    /// Called by [`crate::sessions::store::SessionStore::sweep_retention`]
    /// when [`RetentionPolicy::should_archive`] is true, before the
    /// session transitions to `SoftDeleted`.
    async fn archive(
        &self,
        session: &SessionMeta,
        events: &[StoredEvent],
    ) -> Result<(), super::store::StoreError>;

    /// Persist a tombstone for a session about to be hard-deleted.
    /// The default is a no-op for sinks that only care about archived
    /// events; the cloud audit sink overrides to keep a permanent
    /// record of every deletion.
    async fn tombstone(&self, tombstone: &Tombstone) -> Result<(), super::store::StoreError> {
        let _ = tombstone;
        Ok(())
    }
}

pub type SharedArchiveSink = Arc<dyn ArchiveSink>;

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(
        status: SessionStatus,
        created_at_ms: i64,
        soft_deleted_at_ms: Option<i64>,
    ) -> SessionMeta {
        SessionMeta {
            id: "s".into(),
            tenant_id: None,
            persona: None,
            parent_session_id: None,
            created_at_ms,
            created_at: String::new(),
            updated_at_ms: created_at_ms,
            updated_at: String::new(),
            status,
            event_count: 0,
            last_event_id: None,
            chain_root_hash: None,
            closed_at_ms: None,
            closed_at: None,
            soft_deleted_at_ms,
            ttl_seconds: None,
            tags: Vec::new(),
            attributes: Default::default(),
        }
    }

    #[test]
    fn soft_delete_triggers_when_open_session_exceeds_max_age() {
        let policy = RetentionPolicy {
            max_age_seconds: Some(60),
            ..RetentionPolicy::default()
        };
        let session = meta(SessionStatus::Open, 0, None);
        assert!(policy.should_soft_delete(&session, 61_000));
        assert!(!policy.should_soft_delete(&session, 59_000));
    }

    #[test]
    fn hard_delete_waits_for_grace_window() {
        let policy = RetentionPolicy {
            grace_seconds: 10,
            ..RetentionPolicy::default()
        };
        let session = meta(SessionStatus::SoftDeleted, 0, Some(100));
        assert!(!policy.should_hard_delete(&session, 100 + 9_000));
        assert!(policy.should_hard_delete(&session, 100 + 10_000));
    }

    #[test]
    fn closed_session_archives_after_min_age() {
        let policy = RetentionPolicy {
            min_age_before_archive_seconds: Some(5),
            ..RetentionPolicy::default()
        };
        let session = meta(SessionStatus::Closed, 0, None);
        assert!(!policy.should_soft_delete(&session, 4_000));
        assert!(policy.should_soft_delete(&session, 5_000));
        assert!(!policy.should_archive(&session, 4_000));
        assert!(policy.should_archive(&session, 5_000));
    }

    #[test]
    fn should_archive_only_applies_to_closed_sessions() {
        let policy = RetentionPolicy {
            min_age_before_archive_seconds: Some(1),
            ..RetentionPolicy::default()
        };
        let open = meta(SessionStatus::Open, 0, None);
        let closed = meta(SessionStatus::Closed, 0, None);
        let soft = meta(SessionStatus::SoftDeleted, 0, Some(0));
        assert!(!policy.should_archive(&open, 60_000));
        assert!(policy.should_archive(&closed, 60_000));
        assert!(!policy.should_archive(&soft, 60_000));
    }
}
