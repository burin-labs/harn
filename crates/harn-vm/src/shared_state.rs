use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;

use crate::value::{values_equal, VmChannelHandle, VmValue};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ScopedKey {
    pub(crate) scope: String,
    pub(crate) key: String,
}

struct SharedCell {
    value: VmValue,
    version: u64,
    metrics: SharedMetrics,
}

#[derive(Default)]
struct SharedMap {
    entries: BTreeMap<String, VmValue>,
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

#[derive(Default)]
pub(crate) struct VmSharedStateRuntime {
    cells: RefCell<BTreeMap<ScopedKey, SharedCell>>,
    maps: RefCell<BTreeMap<ScopedKey, SharedMap>>,
    mailboxes: RefCell<BTreeMap<ScopedKey, Mailbox>>,
}

impl VmSharedStateRuntime {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn open_cell(&self, scoped: ScopedKey, initial: VmValue) -> VmValue {
        self.cells
            .borrow_mut()
            .entry(scoped.clone())
            .or_insert_with(|| SharedCell {
                value: initial,
                version: 0,
                metrics: SharedMetrics::default(),
            });
        handle_value("shared_cell", &scoped)
    }

    pub(crate) fn cell_get(&self, scoped: &ScopedKey) -> VmValue {
        let mut cells = self.cells.borrow_mut();
        let cell = cells.entry(scoped.clone()).or_default();
        cell.metrics.read_count += 1;
        cell.value.clone()
    }

    pub(crate) fn cell_snapshot(&self, scoped: &ScopedKey) -> VmValue {
        let mut cells = self.cells.borrow_mut();
        let cell = cells.entry(scoped.clone()).or_default();
        cell.metrics.read_count += 1;
        snapshot_value(cell.value.clone(), cell.version)
    }

    pub(crate) fn cell_set(&self, scoped: &ScopedKey, value: VmValue) -> VmValue {
        let mut cells = self.cells.borrow_mut();
        let cell = cells.entry(scoped.clone()).or_default();
        let old = std::mem::replace(&mut cell.value, value);
        cell.version = cell.version.saturating_add(1);
        cell.metrics.write_count += 1;
        old
    }

    pub(crate) fn cell_cas(
        &self,
        scoped: &ScopedKey,
        expected: &VmValue,
        new_value: VmValue,
    ) -> bool {
        let mut cells = self.cells.borrow_mut();
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

    pub(crate) fn open_map(
        &self,
        scoped: ScopedKey,
        initial: Option<BTreeMap<String, VmValue>>,
    ) -> VmValue {
        self.maps
            .borrow_mut()
            .entry(scoped.clone())
            .or_insert_with(|| SharedMap {
                entries: initial.unwrap_or_default(),
                version: 0,
                metrics: SharedMetrics::default(),
            });
        handle_value("shared_map", &scoped)
    }

    pub(crate) fn map_get(&self, scoped: &ScopedKey, key: &str, default: VmValue) -> VmValue {
        let mut maps = self.maps.borrow_mut();
        let map = maps.entry(scoped.clone()).or_default();
        map.metrics.read_count += 1;
        map.entries.get(key).cloned().unwrap_or(default)
    }

    pub(crate) fn map_snapshot(&self, scoped: &ScopedKey, key: &str) -> VmValue {
        let mut maps = self.maps.borrow_mut();
        let map = maps.entry(scoped.clone()).or_default();
        map.metrics.read_count += 1;
        snapshot_value(
            map.entries.get(key).cloned().unwrap_or(VmValue::Nil),
            map.version,
        )
    }

    pub(crate) fn map_entries(&self, scoped: &ScopedKey) -> VmValue {
        let mut maps = self.maps.borrow_mut();
        let map = maps.entry(scoped.clone()).or_default();
        map.metrics.read_count += 1;
        VmValue::Dict(Rc::new(map.entries.clone()))
    }

    pub(crate) fn map_set(&self, scoped: &ScopedKey, key: String, value: VmValue) -> VmValue {
        let mut maps = self.maps.borrow_mut();
        let map = maps.entry(scoped.clone()).or_default();
        let old = map.entries.insert(key, value).unwrap_or(VmValue::Nil);
        map.version = map.version.saturating_add(1);
        map.metrics.write_count += 1;
        old
    }

    pub(crate) fn map_delete(&self, scoped: &ScopedKey, key: &str) -> VmValue {
        let mut maps = self.maps.borrow_mut();
        let map = maps.entry(scoped.clone()).or_default();
        let old = map.entries.remove(key).unwrap_or(VmValue::Nil);
        if !matches!(old, VmValue::Nil) {
            map.version = map.version.saturating_add(1);
            map.metrics.write_count += 1;
        }
        old
    }

    pub(crate) fn map_cas(
        &self,
        scoped: &ScopedKey,
        key: String,
        expected: &VmValue,
        new_value: VmValue,
    ) -> bool {
        let mut maps = self.maps.borrow_mut();
        let map = maps.entry(scoped.clone()).or_default();
        let current = map.entries.get(&key).cloned().unwrap_or(VmValue::Nil);
        let (expected_value, expected_version) = snapshot_expected(expected);
        let version_matches = expected_version.is_none_or(|version| version == map.version);
        let value_matches = values_equal(&current, &expected_value);
        if version_matches && value_matches {
            if matches!(new_value, VmValue::Nil) {
                map.entries.remove(&key);
            } else {
                map.entries.insert(key, new_value);
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

    pub(crate) fn open_mailbox(&self, scoped: ScopedKey, capacity: usize) -> VmValue {
        self.mailboxes
            .borrow_mut()
            .entry(scoped.clone())
            .or_insert_with(|| {
                let capacity = capacity.max(1);
                let (sender, receiver) = tokio::sync::mpsc::channel(capacity);
                // Mailboxes follow the same local-task invariant as channels:
                // VmValue is !Send and these handles stay on the VM LocalSet.
                #[allow(clippy::arc_with_non_send_sync)]
                let channel = VmChannelHandle {
                    name: Rc::from(scoped.key.clone()),
                    sender: Arc::new(sender),
                    receiver: Arc::new(tokio::sync::Mutex::new(receiver)),
                    closed: Arc::new(AtomicBool::new(false)),
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

    pub(crate) fn mailbox(&self, scoped: &ScopedKey) -> Option<VmValue> {
        if self.mailboxes.borrow().contains_key(scoped) {
            Some(handle_value("mailbox", scoped))
        } else {
            None
        }
    }

    pub(crate) fn mailbox_channel(&self, scoped: &ScopedKey) -> Option<VmChannelHandle> {
        self.mailboxes
            .borrow()
            .get(scoped)
            .map(|mailbox| mailbox.channel.clone())
    }

    pub(crate) fn note_mailbox_send(&self, scoped: &ScopedKey, ok: bool) {
        if let Some(mailbox) = self.mailboxes.borrow().get(scoped) {
            if ok {
                mailbox.sent_count.fetch_add(1, Ordering::SeqCst);
                mailbox.depth.fetch_add(1, Ordering::SeqCst);
            } else {
                mailbox.failed_send_count.fetch_add(1, Ordering::SeqCst);
            }
        }
    }

    pub(crate) fn note_mailbox_receive(&self, scoped: &ScopedKey) {
        if let Some(mailbox) = self.mailboxes.borrow().get(scoped) {
            mailbox.received_count.fetch_add(1, Ordering::SeqCst);
            let _ = mailbox
                .depth
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |depth| {
                    Some(depth.saturating_sub(1))
                });
        }
    }

    pub(crate) fn close_mailbox(&self, scoped: &ScopedKey) -> bool {
        let Some(mailbox) = self.mailboxes.borrow().get(scoped).cloned() else {
            return false;
        };
        !mailbox.channel.closed.swap(true, Ordering::SeqCst)
    }

    pub(crate) fn metrics(&self, kind: Option<&str>, scoped: Option<&ScopedKey>) -> VmValue {
        match (kind, scoped) {
            (Some("shared_cell"), Some(scoped)) => self
                .cells
                .borrow()
                .get(scoped)
                .map(|cell| shared_metrics_value(&cell.metrics, cell.version))
                .unwrap_or_else(empty_shared_metrics),
            (Some("shared_map"), Some(scoped)) => self
                .maps
                .borrow()
                .get(scoped)
                .map(|map| shared_metrics_value(&map.metrics, map.version))
                .unwrap_or_else(empty_shared_metrics),
            (Some("mailbox"), Some(scoped)) => self
                .mailboxes
                .borrow()
                .get(scoped)
                .map(mailbox_metrics_value)
                .unwrap_or_else(empty_mailbox_metrics),
            _ => {
                let mut values = Vec::new();
                for (scoped, cell) in self.cells.borrow().iter() {
                    values.push(with_scope_fields(
                        "shared_cell",
                        scoped,
                        shared_metrics_value(&cell.metrics, cell.version),
                    ));
                }
                for (scoped, map) in self.maps.borrow().iter() {
                    values.push(with_scope_fields(
                        "shared_map",
                        scoped,
                        shared_metrics_value(&map.metrics, map.version),
                    ));
                }
                for (scoped, mailbox) in self.mailboxes.borrow().iter() {
                    values.push(with_scope_fields(
                        "mailbox",
                        scoped,
                        mailbox_metrics_value(mailbox),
                    ));
                }
                VmValue::List(Rc::new(values))
            }
        }
    }
}

fn handle_value(kind: &str, scoped: &ScopedKey) -> VmValue {
    let mut value = BTreeMap::new();
    value.insert(
        "_type".to_string(),
        VmValue::String(Rc::from(kind.to_string())),
    );
    value.insert(
        "scope".to_string(),
        VmValue::String(Rc::from(scoped.scope.clone())),
    );
    value.insert(
        "key".to_string(),
        VmValue::String(Rc::from(scoped.key.clone())),
    );
    VmValue::Dict(Rc::new(value))
}

fn snapshot_value(value: VmValue, version: u64) -> VmValue {
    let mut snapshot = BTreeMap::new();
    snapshot.insert(
        "_type".to_string(),
        VmValue::String(Rc::from("shared_snapshot")),
    );
    snapshot.insert("value".to_string(), value);
    snapshot.insert("version".to_string(), VmValue::Int(version as i64));
    VmValue::Dict(Rc::new(snapshot))
}

fn snapshot_expected(value: &VmValue) -> (VmValue, Option<u64>) {
    let Some(dict) = value.as_dict() else {
        return (value.clone(), None);
    };
    let is_snapshot = matches!(
        dict.get("_type"),
        Some(VmValue::String(kind)) if kind.as_ref() == "shared_snapshot"
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
    value.insert("version".to_string(), VmValue::Int(version as i64));
    value.insert(
        "read_count".to_string(),
        VmValue::Int(metrics.read_count as i64),
    );
    value.insert(
        "write_count".to_string(),
        VmValue::Int(metrics.write_count as i64),
    );
    value.insert(
        "cas_success_count".to_string(),
        VmValue::Int(metrics.cas_success_count as i64),
    );
    value.insert(
        "cas_failure_count".to_string(),
        VmValue::Int(metrics.cas_failure_count as i64),
    );
    value.insert(
        "stale_read_count".to_string(),
        VmValue::Int(metrics.stale_read_count as i64),
    );
    VmValue::Dict(Rc::new(value))
}

fn empty_shared_metrics() -> VmValue {
    shared_metrics_value(&SharedMetrics::default(), 0)
}

fn mailbox_metrics_value(mailbox: &Mailbox) -> VmValue {
    let mut value = BTreeMap::new();
    value.insert(
        "capacity".to_string(),
        VmValue::Int(mailbox.capacity as i64),
    );
    value.insert(
        "depth".to_string(),
        VmValue::Int(mailbox.depth.load(Ordering::SeqCst)),
    );
    value.insert(
        "sent_count".to_string(),
        VmValue::Int(mailbox.sent_count.load(Ordering::SeqCst) as i64),
    );
    value.insert(
        "received_count".to_string(),
        VmValue::Int(mailbox.received_count.load(Ordering::SeqCst) as i64),
    );
    value.insert(
        "failed_send_count".to_string(),
        VmValue::Int(mailbox.failed_send_count.load(Ordering::SeqCst) as i64),
    );
    value.insert(
        "closed".to_string(),
        VmValue::Bool(mailbox.channel.closed.load(Ordering::SeqCst)),
    );
    VmValue::Dict(Rc::new(value))
}

fn empty_mailbox_metrics() -> VmValue {
    let mut value = BTreeMap::new();
    value.insert("capacity".to_string(), VmValue::Int(0));
    value.insert("depth".to_string(), VmValue::Int(0));
    value.insert("sent_count".to_string(), VmValue::Int(0));
    value.insert("received_count".to_string(), VmValue::Int(0));
    value.insert("failed_send_count".to_string(), VmValue::Int(0));
    value.insert("closed".to_string(), VmValue::Bool(false));
    VmValue::Dict(Rc::new(value))
}

fn with_scope_fields(kind: &str, scoped: &ScopedKey, metrics: VmValue) -> VmValue {
    let mut value = metrics.as_dict().cloned().unwrap_or_default();
    value.insert(
        "_type".to_string(),
        VmValue::String(Rc::from(kind.to_string())),
    );
    value.insert(
        "scope".to_string(),
        VmValue::String(Rc::from(scoped.scope.clone())),
    );
    value.insert(
        "key".to_string(),
        VmValue::String(Rc::from(scoped.key.clone())),
    );
    VmValue::Dict(Rc::new(value))
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
