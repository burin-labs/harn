//! Append-only version log of file mutations.
//!
//! Every successful write through the host's `version_record` op lands here
//! with a monotonic sequence number. Agents call `changes_since(seq)`
//! to catch up between turns without re-reading every file, and the seq
//! numbers let higher-level tooling spot "agent fighting itself" loops
//! before they cause damage.
//!
//! Per-file history is capped at [`HISTORY_LIMIT`] entries so an agent
//! that thrashes one path doesn't blow up the log indefinitely. The seq
//! counter itself is global.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Maximum number of entries kept per path. Older entries roll off the
/// front in FIFO order.
pub const HISTORY_LIMIT: usize = 200;

/// Edit-classification for one record. The string forms ride out to Harn
/// scripts and the cross-repo schema so callers can switch on them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditOp {
    /// Mark a snapshot/checkpoint, not a content change.
    Snapshot,
    /// Whole-file write.
    Write,
    /// Targeted line-range replacement.
    ReplaceRange,
    /// Insertion after a specified line.
    InsertAfter,
    /// Targeted line-range deletion.
    DeleteRange,
    /// Patch/diff application.
    Patch,
    /// File deletion.
    Delete,
}

impl EditOp {
    /// String form used by the Harn host bridge.
    pub fn as_str(self) -> &'static str {
        match self {
            EditOp::Snapshot => "snapshot",
            EditOp::Write => "write",
            EditOp::ReplaceRange => "replace_range",
            EditOp::InsertAfter => "insert_after",
            EditOp::DeleteRange => "delete_range",
            EditOp::Patch => "patch",
            EditOp::Delete => "delete",
        }
    }

    /// Parse from the string form; returns `None` for unknown variants so
    /// the caller can decide whether to default to `Write` or to error.
    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "snapshot" => EditOp::Snapshot,
            "write" => EditOp::Write,
            "replace_range" => EditOp::ReplaceRange,
            "insert_after" => EditOp::InsertAfter,
            "delete_range" => EditOp::DeleteRange,
            "patch" => EditOp::Patch,
            "delete" => EditOp::Delete,
            _ => return None,
        })
    }
}

/// One entry in the per-file history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionEntry {
    /// Globally monotonic sequence number.
    pub seq: u64,
    /// Agent that recorded the edit.
    pub agent_id: u64,
    /// Wall-clock ms since the Unix epoch.
    pub timestamp_ms: i64,
    /// Edit classification.
    pub op: EditOp,
    /// Post-edit content hash (or `0` if not applicable, e.g. `Delete`).
    pub hash: u64,
    /// Post-edit byte size.
    pub size: u64,
}

/// Public denormalised form returned by `changes_since`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeRecord {
    /// Workspace-relative path the edit was attributed to.
    pub path: String,
    /// Globally monotonic sequence number.
    pub seq: u64,
    /// Agent id that recorded the edit.
    pub agent_id: u64,
    /// Edit classification.
    pub op: EditOp,
    /// Post-edit hash.
    pub hash: u64,
    /// Post-edit size.
    pub size: u64,
    /// Wall-clock ms since the Unix epoch.
    pub timestamp_ms: i64,
}

/// Append-only log keyed by path. Both forward query patterns —
/// "everything since X" and "the latest entry for this path" — are
/// served from the same map.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct VersionLog {
    /// Latest assigned sequence number. Monotonically increases.
    #[serde(default)]
    pub current_seq: u64,
    /// Per-path history, newest-last; capped at [`HISTORY_LIMIT`] entries.
    #[serde(default)]
    pub history: HashMap<String, Vec<VersionEntry>>,
}

impl VersionLog {
    /// Construct an empty log.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one edit. Returns the assigned seq, which is what the host
    /// builtin echoes back to the caller.
    pub fn record(
        &mut self,
        path: impl Into<String>,
        agent_id: u64,
        op: EditOp,
        hash: u64,
        size: u64,
        timestamp_ms: i64,
    ) -> u64 {
        self.current_seq = self.current_seq.saturating_add(1);
        let entry = VersionEntry {
            seq: self.current_seq,
            agent_id,
            timestamp_ms,
            op,
            hash,
            size,
        };
        let path = path.into();
        let list = self.history.entry(path).or_default();
        list.push(entry);
        if list.len() > HISTORY_LIMIT {
            let drop = list.len() - HISTORY_LIMIT;
            list.drain(0..drop);
        }
        self.current_seq
    }

    /// Every change record with `seq > since`, ordered by seq ascending.
    /// `limit` (when present) keeps the *most recent* `limit` entries.
    pub fn changes_since(&self, since: u64, limit: Option<usize>) -> Vec<ChangeRecord> {
        let mut out: Vec<ChangeRecord> = Vec::new();
        for (path, entries) in &self.history {
            for entry in entries {
                if entry.seq > since {
                    out.push(ChangeRecord {
                        path: path.clone(),
                        seq: entry.seq,
                        agent_id: entry.agent_id,
                        op: entry.op,
                        hash: entry.hash,
                        size: entry.size,
                        timestamp_ms: entry.timestamp_ms,
                    });
                }
            }
        }
        out.sort_by_key(|r| r.seq);
        if let Some(limit) = limit {
            if out.len() > limit {
                let start = out.len() - limit;
                out = out.split_off(start);
            }
        }
        out
    }

    /// Latest entry recorded for `path`, or `None` if untracked.
    pub fn last_entry(&self, path: &str) -> Option<&VersionEntry> {
        self.history.get(path).and_then(|v| v.last())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_assigns_monotonic_seqs() {
        let mut log = VersionLog::new();
        let s1 = log.record("a.rs", 1, EditOp::Write, 10, 5, 100);
        let s2 = log.record("b.rs", 1, EditOp::Write, 11, 5, 110);
        let s3 = log.record("a.rs", 2, EditOp::Write, 12, 5, 120);
        assert_eq!((s1, s2, s3), (1, 2, 3));
        assert_eq!(log.current_seq, 3);
    }

    #[test]
    fn changes_since_returns_sorted_records() {
        let mut log = VersionLog::new();
        log.record("a.rs", 1, EditOp::Write, 1, 1, 100);
        log.record("b.rs", 2, EditOp::Write, 2, 2, 110);
        log.record("a.rs", 3, EditOp::Patch, 3, 3, 120);
        let changes = log.changes_since(1, None);
        let seqs: Vec<u64> = changes.iter().map(|c| c.seq).collect();
        assert_eq!(seqs, vec![2, 3]);
    }

    #[test]
    fn changes_since_respects_limit_and_keeps_most_recent() {
        let mut log = VersionLog::new();
        for i in 1..=5 {
            log.record("a.rs", 1, EditOp::Write, i, i, i as i64);
        }
        let limited = log.changes_since(0, Some(2));
        let seqs: Vec<u64> = limited.iter().map(|c| c.seq).collect();
        assert_eq!(seqs, vec![4, 5]);
    }

    #[test]
    fn history_caps_at_history_limit() {
        let mut log = VersionLog::new();
        for i in 0..(HISTORY_LIMIT + 50) {
            log.record("a.rs", 1, EditOp::Write, i as u64, 0, 0);
        }
        let entries = log.history.get("a.rs").unwrap();
        assert_eq!(entries.len(), HISTORY_LIMIT);
        // Front of the list rolled off — the very first record is gone.
        assert!(entries.first().unwrap().seq > 1);
    }

    #[test]
    fn edit_op_round_trips_through_str_form() {
        for op in [
            EditOp::Snapshot,
            EditOp::Write,
            EditOp::ReplaceRange,
            EditOp::InsertAfter,
            EditOp::DeleteRange,
            EditOp::Patch,
            EditOp::Delete,
        ] {
            assert_eq!(EditOp::parse(op.as_str()), Some(op));
        }
        assert_eq!(EditOp::parse("unknown"), None);
    }
}
