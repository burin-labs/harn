use crate::value::VmDictExt;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;

use crate::value::{values_equal, VmChannelCloseState, VmChannelHandle, VmValue};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ScopedKey {
    pub scope: String,
    pub key: String,
}

struct SharedCell {
    value: VmValue,
    version: u64,
    metrics: SharedMetrics,
}

#[derive(Default)]
struct SharedMap {
    entries: crate::value::DictMap,
    version: u64,
    metrics: SharedMetrics,
}

#[derive(Default)]
struct SharedMetrics {
    read_count: u64,
    write_count: u64,
    cas_success_count: u64,
    cas_failure_count: u64,
    stale_read_count: u64,
}

#[derive(Clone)]
struct Mailbox {
    channel: VmChannelHandle,
    capacity: usize,
    sent_count: Arc<AtomicU64>,
    received_count: Arc<AtomicU64>,
    failed_send_count: Arc<AtomicU64>,
    depth: Arc<AtomicI64>,
}

struct CircuitState {
    failures: usize,
    threshold: usize,
    reset_after: std::time::Duration,
    opened_at: Option<std::time::Instant>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CircuitStatus {
    Closed,
    Open,
    HalfOpen,
}

impl CircuitStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Closed => "closed",
            Self::Open => "open",
            Self::HalfOpen => "half_open",
        }
    }
}

#[derive(Default)]
pub struct VmSharedStateRuntime {
    cells: parking_lot::Mutex<BTreeMap<ScopedKey, SharedCell>>,
    maps: parking_lot::Mutex<BTreeMap<ScopedKey, SharedMap>>,
    mailboxes: parking_lot::Mutex<BTreeMap<ScopedKey, Mailbox>>,
    circuits: parking_lot::Mutex<BTreeMap<String, CircuitState>>,
}

impl VmSharedStateRuntime {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn configure_circuit(
        &self,
        name: String,
        threshold: usize,
        reset_after: std::time::Duration,
    ) {
        self.circuits.lock().insert(
            name,
            CircuitState {
                failures: 0,
                threshold,
                reset_after,
                opened_at: None,
            },
        );
    }

    pub fn circuit_status(&self, name: &str, now: std::time::Instant) -> CircuitStatus {
        let circuits = self.circuits.lock();
        let Some(state) = circuits.get(name) else {
            return CircuitStatus::Closed;
        };
        match state.opened_at {
            None => CircuitStatus::Closed,
            Some(opened_at) if now.saturating_duration_since(opened_at) >= state.reset_after => {
                CircuitStatus::HalfOpen
            }
            Some(_) => CircuitStatus::Open,
        }
    }

    pub fn record_circuit_success(&self, name: &str) {
        if let Some(state) = self.circuits.lock().get_mut(name) {
            state.failures = 0;
            state.opened_at = None;
        }
    }

    pub fn record_circuit_failure(&self, name: &str, now: std::time::Instant) -> bool {
        let mut circuits = self.circuits.lock();
        let Some(state) = circuits.get_mut(name) else {
            return false;
        };
        state.failures = state.failures.saturating_add(1);
        if state.failures >= state.threshold && state.opened_at.is_none() {
            state.opened_at = Some(now);
            return true;
        }
        false
    }

    pub fn reset_circuit(&self, name: &str) {
        self.record_circuit_success(name);
    }

    pub fn open_cell(&self, scoped: ScopedKey, initial: VmValue) -> VmValue {
        self.cells
            .lock()
            .entry(scoped.clone())
            .or_insert_with(|| SharedCell {
                value: initial,
                version: 0,
                metrics: SharedMetrics::default(),
            });
        handle_value("shared_cell", &scoped)
    }

    pub fn cell_get(&self, scoped: &ScopedKey) -> VmValue {
        let mut cells = self.cells.lock();
        let cell = cells.entry(scoped.clone()).or_default();
        cell.metrics.read_count += 1;
        cell.value.clone()
    }

    pub fn cell_snapshot(&self, scoped: &ScopedKey) -> VmValue {
        let mut cells = self.cells.lock();
        let cell = cells.entry(scoped.clone()).or_default();
        cell.metrics.read_count += 1;
        snapshot_value(cell.value.clone(), cell.version)
    }

    pub fn cell_set(&self, scoped: &ScopedKey, value: VmValue) -> VmValue {
        let mut cells = self.cells.lock();
        let cell = cells.entry(scoped.clone()).or_default();
        let old = std::mem::replace(&mut cell.value, value);
        cell.version = cell.version.saturating_add(1);
        cell.metrics.write_count += 1;
        old
    }

    pub fn cell_cas(&self, scoped: &ScopedKey, expected: &VmValue, new_value: VmValue) -> bool {
        let mut cells = self.cells.lock();
        let cell = cells.entry(scoped.clone()).or_default();
        let (expected_value, expected_version) = snapshot_expected(expected);
        let version_matches = expected_version.is_none_or(|version| version == cell.version);
        let value_matches = values_equal(&cell.value, &expected_value);
        if version_matches && value_matches {
            cell.value = new_value;
            cell.version = cell.version.saturating_add(1);
            cell.metrics.write_count += 1;
            cell.metrics.cas_success_count += 1;
            true
        } else {
            cell.metrics.cas_failure_count += 1;
            if expected_version.is_some_and(|version| version != cell.version) {
                cell.metrics.stale_read_count += 1;
            }
            false
        }
    }

    pub fn open_map(&self, scoped: ScopedKey, initial: Option<crate::value::DictMap>) -> VmValue {
        self.maps
            .lock()
            .entry(scoped.clone())
            .or_insert_with(|| SharedMap {
                entries: initial.unwrap_or_default(),
                version: 0,
                metrics: SharedMetrics::default(),
            });
        handle_value("shared_map", &scoped)
    }

    pub fn map_get(&self, scoped: &ScopedKey, key: &str, default: VmValue) -> VmValue {
        let mut maps = self.maps.lock();
        let map = maps.entry(scoped.clone()).or_default();
        map.metrics.read_count += 1;
        map.entries.get(key).cloned().unwrap_or(default)
    }

    pub fn map_snapshot(&self, scoped: &ScopedKey, key: &str) -> VmValue {
        let mut maps = self.maps.lock();
        let map = maps.entry(scoped.clone()).or_default();
        map.metrics.read_count += 1;
        snapshot_value(
            map.entries.get(key).cloned().unwrap_or(VmValue::Nil),
            map.version,
        )
    }

    pub fn map_entries(&self, scoped: &ScopedKey) -> VmValue {
        let mut maps = self.maps.lock();
        let map = maps.entry(scoped.clone()).or_default();
        map.metrics.read_count += 1;
        VmValue::dict(map.entries.clone())
    }

    pub fn map_set(&self, scoped: &ScopedKey, key: String, value: VmValue) -> VmValue {
        let mut maps = self.maps.lock();
        let map = maps.entry(scoped.clone()).or_default();
        let old = map
            .entries
            .insert(crate::value::intern_key(&key), value)
            .unwrap_or(VmValue::Nil);
        map.version = map.version.saturating_add(1);
        map.metrics.write_count += 1;
        old
    }

    pub fn map_delete(&self, scoped: &ScopedKey, key: &str) -> VmValue {
        let mut maps = self.maps.lock();
        let map = maps.entry(scoped.clone()).or_default();
        let old = map.entries.remove(key).unwrap_or(VmValue::Nil);
        if !matches!(old, VmValue::Nil) {
            map.version = map.version.saturating_add(1);
            map.metrics.write_count += 1;
        }
        old
    }

    pub fn map_cas(
        &self,
        scoped: &ScopedKey,
        key: String,
        expected: &VmValue,
        new_value: VmValue,
    ) -> bool {
        let mut maps = self.maps.lock();
        let map = maps.entry(scoped.clone()).or_default();
        let current = map
            .entries
            .get(key.as_str())
            .cloned()
            .unwrap_or(VmValue::Nil);
        let (expected_value, expected_version) = snapshot_expected(expected);
        let version_matches = expected_version.is_none_or(|version| version == map.version);
        let value_matches = values_equal(&current, &expected_value);
        if version_matches && value_matches {
            if matches!(new_value, VmValue::Nil) {
                map.entries.remove(key.as_str());
            } else {
                map.entries
                    .insert(crate::value::intern_key(&key), new_value);
            }
            map.version = map.version.saturating_add(1);
            map.metrics.write_count += 1;
            map.metrics.cas_success_count += 1;
            true
        } else {
            map.metrics.cas_failure_count += 1;
            if expected_version.is_some_and(|version| version != map.version) {
                map.metrics.stale_read_count += 1;
            }
            false
        }
    }

    pub fn open_mailbox(&self, scoped: ScopedKey, capacity: usize) -> VmValue {
        self.mailboxes
            .lock()
            .entry(scoped.clone())
            .or_insert_with(|| {
                let capacity = capacity.max(1);
                let (sender, receiver) = tokio::sync::mpsc::channel(capacity);
                // Mailboxes follow the same local-task ownership invariant as
                // channels: sends and receives are coordinated by the VM that
                // opened the handle.
                let channel = VmChannelHandle {
                    name: std::sync::Arc::from(scoped.key.clone()),
                    sender: Arc::new(sender),
                    receiver: Arc::new(tokio::sync::Mutex::new(receiver)),
                    close: Arc::new(VmChannelCloseState::open()),
                };
                Mailbox {
                    channel,
                    capacity,
                    sent_count: Arc::new(AtomicU64::new(0)),
                    received_count: Arc::new(AtomicU64::new(0)),
                    failed_send_count: Arc::new(AtomicU64::new(0)),
                    depth: Arc::new(AtomicI64::new(0)),
                }
            });
        handle_value("mailbox", &scoped)
    }

    pub fn mailbox(&self, scoped: &ScopedKey) -> Option<VmValue> {
        if self.mailboxes.lock().contains_key(scoped) {
            Some(handle_value("mailbox", scoped))
        } else {
            None
        }
    }

    pub fn mailbox_channel(&self, scoped: &ScopedKey) -> Option<VmChannelHandle> {
        self.mailboxes
            .lock()
            .get(scoped)
            .map(|mailbox| mailbox.channel.clone())
    }

    pub fn note_mailbox_send(&self, scoped: &ScopedKey, ok: bool) {
        if let Some(mailbox) = self.mailboxes.lock().get(scoped) {
            if ok {
                mailbox.sent_count.fetch_add(1, Ordering::SeqCst);
                mailbox.depth.fetch_add(1, Ordering::SeqCst);
            } else {
                mailbox.failed_send_count.fetch_add(1, Ordering::SeqCst);
            }
        }
    }

    pub fn note_mailbox_receive(&self, scoped: &ScopedKey) {
        if let Some(mailbox) = self.mailboxes.lock().get(scoped) {
            mailbox.received_count.fetch_add(1, Ordering::SeqCst);
            let _ = mailbox
                .depth
                .try_update(Ordering::SeqCst, Ordering::SeqCst, |depth| {
                    Some(depth.saturating_sub(1))
                });
        }
    }

    pub fn close_mailbox(&self, scoped: &ScopedKey) -> bool {
        let Some(mailbox) = self.mailboxes.lock().get(scoped).cloned() else {
            return false;
        };
        if !mailbox.channel.close() {
            return false;
        }
        if let Ok(mut rx) = mailbox.channel.receiver.try_lock() {
            rx.close();
        }
        true
    }

    pub fn metrics(&self, kind: Option<&str>, scoped: Option<&ScopedKey>) -> VmValue {
        match (kind, scoped) {
            (Some("shared_cell"), Some(scoped)) => self
                .cells
                .lock()
                .get(scoped)
                .map(|cell| shared_metrics_value(&cell.metrics, cell.version))
                .unwrap_or_else(empty_shared_metrics),
            (Some("shared_map"), Some(scoped)) => self
                .maps
                .lock()
                .get(scoped)
                .map(|map| shared_metrics_value(&map.metrics, map.version))
                .unwrap_or_else(empty_shared_metrics),
            (Some("mailbox"), Some(scoped)) => self
                .mailboxes
                .lock()
                .get(scoped)
                .map(mailbox_metrics_value)
                .unwrap_or_else(empty_mailbox_metrics),
            _ => {
                let mut values = Vec::new();
                for (scoped, cell) in self.cells.lock().iter() {
                    values.push(with_scope_fields(
                        "shared_cell",
                        scoped,
                        shared_metrics_value(&cell.metrics, cell.version),
                    ));
                }
                for (scoped, map) in self.maps.lock().iter() {
                    values.push(with_scope_fields(
                        "shared_map",
                        scoped,
                        shared_metrics_value(&map.metrics, map.version),
                    ));
                }
                for (scoped, mailbox) in self.mailboxes.lock().iter() {
                    values.push(with_scope_fields(
                        "mailbox",
                        scoped,
                        mailbox_metrics_value(mailbox),
                    ));
                }
                VmValue::list(values)
            }
        }
    }
}

fn handle_value(kind: &str, scoped: &ScopedKey) -> VmValue {
    let mut value = BTreeMap::new();
    value.put_str("_type", kind);
    value.put_str("scope", scoped.scope.clone());
    value.put_str("key", scoped.key.clone());
    VmValue::dict(value)
}

fn snapshot_value(value: VmValue, version: u64) -> VmValue {
    let mut snapshot = BTreeMap::new();
    snapshot.put_str("_type", "shared_snapshot");
    snapshot.insert("value".to_string(), value);
    snapshot.put_int("version", version as i64);
    VmValue::dict(snapshot)
}

fn snapshot_expected(value: &VmValue) -> (VmValue, Option<u64>) {
    let Some(dict) = value.as_dict() else {
        return (value.clone(), None);
    };
    let is_snapshot = matches!(
        dict.get("_type"),
        Some(VmValue::String(kind)) if kind.as_str() == "shared_snapshot"
    );
    if !is_snapshot {
        return (value.clone(), None);
    }
    let expected_value = dict.get("value").cloned().unwrap_or(VmValue::Nil);
    let expected_version = dict
        .get("version")
        .and_then(VmValue::as_int)
        .filter(|version| *version >= 0)
        .map(|version| version as u64);
    (expected_value, expected_version)
}

fn shared_metrics_value(metrics: &SharedMetrics, version: u64) -> VmValue {
    let mut value = BTreeMap::new();
    value.put_int("version", version as i64);
    value.put_int("read_count", metrics.read_count as i64);
    value.put_int("write_count", metrics.write_count as i64);
    value.put_int("cas_success_count", metrics.cas_success_count as i64);
    value.put_int("cas_failure_count", metrics.cas_failure_count as i64);
    value.put_int("stale_read_count", metrics.stale_read_count as i64);
    VmValue::dict(value)
}

fn empty_shared_metrics() -> VmValue {
    shared_metrics_value(&SharedMetrics::default(), 0)
}

fn mailbox_metrics_value(mailbox: &Mailbox) -> VmValue {
    let mut value = BTreeMap::new();
    value.put_int("capacity", mailbox.capacity as i64);
    value.put_int("depth", mailbox.depth.load(Ordering::SeqCst));
    value.put_int(
        "sent_count",
        mailbox.sent_count.load(Ordering::SeqCst) as i64,
    );
    value.put_int(
        "received_count",
        mailbox.received_count.load(Ordering::SeqCst) as i64,
    );
    value.put_int(
        "failed_send_count",
        mailbox.failed_send_count.load(Ordering::SeqCst) as i64,
    );
    value.put_bool("closed", mailbox.channel.is_closed());
    VmValue::dict(value)
}

fn empty_mailbox_metrics() -> VmValue {
    let mut value = BTreeMap::new();
    value.put_int("capacity", 0);
    value.put_int("depth", 0);
    value.put_int("sent_count", 0);
    value.put_int("received_count", 0);
    value.put_int("failed_send_count", 0);
    value.put_bool("closed", false);
    VmValue::dict(value)
}

fn with_scope_fields(kind: &str, scoped: &ScopedKey, metrics: VmValue) -> VmValue {
    let mut value = metrics.as_dict().cloned().unwrap_or_default();
    value.put_str("_type", kind);
    value.put_str("scope", scoped.scope.clone());
    value.put_str("key", scoped.key.clone());
    VmValue::dict(value)
}

impl Default for SharedCell {
    fn default() -> Self {
        Self {
            value: VmValue::Nil,
            version: 0,
            metrics: SharedMetrics::default(),
        }
    }
}
