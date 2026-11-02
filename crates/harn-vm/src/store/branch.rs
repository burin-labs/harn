use super::{SharedStore, StoreState};
use crate::value::VmError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub(crate) struct BranchStores(BTreeMap<PathBuf, SharedStore>);

impl BranchStores {
    pub(crate) fn at(&mut self, path: PathBuf) -> SharedStore {
        Arc::clone(
            self.0
                .entry(path.clone())
                .or_insert_with(|| Arc::new(parking_lot::Mutex::new(StoreState::at_path(path)))),
        )
    }
}

pub(super) fn for_execution(store: &SharedStore) -> Result<SharedStore, VmError> {
    let Some(branch) = crate::run_tape::current_branch() else {
        return Ok(Arc::clone(store));
    };
    let path = store.lock().path.clone();
    let path = crate::run_tape::durable_path(&path, "runtime store")?;
    Ok(branch.runtime_store(path))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct StoreChange {
    store: String,
    operation: Operation,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Operation {
    Set {
        // The tape redactor classifies JSON keys, so preserve the store key's position.
        entry: BTreeMap<String, serde_json::Value>,
    },
    Delete {
        key: String,
    },
    Clear,
}

tokio::task_local! {
    static CHANGES: Arc<Mutex<Vec<StoreChange>>>;
}

pub(crate) async fn capture_dispatch<F: std::future::Future>(
    live: F,
) -> (F::Output, Vec<StoreChange>) {
    let parent = CHANGES.try_with(Arc::clone).ok();
    let changes = Arc::new(Mutex::new(Vec::new()));
    let output = CHANGES.scope(changes.clone(), live).await;
    let changes = std::mem::take(&mut *changes.lock().expect("store frame poisoned"));
    if let Some(parent) = parent {
        parent
            .lock()
            .expect("store frame poisoned")
            .extend(changes.iter().cloned());
    }
    (output, changes)
}

fn note(path: &Path, operation: impl FnOnce() -> Operation) {
    let _ = CHANGES.try_with(|changes| {
        changes
            .lock()
            .expect("store frame poisoned")
            .push(StoreChange {
                store: crate::run_tape::recording_store_identity(path),
                operation: operation(),
            });
    });
}

pub(super) fn note_set(path: &Path, key: &str, value: &serde_json::Value) {
    note(path, || Operation::Set {
        entry: BTreeMap::from([(key.to_string(), value.clone())]),
    });
}

pub(super) fn note_delete(path: &Path, key: &str) {
    note(path, || Operation::Delete {
        key: key.to_string(),
    });
}

pub(super) fn note_clear(path: &Path) {
    note(path, || Operation::Clear);
}

pub(crate) fn decode_dispatch(bytes: &[u8]) -> Result<Vec<StoreChange>, String> {
    let changes: Vec<StoreChange> = serde_json::from_slice(bytes)
        .map_err(|error| format!("tape runtime store changes: {error}"))?;
    for change in &changes {
        if matches!(&change.operation, Operation::Set { entry } if entry.len() != 1) {
            return Err("a recorded store set must contain exactly one key".to_string());
        }
        let path = Path::new(&change.store);
        if path.as_os_str().is_empty()
            || path.file_name() != Some(std::ffi::OsStr::new(crate::runtime_paths::STORE_FILE))
            || path
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err(
                "a recorded runtime store must name its store file inside the branch state root"
                    .to_string(),
            );
        }
    }
    Ok(changes)
}

pub(crate) fn replay_dispatch(changes: &[StoreChange]) -> Result<(), String> {
    let branch = crate::run_tape::current_branch()
        .ok_or_else(|| "runtime store replay requires an active branch".to_string())?;
    for change in changes {
        let path = branch.state_root().join(&change.store);
        let path = crate::run_tape::durable_path(&path, "runtime store")
            .map_err(|error| format!("replay runtime store: {error}"))?;
        let store = branch.runtime_store(path);
        let mut store = store.lock();
        match &change.operation {
            Operation::Set { entry } => {
                let (key, value) = entry.first_key_value().expect("validated single-key set");
                store.set(key.clone(), value.clone())
            }
            Operation::Delete { key } => store.delete(key),
            Operation::Clear => store.clear(),
        }
        .map_err(|error| format!("replay runtime store: {error}"))?;
    }
    Ok(())
}
