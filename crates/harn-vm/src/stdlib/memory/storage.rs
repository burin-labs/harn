use std::collections::{BTreeSet, HashMap};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Component, Path, PathBuf};

use super::{
    sha256_hex, MemoryEvent, MemoryRecord, NamespaceConfig, UpdateEvent, EVENT_LOG_FILE,
    VECTOR_CACHE_DIR,
};
use crate::value::VmError;

fn namespace_dir(root: &Path, namespace: &str) -> Result<PathBuf, VmError> {
    Ok(root.join(normalize_relative_component(namespace, "memory namespace")?))
}

pub(super) fn event_log_path(root: &Path, namespace: &str) -> Result<PathBuf, VmError> {
    Ok(namespace_dir(root, namespace)?.join(EVENT_LOG_FILE))
}

pub(super) fn vector_cache_path(
    root: &Path,
    namespace: &str,
    model_hint: &str,
    content_hash: &str,
) -> Result<PathBuf, VmError> {
    Ok(namespace_dir(root, namespace)?
        .join(VECTOR_CACHE_DIR)
        .join(sha256_hex(model_hint.as_bytes()))
        .join(format!("{content_hash}.json")))
}

fn normalize_relative_component(raw: &str, label: &str) -> Result<PathBuf, VmError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(VmError::Runtime(format!("{label} must be non-empty")));
    }
    let candidate = Path::new(raw);
    if candidate.is_absolute() {
        return Err(VmError::Runtime(format!("{label} must be relative")));
    }
    let mut normalized = PathBuf::new();
    for component in candidate.components() {
        match component {
            Component::Normal(part) => normalized.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(VmError::Runtime(format!(
                    "{label} must not escape the memory root"
                )))
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        return Err(VmError::Runtime(format!(
            "{label} must contain a path component"
        )));
    }
    Ok(normalized)
}

pub(super) fn append_event(
    root: &Path,
    namespace: &str,
    event: &MemoryEvent,
) -> Result<(), VmError> {
    let path = event_log_path(root, namespace)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            VmError::Runtime(format!(
                "memory: failed to create {}: {error}",
                parent.display()
            ))
        })?;
    }
    let line = serde_json::to_string(event)
        .map_err(|error| VmError::Runtime(format!("memory: encode error: {error}")))?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|error| {
            VmError::Runtime(format!(
                "memory: failed to open {}: {error}",
                path.display()
            ))
        })?;
    let mut bytes = line.into_bytes();
    bytes.push(b'\n');
    file.write_all(&bytes).map_err(|error| {
        VmError::Runtime(format!(
            "memory: failed to append {}: {error}",
            path.display()
        ))
    })?;
    file.sync_data().map_err(|error| {
        VmError::Runtime(format!(
            "memory: failed to sync {}: {error}",
            path.display()
        ))
    })?;
    Ok(())
}

fn read_events(root: &Path, namespace: &str) -> Result<Vec<MemoryEvent>, VmError> {
    let path = event_log_path(root, namespace)?;
    let file = match fs::File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(VmError::Runtime(format!(
                "memory: failed to read {}: {error}",
                path.display()
            )))
        }
    };
    let reader = BufReader::new(file);
    let mut events = Vec::new();
    for (idx, line) in reader.lines().enumerate() {
        let line = line.map_err(|error| {
            VmError::Runtime(format!(
                "memory: failed to read line {} from {}: {error}",
                idx + 1,
                path.display()
            ))
        })?;
        if line.trim().is_empty() {
            continue;
        }
        let event = serde_json::from_str::<MemoryEvent>(&line).map_err(|error| {
            VmError::Runtime(format!(
                "memory: failed to parse line {} from {}: {error}",
                idx + 1,
                path.display()
            ))
        })?;
        events.push(event);
    }
    Ok(events)
}

pub(super) fn active_records(
    root: &Path,
    namespace: &str,
) -> Result<Vec<(usize, MemoryRecord)>, VmError> {
    let events = read_events(root, namespace)?;
    let mut records: Vec<(usize, MemoryRecord)> = Vec::new();
    let mut position: HashMap<String, usize> = HashMap::new();
    let mut forgotten = BTreeSet::new();
    for event in &events {
        if let MemoryEvent::Forget(event) = event {
            forgotten.extend(event.forgotten_ids.iter().cloned());
        }
    }
    for (idx, event) in events.into_iter().enumerate() {
        match event {
            MemoryEvent::Store(record) if !forgotten.contains(&record.id) => {
                position.insert(record.id.clone(), records.len());
                records.push((idx, record));
            }
            MemoryEvent::Update(update) if !forgotten.contains(&update.id) => {
                if let Some(&pos) = position.get(&update.id) {
                    apply_update(&mut records[pos].1, &update);
                }
            }
            _ => {}
        }
    }
    Ok(records)
}

/// Absent fields leave the record untouched; flags merge by key.
fn apply_update(record: &mut MemoryRecord, update: &UpdateEvent) {
    if let Some(value) = &update.value {
        record.value = value.clone();
    }
    if let Some(text) = &update.text {
        record.text = text.clone();
    }
    if let Some(tags) = &update.tags {
        record.tags = tags.clone();
    }
    if update.status.is_some() {
        record.status = update.status.clone();
    }
    if update.scope.is_some() {
        record.scope = update.scope.clone();
    }
    for (key, value) in &update.flags {
        record.flags.insert(key.clone(), *value);
    }
    if update.provenance.is_some() {
        record.provenance = update.provenance.clone();
    }
}

pub(super) fn read_namespace_config(
    root: &Path,
    namespace: &str,
) -> Result<NamespaceConfig, VmError> {
    let events = read_events(root, namespace)?;
    let mut config = NamespaceConfig::default();
    for event in events {
        if let MemoryEvent::Open(open) = event {
            config = NamespaceConfig {
                backend: open.backend,
                embed_model_hint: open.embed_model_hint,
                embed_dim: open.embed_dim,
                bm25_weight: open.bm25_weight,
                cosine_weight: open.cosine_weight,
            };
        }
    }
    Ok(config)
}
