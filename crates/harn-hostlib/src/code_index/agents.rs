//! Per-workspace agent registry plus advisory per-file locks.
//!
//! Mirrors the Swift `AgentRegistry` actor in `Sources/BurinCodeIndex/`.
//! Tracks live agents (IDE, background eval, agentic loops, etc.) and the
//! TTL-based advisory locks they hold over files. Agents call `heartbeat`
//! on their own cadence; the registry reaps anyone who has gone silent
//! beyond `agent_timeout_ms` and releases their locks.
//!
//! All bookkeeping lives behind a `Mutex` inside [`IndexState`] so the
//! capability stays single-threaded from the Harn VM's perspective. The
//! registry itself is `Send + Sync`-friendly: callers wrap it in
//! `Arc<Mutex<_>>`.
//!
//! ## Recovery
//!
//! [`AgentRegistry::reap`] is the single recovery primitive: walking
//! every recorded agent and downgrading anyone whose `last_seen` is older
//! than the timeout. Lock holders that have been reaped lose their locks
//! at the same time. Embedders call this at startup (after restoring
//! state from a snapshot, if any) to clear out agents that crashed
//! between runs.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Stable identifier for an agent in the registry.
pub type AgentId = u64;

/// Lifecycle state of one tracked agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentState {
    /// Recently heartbeated and considered live.
    Active,
    /// Missed too many heartbeats. Locks have been released; the record
    /// is kept around so historical version-log entries can resolve the
    /// human-readable name.
    Crashed,
    /// Explicitly unregistered. Equivalent to "deleted" but exposed so
    /// listings stay debuggable.
    Gone,
}

/// One row in the registry. Public so embedders that want to surface a
/// `status` panel can read the lifecycle state without going through the
/// host builtins.
#[derive(Debug, Clone)]
pub struct AgentInfo {
    /// Stable identifier.
    pub id: AgentId,
    /// Human-readable label (`"editor"`, `"copilot"`, etc.).
    pub name: String,
    /// Lifecycle state.
    pub state: AgentState,
    /// Wall-clock ms since the Unix epoch of the last heartbeat (or
    /// register/lock activity).
    pub last_seen_ms: i64,
    /// Cumulative number of edits attributed to this agent.
    pub edit_count: u64,
    /// Workspace-relative path → expiry timestamp (ms). Empty when the
    /// agent holds no locks.
    pub locked_paths: HashMap<String, i64>,
}

/// Registry config — defaults match the Swift actor on the burin-code
/// side so the cross-repo schema-drift tests stay aligned.
#[derive(Debug, Clone, Copy)]
pub struct RegistryConfig {
    /// Default lock TTL when callers don't supply one. 30 seconds in the
    /// Swift port.
    pub default_lock_ttl_ms: i64,
    /// How long a registered agent can stay silent before the next reap
    /// downgrades it to `Crashed` and releases its locks. 45 seconds.
    pub agent_timeout_ms: i64,
}

impl Default for RegistryConfig {
    fn default() -> Self {
        Self {
            default_lock_ttl_ms: 30_000,
            agent_timeout_ms: 45_000,
        }
    }
}

/// Per-workspace agent registry plus advisory per-file lock table.
#[derive(Debug, Default, Clone)]
pub struct AgentRegistry {
    config: RegistryConfig,
    next_id: AgentId,
    agents: HashMap<AgentId, AgentInfo>,
}

impl AgentRegistry {
    /// Construct an empty registry with default TTL/timeout values.
    pub fn new() -> Self {
        Self::with_config(RegistryConfig::default())
    }

    /// Construct an empty registry with explicit thresholds.
    pub fn with_config(config: RegistryConfig) -> Self {
        Self {
            config,
            next_id: 1,
            agents: HashMap::new(),
        }
    }

    /// Borrow the active config (lock TTL + timeout).
    pub fn config(&self) -> RegistryConfig {
        self.config
    }

    /// Register a new agent under an auto-assigned id. The display name is
    /// stored verbatim so embedders can surface it in `status`.
    pub fn register(&mut self, name: impl Into<String>, now_ms: i64) -> AgentId {
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .expect("AgentId overflow — registry has been alive an absurd amount of time");
        self.register_with_id(id, name, now_ms);
        id
    }

    /// Register or refresh an agent under an explicit id. Used by callers
    /// (and the version log) that need to thread an externally-assigned id
    /// through the registry. Returns the same id back.
    pub fn register_with_id(
        &mut self,
        id: AgentId,
        name: impl Into<String>,
        now_ms: i64,
    ) -> AgentId {
        self.next_id = self.next_id.max(id.saturating_add(1));
        self.agents.insert(
            id,
            AgentInfo {
                id,
                name: name.into(),
                state: AgentState::Active,
                last_seen_ms: now_ms,
                edit_count: 0,
                locked_paths: HashMap::new(),
            },
        );
        id
    }

    /// Refresh an agent's `last_seen_ms`. If the agent isn't registered we
    /// transparently register it with a placeholder name (`agent-<id>`),
    /// matching the Swift actor's "self-heal" behaviour.
    pub fn heartbeat(&mut self, id: AgentId, now_ms: i64) {
        match self.agents.get_mut(&id) {
            Some(info) => {
                info.last_seen_ms = now_ms;
                if info.state == AgentState::Crashed {
                    info.state = AgentState::Active;
                }
            }
            None => {
                self.register_with_id(id, format!("agent-{id}"), now_ms);
            }
        }
    }

    /// Drop an agent record. No-op if the id isn't registered.
    pub fn unregister(&mut self, id: AgentId) {
        self.agents.remove(&id);
    }

    /// Iterate over the live agent records — useful for `status` payloads.
    pub fn agents(&self) -> impl Iterator<Item = &AgentInfo> {
        self.agents.values()
    }

    /// Look up one agent record by id.
    pub fn get(&self, id: AgentId) -> Option<&AgentInfo> {
        self.agents.get(&id)
    }

    /// Bump the `edit_count` for `id`. Used by `version_record` so the
    /// status surface can quickly answer "is this agent still busy?".
    pub fn note_edit(&mut self, id: AgentId, now_ms: i64) {
        if let Some(info) = self.agents.get_mut(&id) {
            info.edit_count = info.edit_count.saturating_add(1);
            info.last_seen_ms = now_ms;
        }
    }

    /// Try to acquire an exclusive lock on `path` for `agent_id`. Reaps
    /// expired records first so a stale holder doesn't block forever.
    /// Returns `true` if the lock was granted.
    pub fn try_lock(
        &mut self,
        agent_id: AgentId,
        path: &str,
        ttl_ms: Option<i64>,
        now_ms: i64,
    ) -> bool {
        self.reap(now_ms);
        let ttl = ttl_ms.unwrap_or(self.config.default_lock_ttl_ms);
        for (other_id, other) in &self.agents {
            if *other_id == agent_id {
                continue;
            }
            if let Some(expiry) = other.locked_paths.get(path) {
                if *expiry > now_ms {
                    return false;
                }
            }
        }
        if !self.agents.contains_key(&agent_id) {
            self.register_with_id(agent_id, format!("agent-{agent_id}"), now_ms);
        }
        let info = self
            .agents
            .get_mut(&agent_id)
            .expect("just registered above");
        info.locked_paths.insert(path.to_string(), now_ms + ttl);
        info.last_seen_ms = now_ms;
        true
    }

    /// Release a lock previously held by `agent_id` on `path`. No-op when
    /// the agent or the lock are gone.
    pub fn release_lock(&mut self, agent_id: AgentId, path: &str) {
        if let Some(info) = self.agents.get_mut(&agent_id) {
            info.locked_paths.remove(path);
        }
    }

    /// Return the id of the agent currently holding `path`, or `None`.
    /// Expired holders are reaped lazily so the answer reflects state
    /// without requiring a separate pass.
    pub fn lock_holder(&mut self, path: &str, now_ms: i64) -> Option<AgentId> {
        self.reap(now_ms);
        for (id, info) in &self.agents {
            if let Some(expiry) = info.locked_paths.get(path) {
                if *expiry > now_ms {
                    return Some(*id);
                }
            }
        }
        None
    }

    /// Walk every agent and downgrade ones whose `last_seen_ms` is older
    /// than `agent_timeout_ms`. Crashed agents drop their locks as a
    /// side effect. Idempotent — embedders call this at startup to recover
    /// state inherited from a previous run.
    pub fn reap(&mut self, now_ms: i64) {
        let timeout = self.config.agent_timeout_ms;
        for info in self.agents.values_mut() {
            if info.state == AgentState::Active && now_ms - info.last_seen_ms > timeout {
                info.state = AgentState::Crashed;
                info.locked_paths.clear();
            }
        }
    }

    /// Persist the registry into a serialisable form.
    pub fn snapshot(&self) -> SerializedRegistry {
        SerializedRegistry {
            next_id: self.next_id,
            agents: self.agents.values().map(SerializedAgent::from).collect(),
        }
    }

    /// Restore a registry from a previously persisted snapshot.
    pub fn from_snapshot(config: RegistryConfig, snap: SerializedRegistry) -> Self {
        let mut agents = HashMap::with_capacity(snap.agents.len());
        for entry in snap.agents {
            agents.insert(
                entry.id,
                AgentInfo {
                    id: entry.id,
                    name: entry.name,
                    state: entry.state,
                    last_seen_ms: entry.last_seen_ms,
                    edit_count: entry.edit_count,
                    locked_paths: entry.locked_paths,
                },
            );
        }
        Self {
            config,
            next_id: snap.next_id.max(1),
            agents,
        }
    }
}

/// On-disk layout for an agent record. Public so the snapshot module can
/// embed it. Field shapes intentionally mirror the Swift `AgentInfo` so
/// existing snapshots remain readable across the bridge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerializedAgent {
    /// Stable identifier.
    pub id: AgentId,
    /// Human-readable label.
    pub name: String,
    /// Lifecycle state at snapshot time.
    pub state: AgentState,
    /// Wall-clock ms of the last heartbeat.
    pub last_seen_ms: i64,
    /// Number of edits attributed at snapshot time.
    pub edit_count: u64,
    /// Locks held at snapshot time (path → expiry ms).
    pub locked_paths: HashMap<String, i64>,
}

impl From<&AgentInfo> for SerializedAgent {
    fn from(info: &AgentInfo) -> Self {
        Self {
            id: info.id,
            name: info.name.clone(),
            state: info.state,
            last_seen_ms: info.last_seen_ms,
            edit_count: info.edit_count,
            locked_paths: info.locked_paths.clone(),
        }
    }
}

/// On-disk layout for the full registry.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SerializedRegistry {
    /// Next id to hand out — preserved across restarts so reused ids don't
    /// collide with historical records in the version log.
    #[serde(default)]
    pub next_id: AgentId,
    /// All known agent records.
    #[serde(default)]
    pub agents: Vec<SerializedAgent>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_then_heartbeat_keeps_agent_active() {
        let mut reg = AgentRegistry::new();
        let id = reg.register("editor", 1_000);
        assert!(matches!(reg.get(id).unwrap().state, AgentState::Active));
        reg.heartbeat(id, 5_000);
        let info = reg.get(id).unwrap();
        assert_eq!(info.last_seen_ms, 5_000);
        assert_eq!(info.state, AgentState::Active);
    }

    #[test]
    fn reap_marks_silent_agents_crashed_and_drops_locks() {
        let mut reg = AgentRegistry::new();
        let id = reg.register("editor", 0);
        assert!(reg.try_lock(id, "src/main.rs", None, 0));
        // 60s later the agent has missed the 45s timeout.
        reg.reap(60_000);
        let info = reg.get(id).unwrap();
        assert_eq!(info.state, AgentState::Crashed);
        assert!(info.locked_paths.is_empty());
        assert_eq!(reg.lock_holder("src/main.rs", 60_000), None);
    }

    #[test]
    fn try_lock_blocks_other_agents_until_expiry() {
        let mut reg = AgentRegistry::new();
        let a = reg.register("a", 0);
        let b = reg.register("b", 0);
        assert!(reg.try_lock(a, "f.rs", Some(1_000), 0));
        // While A holds it, B can't grab it.
        assert!(!reg.try_lock(b, "f.rs", Some(1_000), 100));
        // Past expiry, B can take over.
        assert!(reg.try_lock(b, "f.rs", Some(1_000), 5_000));
        assert_eq!(reg.lock_holder("f.rs", 5_000), Some(b));
    }

    #[test]
    fn release_lock_lets_others_acquire_immediately() {
        let mut reg = AgentRegistry::new();
        let a = reg.register("a", 0);
        let b = reg.register("b", 0);
        reg.try_lock(a, "x", None, 0);
        reg.release_lock(a, "x");
        assert!(reg.try_lock(b, "x", None, 100));
    }

    #[test]
    fn heartbeat_resurrects_a_crashed_agent() {
        let mut reg = AgentRegistry::new();
        let id = reg.register("a", 0);
        reg.reap(60_000);
        assert_eq!(reg.get(id).unwrap().state, AgentState::Crashed);
        reg.heartbeat(id, 70_000);
        assert_eq!(reg.get(id).unwrap().state, AgentState::Active);
    }

    #[test]
    fn snapshot_round_trips_through_serialized_form() {
        let mut reg = AgentRegistry::new();
        let id = reg.register("editor", 100);
        reg.try_lock(id, "src/main.rs", Some(1_000), 100);
        let snap = reg.snapshot();
        let restored = AgentRegistry::from_snapshot(reg.config(), snap);
        assert_eq!(restored.get(id).unwrap().name, "editor");
        assert_eq!(
            restored
                .get(id)
                .unwrap()
                .locked_paths
                .get("src/main.rs")
                .copied(),
            Some(1_100)
        );
    }
}
