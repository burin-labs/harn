//! `std/session-store` — the canonical append-only, hash-chained session
//! event store other agent stores layer on.
//!
//! Each stream is a JSONL file at `<root>/.harn/session-store/<session_id>.jsonl`.
//! Every line is a session event whose shape mirrors the `harn-serve`
//! `StoredEvent`: `event_id` (1-based, monotonic), `session_id`, `tenant_id`,
//! `parent_event_id`, `actor`, `kind`, `payload`, `tags`, `headers`, `ts_ms`,
//! `ts`, `record_hash`, `prev_hash`. The record hash is
//! `"sha256:" + sha256(json_stringify({session_id, event_id, payload, prev_hash}))`,
//! computed with the *same* `vm_value_to_json` serializer and `sha256` the
//! Harn `json_stringify` / `sha256` builtins use — so a native append is
//! byte-identical to the equivalent Harn pipeline and to Burin's Swift store,
//! and chains they wrote interoperate with chains this module writes.
//!
//! Reads project the append-only log. A `payload` is treated as a *mutation*:
//! `{operation: "upsert", record: {id, ...}}`, `{operation: "delete", id}`,
//! `{operation: "replace", records: [...]}`, or `{operation: "clear"}`. The
//! collection projection folds these to the latest surviving record per `id`
//! in first-seen order; the value projection folds `replace`/`clear` to a
//! single value. This is the substrate for the richer memory surface, the
//! hypothesis store, and Burin's learned-context pipelines.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::stdlib::json::vm_value_to_json;
use crate::stdlib::json_to_vm_value;
use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{intern_key, DictMap, VmDictExt, VmError, VmValue};
use crate::vm::Vm;

const SESSION_STORE_DIR: &str = "session-store";
const EVENT_FILE_EXT: &str = "jsonl";
const DEFAULT_ACTOR: &str = "harn";
const DEFAULT_KIND_TYPE: &str = "LibrarianEntry";

pub(crate) fn register_session_store_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    &SESSION_STORE_APPEND_IMPL_DEF,
    &SESSION_STORE_EVENTS_IMPL_DEF,
    &SESSION_STORE_PAYLOADS_IMPL_DEF,
    &SESSION_STORE_PROJECT_IMPL_DEF,
    &SESSION_STORE_PROJECT_VALUE_IMPL_DEF,
    &SESSION_STORE_VERIFY_IMPL_DEF,
    &SESSION_STORE_PATH_IMPL_DEF,
];

// ---------------------------------------------------------------------------
// Builtins
// ---------------------------------------------------------------------------

#[harn_builtin(
    sig = "__session_store_append(session_id: string, payload: any, options?: dict) -> dict",
    category = "session_store"
)]
fn session_store_append_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let session_id = required_string(args, 0, "__session_store_append", "session_id")?;
    let payload = args.get(1).cloned().unwrap_or(VmValue::Nil);
    let options = args.get(2).and_then(VmValue::as_dict);
    let root = store_root(options);
    let event = append_event(&root, &session_id, payload, options)?;
    Ok(event)
}

#[harn_builtin(
    sig = "__session_store_events(session_id: string, options?: dict) -> list",
    category = "session_store"
)]
fn session_store_events_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let session_id = required_string(args, 0, "__session_store_events", "session_id")?;
    let options = args.get(1).and_then(VmValue::as_dict);
    let events = read_events(&store_root(options), &session_id)?;
    Ok(VmValue::List(std::sync::Arc::new(events)))
}

#[harn_builtin(
    sig = "__session_store_payloads(session_id: string, options?: dict) -> list",
    category = "session_store"
)]
fn session_store_payloads_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let session_id = required_string(args, 0, "__session_store_payloads", "session_id")?;
    let options = args.get(1).and_then(VmValue::as_dict);
    let events = read_events(&store_root(options), &session_id)?;
    let payloads = events
        .iter()
        .filter_map(event_payload)
        .filter(|payload| !matches!(payload, VmValue::Nil))
        .collect::<Vec<_>>();
    Ok(VmValue::List(std::sync::Arc::new(payloads)))
}

#[harn_builtin(
    sig = "__session_store_project(session_id: string, options?: dict) -> list",
    category = "session_store"
)]
fn session_store_project_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let session_id = required_string(args, 0, "__session_store_project", "session_id")?;
    let options = args.get(1).and_then(VmValue::as_dict);
    let events = read_events(&store_root(options), &session_id)?;
    let mutations = events.iter().filter_map(event_payload).collect::<Vec<_>>();
    Ok(VmValue::List(std::sync::Arc::new(project_collection(
        &mutations,
    ))))
}

#[harn_builtin(
    sig = "__session_store_project_value(session_id: string, default_value: any, options?: dict) -> any",
    category = "session_store"
)]
fn session_store_project_value_impl(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let session_id = required_string(args, 0, "__session_store_project_value", "session_id")?;
    let default_value = args.get(1).cloned().unwrap_or(VmValue::Nil);
    let options = args.get(2).and_then(VmValue::as_dict);
    let events = read_events(&store_root(options), &session_id)?;
    let mutations = events.iter().filter_map(event_payload).collect::<Vec<_>>();
    Ok(project_value(&mutations, default_value))
}

#[harn_builtin(
    sig = "__session_store_verify(session_id: string, options?: dict) -> dict",
    category = "session_store"
)]
fn session_store_verify_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let session_id = required_string(args, 0, "__session_store_verify", "session_id")?;
    let options = args.get(1).and_then(VmValue::as_dict);
    let events = read_events(&store_root(options), &session_id)?;
    Ok(verify_chain(&session_id, &events))
}

#[harn_builtin(
    sig = "__session_store_path(session_id: string, options?: dict) -> string",
    category = "session_store"
)]
fn session_store_path_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let session_id = required_string(args, 0, "__session_store_path", "session_id")?;
    let options = args.get(1).and_then(VmValue::as_dict);
    let path = session_path(&store_root(options), &session_id)?;
    Ok(VmValue::String(arcstr::ArcStr::from(
        path.to_string_lossy().as_ref(),
    )))
}

// ---------------------------------------------------------------------------
// Append + hash chain
// ---------------------------------------------------------------------------

fn append_event(
    root: &Path,
    session_id: &str,
    payload: VmValue,
    options: Option<&DictMap>,
) -> Result<VmValue, VmError> {
    let path = session_path(root, session_id)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            VmError::Runtime(format!(
                "session_store: failed to create {}: {error}",
                parent.display()
            ))
        })?;
    }
    let existing = read_events(root, session_id)?;
    let previous = existing.last();
    let event_id = previous
        .and_then(|event| dict_get(event, "event_id"))
        .and_then(coerce_i64)
        .unwrap_or(0)
        + 1;
    let prev_hash = previous
        .and_then(|event| dict_get(event, "record_hash"))
        .and_then(as_string);
    let parent_event_id = previous
        .and_then(|event| dict_get(event, "event_id"))
        .and_then(coerce_i64);

    // Record hash covers exactly {session_id, event_id, payload, prev_hash},
    // serialized by the same sorted-key writer `json_stringify` uses.
    let record_hash = compute_record_hash(session_id, event_id, &payload, prev_hash.as_deref());

    let kind_name = option_string(options, "kind").unwrap_or_else(|| "custom".to_string());
    let kind = if kind_name == "hypothesis" {
        let mut map = DictMap::new();
        map.put_str("kind", "hypothesis");
        VmValue::dict(map)
    } else {
        let kind_type =
            option_string(options, "kind_type").unwrap_or_else(|| DEFAULT_KIND_TYPE.to_string());
        let mut map = DictMap::new();
        map.put_str("kind", "custom");
        map.put_str("type", kind_type.as_str());
        VmValue::dict(map)
    };

    let now_iso = option_string(options, "now").unwrap_or_else(now_rfc3339);
    let ts_ms = parse_ts_ms(&now_iso);

    let mut event = DictMap::new();
    event.put_int("event_id", event_id);
    event.insert(intern_key("session_id"), string_value(session_id));
    event.insert(
        intern_key("tenant_id"),
        options
            .and_then(|opts| opts.get("tenant_id"))
            .cloned()
            .unwrap_or(VmValue::Nil),
    );
    event.insert(
        intern_key("parent_event_id"),
        parent_event_id.map(VmValue::Int).unwrap_or(VmValue::Nil),
    );
    event.insert(
        intern_key("actor"),
        string_value(&option_string(options, "actor").unwrap_or_else(|| DEFAULT_ACTOR.to_string())),
    );
    event.insert(intern_key("kind"), kind);
    event.insert(intern_key("payload"), payload);
    event.insert(
        intern_key("tags"),
        options
            .and_then(|opts| opts.get("tags"))
            .cloned()
            .unwrap_or_else(|| VmValue::List(std::sync::Arc::new(Vec::new()))),
    );
    event.insert(
        intern_key("headers"),
        options
            .and_then(|opts| opts.get("headers"))
            .cloned()
            .unwrap_or_else(|| VmValue::dict(DictMap::new())),
    );
    event.put_int("ts_ms", ts_ms);
    event.insert(intern_key("ts"), string_value(&now_iso));
    event.insert(intern_key("record_hash"), string_value(&record_hash));
    event.insert(
        intern_key("prev_hash"),
        prev_hash
            .map(|hash| string_value(&hash))
            .unwrap_or(VmValue::Nil),
    );
    let event = VmValue::dict(event);

    let line = format!("{}\n", vm_value_to_json(&event));
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|error| {
            VmError::Runtime(format!(
                "session_store: failed to open {}: {error}",
                path.display()
            ))
        })?;
    file.write_all(line.as_bytes()).map_err(|error| {
        VmError::Runtime(format!(
            "session_store: failed to append {}: {error}",
            path.display()
        ))
    })?;
    file.sync_data().map_err(|error| {
        VmError::Runtime(format!(
            "session_store: failed to sync {}: {error}",
            path.display()
        ))
    })?;
    Ok(event)
}

/// `"sha256:" + sha256(json_stringify({session_id, event_id, payload, prev_hash}))`.
/// The map is built via the sorted `DictMap`, so the serialized bytes are
/// deterministic and match what Burin's Harn pipeline computes for the same
/// inputs (both go through `vm_value_to_json`).
fn compute_record_hash(
    session_id: &str,
    event_id: i64,
    payload: &VmValue,
    prev_hash: Option<&str>,
) -> String {
    let mut map = DictMap::new();
    map.insert(intern_key("session_id"), string_value(session_id));
    map.put_int("event_id", event_id);
    map.insert(intern_key("payload"), payload.clone());
    map.insert(
        intern_key("prev_hash"),
        prev_hash.map(string_value).unwrap_or(VmValue::Nil),
    );
    let canonical = vm_value_to_json(&VmValue::dict(map));
    format!(
        "sha256:{}",
        hex::encode(Sha256::digest(canonical.as_bytes()))
    )
}

fn verify_chain(session_id: &str, events: &[VmValue]) -> VmValue {
    let mut result = DictMap::new();
    result.put_str("_type", "session_store_verify");
    result.put_str("session_id", session_id);
    result.put_int("count", events.len() as i64);

    let mut expected_prev: Option<String> = None;
    for (index, event) in events.iter().enumerate() {
        let event_id = dict_get(event, "event_id")
            .and_then(coerce_i64)
            .unwrap_or(-1);
        let payload = event_payload(event).unwrap_or(VmValue::Nil);
        let stored_hash = dict_get(event, "record_hash").and_then(as_string);
        let prev_hash = dict_get(event, "prev_hash").and_then(as_string);

        let seq_ok = event_id == (index as i64) + 1;
        let prev_ok = prev_hash == expected_prev;
        let recomputed = compute_record_hash(session_id, event_id, &payload, prev_hash.as_deref());
        let hash_ok = stored_hash.as_deref() == Some(recomputed.as_str());

        if !seq_ok || !prev_ok || !hash_ok {
            result.put_bool("ok", false);
            result.put_int("broken_at", index as i64);
            let reason = if !hash_ok {
                "record_hash mismatch"
            } else if !prev_ok {
                "prev_hash chain break"
            } else {
                "event_id sequence gap"
            };
            result.put_str("reason", reason);
            return VmValue::dict(result);
        }
        expected_prev = stored_hash;
    }
    result.put_bool("ok", true);
    VmValue::dict(result)
}

// ---------------------------------------------------------------------------
// Projection (mutation fold)
// ---------------------------------------------------------------------------

/// Fold a list of mutation payloads to the latest surviving record per `id`,
/// in first-seen order. Mirrors `session_store_project_collection` in Burin's
/// `session-store.harn`.
fn project_collection(mutations: &[VmValue]) -> Vec<VmValue> {
    let mut records: HashMap<String, VmValue> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    for mutation in mutations {
        match operation(mutation).as_deref() {
            Some("upsert") => {
                let record = dict_get(mutation, "record")
                    .cloned()
                    .unwrap_or(VmValue::Nil);
                let id = record_id(&record);
                if id.is_empty() {
                    continue;
                }
                if !records.contains_key(&id) {
                    order.push(id.clone());
                }
                records.insert(id, record);
            }
            Some("delete") => {
                let id = dict_get(mutation, "id")
                    .and_then(as_string)
                    .unwrap_or_default();
                let id = id.trim().to_string();
                records.remove(&id);
                order.retain(|existing| existing != &id);
            }
            Some("replace") => {
                records.clear();
                order.clear();
                if let Some(list) = dict_get(mutation, "records").and_then(as_list) {
                    for record in list.iter() {
                        let id = record_id(record);
                        if id.is_empty() {
                            continue;
                        }
                        if !records.contains_key(&id) {
                            order.push(id.clone());
                        }
                        records.insert(id, record.clone());
                    }
                }
            }
            Some("clear") => {
                records.clear();
                order.clear();
            }
            _ => {}
        }
    }
    order
        .into_iter()
        .filter_map(|id| records.get(&id).cloned())
        .collect()
}

/// Fold `replace`/`clear` mutations to a single value. Mirrors
/// `session_store_project_value`.
fn project_value(mutations: &[VmValue], default_value: VmValue) -> VmValue {
    let mut value = default_value.clone();
    for mutation in mutations {
        match operation(mutation).as_deref() {
            Some("replace") => {
                if let Some(next) = dict_get(mutation, "value") {
                    if !matches!(next, VmValue::Nil) {
                        value = next.clone();
                    }
                }
            }
            Some("clear") => value = default_value.clone(),
            _ => {}
        }
    }
    value
}

// ---------------------------------------------------------------------------
// I/O + paths
// ---------------------------------------------------------------------------

fn store_root(options: Option<&DictMap>) -> PathBuf {
    option_string(options, "root")
        .map(|root| crate::stdlib::process::resolve_source_relative_path(&root))
        .or_else(|| {
            std::env::var("HARN_SESSION_STORE_ROOT")
                .ok()
                .filter(|value| !value.trim().is_empty())
                .map(|root| crate::stdlib::process::resolve_source_relative_path(&root))
        })
        .unwrap_or_else(crate::stdlib::process::runtime_root_base)
}

fn session_path(root: &Path, session_id: &str) -> Result<PathBuf, VmError> {
    let file = safe_stream_file(session_id)?;
    Ok(root.join(".harn").join(SESSION_STORE_DIR).join(file))
}

/// A session id becomes a single JSONL filename. Dots are allowed (stream
/// names like `burin.agent_context.memories.project` are dotted), but path
/// separators and `..` are rejected so a stream can never escape the store.
fn safe_stream_file(session_id: &str) -> Result<String, VmError> {
    let trimmed = session_id.trim();
    if trimmed.is_empty() {
        return Err(VmError::Runtime(
            "session_store: session_id must be non-empty".to_string(),
        ));
    }
    if trimmed == "." || trimmed == ".." || trimmed.contains("..") {
        return Err(VmError::Runtime(
            "session_store: session_id must not contain `..`".to_string(),
        ));
    }
    if trimmed.contains('/') || trimmed.contains('\\') || trimmed.contains('\0') {
        return Err(VmError::Runtime(
            "session_store: session_id must not contain path separators".to_string(),
        ));
    }
    Ok(format!("{trimmed}.{EVENT_FILE_EXT}"))
}

fn read_events(root: &Path, session_id: &str) -> Result<Vec<VmValue>, VmError> {
    let path = session_path(root, session_id)?;
    let file = match fs::File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(VmError::Runtime(format!(
                "session_store: failed to read {}: {error}",
                path.display()
            )))
        }
    };
    let reader = BufReader::new(file);
    let mut events = Vec::new();
    for (idx, line) in reader.lines().enumerate() {
        let line = line.map_err(|error| {
            VmError::Runtime(format!(
                "session_store: failed to read line {} from {}: {error}",
                idx + 1,
                path.display()
            ))
        })?;
        if line.trim().is_empty() {
            continue;
        }
        // Skip unparseable lines rather than aborting the whole read, matching
        // Burin's tolerant `session_store_read_events`.
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) {
            events.push(json_to_vm_value(&value));
        }
    }
    Ok(events)
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

fn required_string(
    args: &[VmValue],
    idx: usize,
    fn_name: &str,
    arg_name: &str,
) -> Result<String, VmError> {
    args.get(idx)
        .map(VmValue::display)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            VmError::Runtime(format!(
                "{fn_name}: `{arg_name}` must be a non-empty string"
            ))
        })
}

fn option_string(options: Option<&DictMap>, key: &str) -> Option<String> {
    options
        .and_then(|opts| opts.get(key))
        .map(VmValue::display)
        .filter(|value| !value.trim().is_empty())
}

fn dict_get<'a>(value: &'a VmValue, key: &str) -> Option<&'a VmValue> {
    value.as_dict().and_then(|dict| dict.get(key))
}

fn event_payload(event: &VmValue) -> Option<VmValue> {
    dict_get(event, "payload").cloned()
}

fn operation(mutation: &VmValue) -> Option<String> {
    dict_get(mutation, "operation")
        .and_then(as_string)
        .map(|op| op.trim().to_string())
}

fn record_id(record: &VmValue) -> String {
    dict_get(record, "id")
        .and_then(as_string)
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn as_string(value: &VmValue) -> Option<String> {
    match value {
        VmValue::Nil => None,
        other => Some(other.display()),
    }
}

fn as_list(value: &VmValue) -> Option<std::sync::Arc<Vec<VmValue>>> {
    match value {
        VmValue::List(items) => Some(items.clone()),
        _ => None,
    }
}

fn coerce_i64(value: &VmValue) -> Option<i64> {
    match value {
        VmValue::Int(raw) => Some(*raw),
        VmValue::Float(raw) if raw.is_finite() => Some(*raw as i64),
        _ => None,
    }
}

fn string_value(value: &str) -> VmValue {
    VmValue::String(arcstr::ArcStr::from(value))
}

fn now_rfc3339() -> String {
    rfc3339_from_epoch_ms(crate::stdlib::clock::now_wall_ms())
}

fn parse_ts_ms(ts: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(ts)
        .map(|dt| dt.timestamp_millis())
        .unwrap_or_else(|_| crate::stdlib::clock::now_wall_ms())
}

fn rfc3339_from_epoch_ms(ms: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms)
        .unwrap_or(chrono::DateTime::<chrono::Utc>::UNIX_EPOCH)
        .to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "harn-session-store-test-{name}-{}",
            uuid::Uuid::now_v7()
        ))
    }

    fn upsert(id: &str, title: &str) -> VmValue {
        let mut record = DictMap::new();
        record.put_str("id", id);
        record.put_str("title", title);
        let mut mutation = DictMap::new();
        mutation.put_str("operation", "upsert");
        mutation.insert(intern_key("record"), VmValue::dict(record));
        VmValue::dict(mutation)
    }

    #[test]
    fn append_chains_event_id_and_prev_hash() {
        let root = temp_root("chain");
        let first = append_event(&root, "s1", upsert("a", "one"), None).unwrap();
        let second = append_event(&root, "s1", upsert("b", "two"), None).unwrap();
        assert_eq!(dict_get(&first, "event_id").and_then(coerce_i64), Some(1));
        assert_eq!(dict_get(&second, "event_id").and_then(coerce_i64), Some(2));
        assert!(matches!(dict_get(&first, "prev_hash"), Some(VmValue::Nil)));
        assert_eq!(
            dict_get(&second, "prev_hash").and_then(as_string),
            dict_get(&first, "record_hash").and_then(as_string)
        );
        let first_hash = dict_get(&first, "record_hash").and_then(as_string).unwrap();
        assert!(first_hash.starts_with("sha256:"));

        let events = read_events(&root, "s1").unwrap();
        let report = verify_chain("s1", &events);
        assert!(matches!(dict_get(&report, "ok"), Some(VmValue::Bool(true))));
    }

    #[test]
    fn record_hash_matches_json_stringify_formula() {
        // Independently reproduce Burin's formula and confirm parity.
        let root = temp_root("parity");
        let event = append_event(&root, "s1", upsert("a", "one"), None).unwrap();
        let payload = event_payload(&event).unwrap();
        let mut map = DictMap::new();
        map.insert(intern_key("session_id"), string_value("s1"));
        map.put_int("event_id", 1);
        map.insert(intern_key("payload"), payload);
        map.insert(intern_key("prev_hash"), VmValue::Nil);
        let canonical = vm_value_to_json(&VmValue::dict(map));
        let expected = format!(
            "sha256:{}",
            hex::encode(Sha256::digest(canonical.as_bytes()))
        );
        assert_eq!(
            dict_get(&event, "record_hash").and_then(as_string),
            Some(expected)
        );
        // Sanity: canonical JSON is sorted (event_id before session_id).
        assert!(canonical.starts_with("{\"event_id\":1,"));
    }

    #[test]
    fn project_collection_folds_mutations() {
        let root = temp_root("project");
        append_event(&root, "s1", upsert("a", "v1"), None).unwrap();
        append_event(&root, "s1", upsert("a", "v2"), None).unwrap();
        append_event(&root, "s1", upsert("b", "keep"), None).unwrap();
        // delete b
        let mut del = DictMap::new();
        del.put_str("operation", "delete");
        del.put_str("id", "b");
        append_event(&root, "s1", VmValue::dict(del), None).unwrap();

        let events = read_events(&root, "s1").unwrap();
        let mutations = events.iter().filter_map(event_payload).collect::<Vec<_>>();
        let records = project_collection(&mutations);
        assert_eq!(records.len(), 1);
        assert_eq!(record_id(&records[0]), "a");
        assert_eq!(
            dict_get(&records[0], "title").and_then(as_string),
            Some("v2".to_string())
        );
    }

    #[test]
    fn verify_detects_tamper() {
        let root = temp_root("tamper");
        append_event(&root, "s1", upsert("a", "one"), None).unwrap();
        append_event(&root, "s1", upsert("b", "two"), None).unwrap();
        let mut events = read_events(&root, "s1").unwrap();
        // Tamper: rewrite the first payload without recomputing the hash.
        let mut tampered = events[0].as_dict().unwrap().clone();
        tampered.insert(intern_key("payload"), upsert("a", "HACKED"));
        events[0] = VmValue::dict(tampered);
        let report = verify_chain("s1", &events);
        assert!(matches!(
            dict_get(&report, "ok"),
            Some(VmValue::Bool(false))
        ));
        assert_eq!(dict_get(&report, "broken_at").and_then(coerce_i64), Some(0));
    }

    #[test]
    fn stream_id_escape_is_rejected() {
        assert!(safe_stream_file("../evil").is_err());
        assert!(safe_stream_file("a/b").is_err());
        assert!(safe_stream_file("burin.agent_context.memories.project").is_ok());
    }
}
