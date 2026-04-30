use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::value::{VmError, VmValue};
use crate::vm::Vm;

const MEMORY_TYPE: &str = "memory_record";
const EVENT_LOG_FILE: &str = "events.jsonl";
const DEFAULT_RECALL_LIMIT: usize = 5;
const DEFAULT_SUMMARY_LIMIT: usize = 20;
const MAX_RECALL_LIMIT: usize = 100;
const MAX_SUMMARY_LIMIT: usize = 200;
const MAX_SUMMARY_CHARS: usize = 4000;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum MemoryEvent {
    Store(MemoryRecord),
    Forget(ForgetEvent),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct MemoryRecord {
    id: String,
    namespace: String,
    key: String,
    value: JsonValue,
    #[serde(default)]
    text: String,
    #[serde(default)]
    tags: Vec<String>,
    stored_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provenance: Option<JsonValue>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ForgetEvent {
    id: String,
    namespace: String,
    predicate: JsonValue,
    forgotten_ids: Vec<String>,
    forgotten_at: String,
}

#[derive(Clone, Debug)]
struct ScoredRecord {
    record: MemoryRecord,
    score: f64,
    sequence: usize,
}

pub(crate) fn register_memory_builtins(vm: &mut Vm) {
    vm.register_builtin("__memory_store", |args, _out| {
        let namespace = required_string(args, 0, "__memory_store", "namespace")?;
        let key = required_string(args, 1, "__memory_store", "key")?;
        let value = args.get(2).cloned().ok_or_else(|| {
            VmError::Runtime("__memory_store: `value` argument is required".to_string())
        })?;
        let tags = parse_tags(args.get(3), "__memory_store")?;
        let options = args.get(4).and_then(VmValue::as_dict);
        let root = memory_root(options);
        let record = MemoryRecord {
            id: option_string(options, "id").unwrap_or_else(|| uuid::Uuid::now_v7().to_string()),
            namespace: namespace.clone(),
            key,
            value: crate::llm::vm_value_to_json(&value),
            text: value_to_search_text(&value),
            tags,
            stored_at: option_string(options, "now").unwrap_or_else(now_rfc3339),
            provenance: options
                .and_then(|opts| opts.get("provenance"))
                .map(crate::llm::vm_value_to_json),
        };
        append_event(&root, &namespace, &MemoryEvent::Store(record.clone()))?;
        Ok(memory_record_to_vm(&record, None))
    });

    vm.register_builtin("__memory_recall", |args, _out| {
        let namespace = required_string(args, 0, "__memory_recall", "namespace")?;
        let query = required_string(args, 1, "__memory_recall", "query")?;
        let limit = optional_usize(args.get(2))
            .unwrap_or(DEFAULT_RECALL_LIMIT)
            .clamp(1, MAX_RECALL_LIMIT);
        let options = args.get(3).and_then(VmValue::as_dict);
        let root = memory_root(options);
        let records = active_records(&root, &namespace)?;
        let mut scored = score_records(records, &query);
        scored.truncate(limit);
        Ok(VmValue::List(Rc::new(
            scored
                .iter()
                .map(|item| memory_record_to_vm(&item.record, Some(item.score)))
                .collect(),
        )))
    });

    vm.register_builtin("__memory_summarize", |args, _out| {
        let namespace = required_string(args, 0, "__memory_summarize", "namespace")?;
        let window = args.get(1);
        let options = args.get(2).and_then(VmValue::as_dict);
        let root = memory_root(options);
        let mut records = active_records(&root, &namespace)?;
        records.sort_by(|left, right| {
            left.1
                .stored_at
                .cmp(&right.1.stored_at)
                .then_with(|| left.0.cmp(&right.0))
        });
        let selected = select_summary_records(records, window)?;
        Ok(summary_to_vm(&namespace, selected))
    });

    vm.register_builtin("__memory_forget", |args, _out| {
        let namespace = required_string(args, 0, "__memory_forget", "namespace")?;
        let predicate = args.get(1).cloned().ok_or_else(|| {
            VmError::Runtime("__memory_forget: `predicate` argument is required".to_string())
        })?;
        let options = args.get(2).and_then(VmValue::as_dict);
        let root = memory_root(options);
        let active = active_records(&root, &namespace)?;
        let predicate_json = crate::llm::vm_value_to_json(&predicate);
        let forgotten_ids = active
            .into_iter()
            .filter_map(|(_, record)| {
                predicate_matches_record(&predicate, &record).then_some(record.id)
            })
            .collect::<Vec<_>>();
        let event = ForgetEvent {
            id: option_string(options, "id").unwrap_or_else(|| uuid::Uuid::now_v7().to_string()),
            namespace: namespace.clone(),
            predicate: predicate_json,
            forgotten_ids: forgotten_ids.clone(),
            forgotten_at: option_string(options, "now").unwrap_or_else(now_rfc3339),
        };
        append_event(&root, &namespace, &MemoryEvent::Forget(event.clone()))?;
        Ok(forget_result_to_vm(&event))
    });
}

fn required_string(
    args: &[VmValue],
    idx: usize,
    fn_name: &str,
    arg_name: &str,
) -> Result<String, VmError> {
    let value = args
        .get(idx)
        .map(VmValue::display)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            VmError::Runtime(format!(
                "{fn_name}: `{arg_name}` must be a non-empty string"
            ))
        })?;
    Ok(value)
}

fn optional_usize(value: Option<&VmValue>) -> Option<usize> {
    match value {
        Some(VmValue::Int(raw)) if *raw > 0 => Some(*raw as usize),
        Some(VmValue::Float(raw)) if *raw > 0.0 => Some(*raw as usize),
        _ => None,
    }
}

fn option_string(options: Option<&BTreeMap<String, VmValue>>, key: &str) -> Option<String> {
    options
        .and_then(|opts| opts.get(key))
        .map(VmValue::display)
        .filter(|value| !value.trim().is_empty())
}

fn memory_root(options: Option<&BTreeMap<String, VmValue>>) -> PathBuf {
    option_string(options, "root")
        .or_else(|| std::env::var("HARN_MEMORY_ROOT").ok())
        .map(|root| crate::stdlib::process::resolve_source_relative_path(&root))
        .unwrap_or_else(|| crate::stdlib::process::runtime_root_base().join(".harn/memory"))
}

fn parse_tags(value: Option<&VmValue>, fn_name: &str) -> Result<Vec<String>, VmError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    match value {
        VmValue::Nil => Ok(Vec::new()),
        VmValue::String(tag) => Ok(vec![tag.to_string()]),
        VmValue::List(items) => {
            let mut tags = items
                .iter()
                .map(VmValue::display)
                .filter(|tag| !tag.trim().is_empty())
                .collect::<Vec<_>>();
            tags.sort();
            tags.dedup();
            Ok(tags)
        }
        other => Err(VmError::Runtime(format!(
            "{fn_name}: `tags` must be a string, list, or nil, got {}",
            other.type_name()
        ))),
    }
}

fn namespace_dir(root: &Path, namespace: &str) -> Result<PathBuf, VmError> {
    Ok(root.join(normalize_relative_component(namespace, "memory namespace")?))
}

fn event_log_path(root: &Path, namespace: &str) -> Result<PathBuf, VmError> {
    Ok(namespace_dir(root, namespace)?.join(EVENT_LOG_FILE))
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

fn append_event(root: &Path, namespace: &str, event: &MemoryEvent) -> Result<(), VmError> {
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

fn active_records(root: &Path, namespace: &str) -> Result<Vec<(usize, MemoryRecord)>, VmError> {
    let events = read_events(root, namespace)?;
    let mut records = Vec::new();
    let mut forgotten = BTreeSet::new();
    for event in &events {
        if let MemoryEvent::Forget(event) = event {
            forgotten.extend(event.forgotten_ids.iter().cloned());
        }
    }
    for (idx, event) in events.into_iter().enumerate() {
        if let MemoryEvent::Store(record) = event {
            if !forgotten.contains(&record.id) {
                records.push((idx, record));
            }
        }
    }
    Ok(records)
}

fn score_records(records: Vec<(usize, MemoryRecord)>, query: &str) -> Vec<ScoredRecord> {
    let query_terms = tokenize(query);
    if query_terms.is_empty() {
        let mut newest = records
            .into_iter()
            .map(|(sequence, record)| ScoredRecord {
                record,
                score: 0.0,
                sequence,
            })
            .collect::<Vec<_>>();
        newest.sort_by(newest_first);
        return newest;
    }

    let docs = records
        .iter()
        .map(|(_, record)| tokenize(&searchable_text(record)))
        .collect::<Vec<_>>();
    let total_docs = docs.len().max(1) as f64;
    let avg_len = docs.iter().map(Vec::len).sum::<usize>().max(1) as f64 / total_docs;
    let mut doc_freq = HashMap::<String, usize>::new();
    for doc in &docs {
        let unique = doc.iter().cloned().collect::<BTreeSet<_>>();
        for term in unique {
            *doc_freq.entry(term).or_insert(0) += 1;
        }
    }

    let mut scored = records
        .into_iter()
        .zip(docs)
        .filter_map(|((sequence, record), doc)| {
            let score = bm25_score(
                &query_terms,
                &doc,
                &doc_freq,
                total_docs,
                docs_len_f64(&doc),
                avg_len,
            ) + exact_field_boost(&query_terms, &record);
            (score > 0.0).then_some(ScoredRecord {
                record,
                score,
                sequence,
            })
        })
        .collect::<Vec<_>>();
    scored.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| newest_first(left, right))
    });
    scored
}

fn docs_len_f64(doc: &[String]) -> f64 {
    doc.len().max(1) as f64
}

fn bm25_score(
    query_terms: &[String],
    doc: &[String],
    doc_freq: &HashMap<String, usize>,
    total_docs: f64,
    doc_len: f64,
    avg_len: f64,
) -> f64 {
    if doc.is_empty() {
        return 0.0;
    }
    let mut tf = HashMap::<&str, usize>::new();
    for term in doc {
        *tf.entry(term.as_str()).or_insert(0) += 1;
    }
    let k1 = 1.2;
    let b = 0.75;
    query_terms
        .iter()
        .map(|term| {
            let freq = tf.get(term.as_str()).copied().unwrap_or(0) as f64;
            if freq == 0.0 {
                return 0.0;
            }
            let df = doc_freq.get(term).copied().unwrap_or(0) as f64;
            let idf = ((total_docs - df + 0.5) / (df + 0.5) + 1.0).ln();
            idf * (freq * (k1 + 1.0)) / (freq + k1 * (1.0 - b + b * doc_len / avg_len))
        })
        .sum()
}

fn exact_field_boost(query_terms: &[String], record: &MemoryRecord) -> f64 {
    let key = tokenize(&record.key).into_iter().collect::<BTreeSet<_>>();
    let tags = record
        .tags
        .iter()
        .flat_map(|tag| tokenize(tag))
        .collect::<BTreeSet<_>>();
    query_terms.iter().fold(0.0, |score, term| {
        score
            + if key.contains(term) { 0.4 } else { 0.0 }
            + if tags.contains(term) { 0.25 } else { 0.0 }
    })
}

fn searchable_text(record: &MemoryRecord) -> String {
    format!(
        "{}\n{}\n{}\n{}",
        record.key,
        record.text,
        record.tags.join(" "),
        record.value
    )
}

fn tokenize(input: &str) -> Vec<String> {
    input
        .split(|ch: char| !ch.is_alphanumeric())
        .filter_map(|term| {
            let term = term.trim().to_ascii_lowercase();
            (term.len() > 1).then_some(term)
        })
        .collect()
}

fn newest_first(left: &ScoredRecord, right: &ScoredRecord) -> Ordering {
    right
        .record
        .stored_at
        .cmp(&left.record.stored_at)
        .then_with(|| right.sequence.cmp(&left.sequence))
        .then_with(|| left.record.id.cmp(&right.record.id))
}

fn value_to_search_text(value: &VmValue) -> String {
    match value {
        VmValue::String(text) => text.to_string(),
        other => crate::llm::vm_value_to_json(other).to_string(),
    }
}

fn select_summary_records(
    records: Vec<(usize, MemoryRecord)>,
    window: Option<&VmValue>,
) -> Result<Vec<MemoryRecord>, VmError> {
    let (limit, query, tags) = parse_summary_window(window)?;
    let mut selected = if let Some(query) = query {
        score_records(records, &query)
            .into_iter()
            .map(|item| item.record)
            .collect::<Vec<_>>()
    } else {
        records
            .into_iter()
            .rev()
            .map(|(_, record)| record)
            .collect::<Vec<_>>()
    };
    if !tags.is_empty() {
        selected.retain(|record| tags.iter().any(|tag| record.tags.contains(tag)));
    }
    selected.truncate(limit);
    selected.sort_by(|left, right| {
        left.stored_at
            .cmp(&right.stored_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(selected)
}

fn parse_summary_window(
    window: Option<&VmValue>,
) -> Result<(usize, Option<String>, Vec<String>), VmError> {
    match window {
        None | Some(VmValue::Nil) => Ok((DEFAULT_SUMMARY_LIMIT, None, Vec::new())),
        Some(VmValue::Int(limit)) if *limit > 0 => {
            Ok(((*limit as usize).min(MAX_SUMMARY_LIMIT), None, Vec::new()))
        }
        Some(VmValue::Dict(dict)) => {
            let limit = optional_usize(dict.get("limit"))
                .unwrap_or(DEFAULT_SUMMARY_LIMIT)
                .clamp(1, MAX_SUMMARY_LIMIT);
            let query = dict
                .get("query")
                .map(VmValue::display)
                .filter(|query| !query.trim().is_empty());
            let tags = parse_tags(
                dict.get("tags").or_else(|| dict.get("tag")),
                "memory_summarize",
            )?;
            Ok((limit, query, tags))
        }
        Some(other) => Err(VmError::Runtime(format!(
            "__memory_summarize: `window` must be nil, int, or dict, got {}",
            other.type_name()
        ))),
    }
}

fn predicate_matches_record(predicate: &VmValue, record: &MemoryRecord) -> bool {
    match predicate {
        VmValue::String(raw) => {
            if raw.trim().is_empty() {
                return false;
            }
            let needle = raw.to_ascii_lowercase();
            searchable_text(record)
                .to_ascii_lowercase()
                .contains(&needle)
        }
        VmValue::Dict(dict) => {
            let mut matched_any = false;
            if let Some(value) = dict.get("id") {
                matched_any = true;
                if !value_matches_any(value, &record.id) {
                    return false;
                }
            }
            if let Some(value) = dict.get("key") {
                matched_any = true;
                if !value_matches_any(value, &record.key) {
                    return false;
                }
            }
            if let Some(value) = dict.get("tag").or_else(|| dict.get("tags")) {
                matched_any = true;
                let wanted = values_as_strings(value);
                if wanted.is_empty() || !wanted.iter().any(|tag| record.tags.contains(tag)) {
                    return false;
                }
            }
            if let Some(value) = dict.get("query") {
                matched_any = true;
                let query_terms = tokenize(&value.display());
                let text_terms = tokenize(&searchable_text(record))
                    .into_iter()
                    .collect::<BTreeSet<_>>();
                if query_terms.is_empty()
                    || !query_terms.iter().any(|term| text_terms.contains(term))
                {
                    return false;
                }
            }
            matched_any
        }
        _ => false,
    }
}

fn value_matches_any(value: &VmValue, candidate: &str) -> bool {
    values_as_strings(value)
        .iter()
        .any(|value| value == candidate)
}

fn values_as_strings(value: &VmValue) -> Vec<String> {
    match value {
        VmValue::List(items) => items
            .iter()
            .map(VmValue::display)
            .filter(|value| !value.trim().is_empty())
            .collect(),
        VmValue::Nil => Vec::new(),
        other => {
            let value = other.display();
            if value.trim().is_empty() {
                Vec::new()
            } else {
                vec![value]
            }
        }
    }
}

fn memory_record_to_vm(record: &MemoryRecord, score: Option<f64>) -> VmValue {
    let mut map = BTreeMap::new();
    map.insert("_type".to_string(), VmValue::String(Rc::from(MEMORY_TYPE)));
    map.insert(
        "id".to_string(),
        VmValue::String(Rc::from(record.id.as_str())),
    );
    map.insert(
        "namespace".to_string(),
        VmValue::String(Rc::from(record.namespace.as_str())),
    );
    map.insert(
        "key".to_string(),
        VmValue::String(Rc::from(record.key.as_str())),
    );
    map.insert(
        "value".to_string(),
        crate::stdlib::json_to_vm_value(&record.value),
    );
    map.insert(
        "text".to_string(),
        VmValue::String(Rc::from(record.text.as_str())),
    );
    map.insert(
        "tags".to_string(),
        VmValue::List(Rc::new(
            record
                .tags
                .iter()
                .map(|tag| VmValue::String(Rc::from(tag.as_str())))
                .collect(),
        )),
    );
    map.insert(
        "stored_at".to_string(),
        VmValue::String(Rc::from(record.stored_at.as_str())),
    );
    map.insert(
        "provenance".to_string(),
        record
            .provenance
            .as_ref()
            .map(crate::stdlib::json_to_vm_value)
            .unwrap_or(VmValue::Nil),
    );
    if let Some(score) = score {
        map.insert("score".to_string(), VmValue::Float(score));
    }
    VmValue::Dict(Rc::new(map))
}

fn summary_to_vm(namespace: &str, records: Vec<MemoryRecord>) -> VmValue {
    let mut text = String::new();
    for record in &records {
        let line = format!(
            "- [{}] {}: {}\n",
            record.tags.join(","),
            record.key,
            first_line(&record.text)
        );
        if text.len() + line.len() > MAX_SUMMARY_CHARS {
            break;
        }
        text.push_str(&line);
    }
    let mut map = BTreeMap::new();
    map.insert(
        "_type".to_string(),
        VmValue::String(Rc::from("memory_summary")),
    );
    map.insert(
        "namespace".to_string(),
        VmValue::String(Rc::from(namespace.to_string())),
    );
    map.insert("count".to_string(), VmValue::Int(records.len() as i64));
    map.insert("text".to_string(), VmValue::String(Rc::from(text)));
    map.insert(
        "records".to_string(),
        VmValue::List(Rc::new(
            records
                .iter()
                .map(|record| memory_record_to_vm(record, None))
                .collect(),
        )),
    );
    VmValue::Dict(Rc::new(map))
}

fn forget_result_to_vm(event: &ForgetEvent) -> VmValue {
    let mut map = BTreeMap::new();
    map.insert(
        "_type".to_string(),
        VmValue::String(Rc::from("memory_forget")),
    );
    map.insert(
        "id".to_string(),
        VmValue::String(Rc::from(event.id.as_str())),
    );
    map.insert(
        "namespace".to_string(),
        VmValue::String(Rc::from(event.namespace.as_str())),
    );
    map.insert(
        "forgotten".to_string(),
        VmValue::Int(event.forgotten_ids.len() as i64),
    );
    map.insert(
        "forgotten_ids".to_string(),
        VmValue::List(Rc::new(
            event
                .forgotten_ids
                .iter()
                .map(|id| VmValue::String(Rc::from(id.as_str())))
                .collect(),
        )),
    );
    map.insert(
        "forgotten_at".to_string(),
        VmValue::String(Rc::from(event.forgotten_at.as_str())),
    );
    VmValue::Dict(Rc::new(map))
}

fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or("").trim().to_string()
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("harn-memory-test-{name}-{}", uuid::Uuid::now_v7()));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    #[test]
    fn recall_scores_matching_records_and_forget_tombstones_them() {
        let root = temp_root("recall");
        let namespace = "agent/customer";
        let first = MemoryRecord {
            id: "mem-1".to_string(),
            namespace: namespace.to_string(),
            key: "alice".to_string(),
            value: serde_json::json!({"text": "Alice prefers Rust examples"}),
            text: "Alice prefers Rust examples".to_string(),
            tags: vec!["profile".to_string()],
            stored_at: "2026-04-29T00:00:00Z".to_string(),
            provenance: None,
        };
        let second = MemoryRecord {
            id: "mem-2".to_string(),
            namespace: namespace.to_string(),
            key: "bob".to_string(),
            value: serde_json::json!("Bob likes TypeScript"),
            text: "Bob likes TypeScript".to_string(),
            tags: vec!["profile".to_string()],
            stored_at: "2026-04-29T00:00:01Z".to_string(),
            provenance: None,
        };
        append_event(&root, namespace, &MemoryEvent::Store(first)).unwrap();
        append_event(&root, namespace, &MemoryEvent::Store(second)).unwrap();

        let recalled = score_records(active_records(&root, namespace).unwrap(), "rust profile");
        assert_eq!(recalled.first().unwrap().record.id, "mem-1");
        assert!(recalled.first().unwrap().score > 0.0);

        let forget = ForgetEvent {
            id: "forget-1".to_string(),
            namespace: namespace.to_string(),
            predicate: serde_json::json!({"tag": "profile"}),
            forgotten_ids: recalled.iter().map(|item| item.record.id.clone()).collect(),
            forgotten_at: "2026-04-29T00:00:02Z".to_string(),
        };
        append_event(&root, namespace, &MemoryEvent::Forget(forget)).unwrap();
        assert!(active_records(&root, namespace).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn namespace_rejects_parent_escape() {
        let error = event_log_path(Path::new("/tmp/memory"), "../escape")
            .expect_err("namespace escape should fail");
        assert!(error.to_string().contains("escape"));
    }
}
