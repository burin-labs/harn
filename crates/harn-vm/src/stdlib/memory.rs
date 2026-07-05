//! `std/memory` — append-only durable memory with optional host-backed
//! vector recall.
//!
//! The default backend remains deterministic BM25. Callers can opt in to a
//! vector or hybrid backend per namespace via [`memory_open`]; once opened,
//! [`memory_recall`] can score with cosine similarity over embeddings the
//! host supplies through the `memory.embed` capability. Embeddings are
//! cached on disk per `(model_hint, content_hash)` so replays from the same
//! event log are deterministic.
//!
//! Trust boundary: Harn never bundles an embedding model; the host is
//! responsible for the model choice, rate limiting, and cost accounting.
//! See `docs/src/host-boundary.md` and `docs/src/memory.md`.

use crate::value::VmDictExt;
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};

use crate::stdlib::host::dispatch_host_operation;
use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

const MEMORY_TYPE: &str = "memory_record";
const EVENT_LOG_FILE: &str = "events.jsonl";
const VECTOR_CACHE_DIR: &str = "vectors";
const DEFAULT_RECALL_LIMIT: usize = 5;
const DEFAULT_SUMMARY_LIMIT: usize = 20;
const MAX_RECALL_LIMIT: usize = 100;
const MAX_SUMMARY_LIMIT: usize = 200;
const MAX_SUMMARY_CHARS: usize = 4000;
const DEFAULT_EMBED_MODEL_HINT: &str = "default";
const DEFAULT_HYBRID_BM25_WEIGHT: f64 = 0.5;
const DEFAULT_HYBRID_COSINE_WEIGHT: f64 = 0.5;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum MemoryBackend {
    #[default]
    Bm25,
    Vector,
    Hybrid,
}

impl MemoryBackend {
    fn parse(value: &str, fn_name: &str) -> Result<Self, VmError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "" | "bm25" | "lexical" => Ok(Self::Bm25),
            "vector" | "semantic" => Ok(Self::Vector),
            "hybrid" => Ok(Self::Hybrid),
            other => Err(VmError::Runtime(format!(
                "{fn_name}: unknown backend `{other}` (expected bm25, vector, or hybrid)"
            ))),
        }
    }

    fn default_recall_mode(self) -> RecallMode {
        match self {
            Self::Bm25 => RecallMode::Lexical,
            Self::Vector => RecallMode::Semantic,
            Self::Hybrid => RecallMode::Hybrid,
        }
    }

    fn uses_embeddings(self) -> bool {
        matches!(self, Self::Vector | Self::Hybrid)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RecallMode {
    Lexical,
    Semantic,
    Hybrid,
}

impl RecallMode {
    fn parse(value: &str, fn_name: &str) -> Result<Self, VmError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "lexical" | "bm25" => Ok(Self::Lexical),
            "semantic" | "vector" => Ok(Self::Semantic),
            "hybrid" => Ok(Self::Hybrid),
            other => Err(VmError::Runtime(format!(
                "{fn_name}: unknown recall mode `{other}` (expected lexical, semantic, or hybrid)"
            ))),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum MemoryEvent {
    Store(MemoryRecord),
    Forget(ForgetEvent),
    Open(OpenEvent),
    Update(UpdateEvent),
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
    // Additive record metadata. All three skip serialization when unset, so
    // records written without them stay byte-identical to pre-existing logs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scope: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    flags: BTreeMap<String, bool>,
}

/// An in-place, append-only mutation to a stored record identified by `id`.
/// Only the fields present in the patch are overlaid at projection time; the
/// append-only log is never edited. `value` also refreshes the derived search
/// `text`. `flags` are merged key-by-key (set `false` to disable a flag).
#[derive(Clone, Debug, Deserialize, Serialize)]
struct UpdateEvent {
    id: String,
    namespace: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    value: Option<JsonValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tags: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scope: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    flags: BTreeMap<String, bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provenance: Option<JsonValue>,
    updated_at: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ForgetEvent {
    id: String,
    namespace: String,
    predicate: JsonValue,
    forgotten_ids: Vec<String>,
    forgotten_at: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct OpenEvent {
    id: String,
    namespace: String,
    backend: MemoryBackend,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    embed_model_hint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    embed_dim: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bm25_weight: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cosine_weight: Option<f64>,
    opened_at: String,
}

#[derive(Clone, Debug, Default)]
struct NamespaceConfig {
    backend: MemoryBackend,
    embed_model_hint: Option<String>,
    embed_dim: Option<usize>,
    bm25_weight: Option<f64>,
    cosine_weight: Option<f64>,
}

impl NamespaceConfig {
    fn model_hint(&self) -> &str {
        self.embed_model_hint
            .as_deref()
            .filter(|hint| !hint.is_empty())
            .unwrap_or(DEFAULT_EMBED_MODEL_HINT)
    }

    fn hybrid_weights(&self) -> (f64, f64) {
        (
            self.bm25_weight.unwrap_or(DEFAULT_HYBRID_BM25_WEIGHT),
            self.cosine_weight.unwrap_or(DEFAULT_HYBRID_COSINE_WEIGHT),
        )
    }
}

#[derive(Clone, Debug)]
struct ScoredRecord {
    record: MemoryRecord,
    score: f64,
    sequence: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct CachedEmbedding {
    model: String,
    dim: usize,
    vector: Vec<f64>,
}

pub(crate) fn register_memory_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    &MEMORY_STORE_IMPL_DEF,
    &MEMORY_RECALL_IMPL_DEF,
    &MEMORY_OPEN_IMPL_DEF,
    &MEMORY_SUMMARIZE_IMPL_DEF,
    &MEMORY_FORGET_IMPL_DEF,
    &MEMORY_UPDATE_IMPL_DEF,
    &MEMORY_LIST_IMPL_DEF,
];

#[harn_builtin(
    sig = "__memory_store(namespace: string, key: string, value: any, tags?: any, options?: dict) -> dict",
    kind = "async",
    category = "memory"
)]
async fn memory_store_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let namespace = required_string(&args, 0, "__memory_store", "namespace")?;
    let key = required_string(&args, 1, "__memory_store", "key")?;
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
        status: option_string(options, "status"),
        scope: option_string(options, "scope"),
        flags: parse_flags(options.and_then(|opts| opts.get("flags"))),
    };
    append_event(&root, &namespace, &MemoryEvent::Store(record.clone()))?;

    let config = read_namespace_config(&root, &namespace)?;
    let want_embed = option_bool(options, "embed").unwrap_or(false)
        || (config.backend.uses_embeddings() && option_bool(options, "skip_embed") != Some(true));
    if want_embed {
        let model_hint = option_string(options, "embed_model_hint")
            .unwrap_or_else(|| config.model_hint().to_string());
        let _ = ensure_embedding(&root, &namespace, &searchable_text(&record), &model_hint).await?;
    }

    Ok(memory_record_to_vm(&record, None))
}

#[harn_builtin(
    sig = "__memory_recall(namespace: string, query: string, limit?: int, options?: dict) -> list",
    kind = "async",
    category = "memory"
)]
async fn memory_recall_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let namespace = required_string(&args, 0, "__memory_recall", "namespace")?;
    let query = required_string(&args, 1, "__memory_recall", "query")?;
    let limit = optional_usize(args.get(2))
        .unwrap_or(DEFAULT_RECALL_LIMIT)
        .clamp(1, MAX_RECALL_LIMIT);
    let options = args.get(3).and_then(VmValue::as_dict);
    let root = memory_root(options);
    let config = read_namespace_config(&root, &namespace)?;
    let mode = if let Some(raw) = option_string(options, "mode") {
        RecallMode::parse(&raw, "__memory_recall")?
    } else {
        config.backend.default_recall_mode()
    };
    let model_hint = option_string(options, "embed_model_hint")
        .unwrap_or_else(|| config.model_hint().to_string());

    let records = active_records(&root, &namespace)?;
    let scored = score_records_async(
        records,
        &query,
        mode,
        &root,
        &namespace,
        &model_hint,
        &config,
    )
    .await?;
    Ok(VmValue::List(std::sync::Arc::new(
        scored
            .into_iter()
            .take(limit)
            .map(|item| memory_record_to_vm(&item.record, Some(item.score)))
            .collect(),
    )))
}

#[harn_builtin(
    sig = "__memory_open(namespace: string, options?: dict) -> dict",
    kind = "async",
    category = "memory"
)]
async fn memory_open_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let namespace = required_string(&args, 0, "__memory_open", "namespace")?;
    let options = args.get(1).and_then(VmValue::as_dict);
    let backend = match option_string(options, "backend") {
        Some(raw) => MemoryBackend::parse(&raw, "__memory_open")?,
        None => MemoryBackend::Bm25,
    };
    let embed_model_hint =
        option_string(options, "embed_model_hint").or_else(|| option_string(options, "model_hint"));
    let embed_dim = options
        .and_then(|opts| opts.get("embed_dim"))
        .and_then(coerce_usize);
    let bm25_weight = options
        .and_then(|opts| opts.get("bm25_weight"))
        .and_then(coerce_finite_f64);
    let cosine_weight = options
        .and_then(|opts| opts.get("cosine_weight"))
        .and_then(coerce_finite_f64);
    if backend == MemoryBackend::Hybrid {
        for (label, value) in [
            ("bm25_weight", bm25_weight),
            ("cosine_weight", cosine_weight),
        ] {
            if let Some(weight) = value {
                if weight < 0.0 {
                    return Err(VmError::Runtime(format!(
                        "__memory_open: `{label}` must be non-negative"
                    )));
                }
            }
        }
    }
    let event = OpenEvent {
        id: option_string(options, "id").unwrap_or_else(|| uuid::Uuid::now_v7().to_string()),
        namespace: namespace.clone(),
        backend,
        embed_model_hint,
        embed_dim,
        bm25_weight,
        cosine_weight,
        opened_at: option_string(options, "now").unwrap_or_else(now_rfc3339),
    };
    let root = memory_root(options);
    append_event(&root, &namespace, &MemoryEvent::Open(event.clone()))?;
    Ok(memory_open_to_vm(&event))
}

#[harn_builtin(
    sig = "__memory_summarize(namespace: string, window?: any, options?: dict) -> dict",
    category = "memory"
)]
fn memory_summarize_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
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
}

#[harn_builtin(
    sig = "__memory_forget(namespace: string, predicate: any, options?: dict) -> dict",
    category = "memory"
)]
fn memory_forget_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
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
        forgotten_ids,
        forgotten_at: option_string(options, "now").unwrap_or_else(now_rfc3339),
    };
    append_event(&root, &namespace, &MemoryEvent::Forget(event.clone()))?;
    Ok(forget_result_to_vm(&event))
}

#[harn_builtin(
    sig = "__memory_update(namespace: string, id: string, patch: dict, options?: dict) -> dict",
    category = "memory"
)]
fn memory_update_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let namespace = required_string(args, 0, "__memory_update", "namespace")?;
    let id = required_string(args, 1, "__memory_update", "id")?;
    let patch = args.get(2).and_then(VmValue::as_dict);
    let options = args.get(3).and_then(VmValue::as_dict);
    let root = memory_root(options);

    // Ignore a patch that targets an unknown/forgotten id: nothing to overlay.
    let exists = active_records(&root, &namespace)?
        .iter()
        .any(|(_, record)| record.id == id);
    if !exists {
        return Ok(VmValue::Nil);
    }

    let value = patch.and_then(|p| p.get("value")).cloned();
    let text = value.as_ref().map(value_to_search_text);
    let event = UpdateEvent {
        id: id.clone(),
        namespace: namespace.clone(),
        value: value.as_ref().map(crate::llm::vm_value_to_json),
        text,
        tags: match patch.and_then(|p| p.get("tags")) {
            Some(tags) => Some(parse_tags(Some(tags), "__memory_update")?),
            None => None,
        },
        status: patch
            .and_then(|p| p.get("status"))
            .map(VmValue::display)
            .filter(|value| !value.trim().is_empty()),
        scope: patch
            .and_then(|p| p.get("scope"))
            .map(VmValue::display)
            .filter(|value| !value.trim().is_empty()),
        flags: parse_flags(patch.and_then(|p| p.get("flags"))),
        provenance: patch
            .and_then(|p| p.get("provenance"))
            .map(crate::llm::vm_value_to_json),
        updated_at: option_string(options, "now").unwrap_or_else(now_rfc3339),
    };
    append_event(&root, &namespace, &MemoryEvent::Update(event))?;

    Ok(active_records(&root, &namespace)?
        .into_iter()
        .find(|(_, record)| record.id == id)
        .map(|(_, record)| memory_record_to_vm(&record, None))
        .unwrap_or(VmValue::Nil))
}

#[harn_builtin(
    sig = "__memory_list(namespace: string, options?: dict) -> list",
    category = "memory"
)]
fn memory_list_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let namespace = required_string(args, 0, "__memory_list", "namespace")?;
    let options = args.get(1).and_then(VmValue::as_dict);
    let root = memory_root(options);

    let status_filter = option_string(options, "status");
    let scope_filter = option_string(options, "scope");
    let tag_filter = option_string(options, "tag");
    let flag_filter = option_string(options, "flag");
    let limit = options
        .and_then(|opts| opts.get("limit"))
        .and_then(coerce_usize);

    let mut records = active_records(&root, &namespace)?;
    // Enumerate newest-first (by stored_at, then log order) — a stable, full
    // listing distinct from the query-ranked `memory_recall`.
    records.sort_by(|left, right| {
        right
            .1
            .stored_at
            .cmp(&left.1.stored_at)
            .then_with(|| right.0.cmp(&left.0))
    });

    let filtered = records
        .into_iter()
        .filter(|(_, record)| match &status_filter {
            Some(status) => record.status.as_deref() == Some(status.as_str()),
            None => true,
        })
        .filter(|(_, record)| match &scope_filter {
            Some(scope) => record.scope.as_deref() == Some(scope.as_str()),
            None => true,
        })
        .filter(|(_, record)| match &tag_filter {
            Some(tag) => record.tags.iter().any(|candidate| candidate == tag),
            None => true,
        })
        .filter(|(_, record)| match &flag_filter {
            Some(flag) => record.flags.get(flag).copied().unwrap_or(false),
            None => true,
        })
        .map(|(_, record)| memory_record_to_vm(&record, None));

    let items: Vec<VmValue> = match limit {
        Some(limit) => filtered.take(limit).collect(),
        None => filtered.collect(),
    };
    Ok(VmValue::List(std::sync::Arc::new(items)))
}

/// Parse an `options.flags` value into a `{name: bool}` map. Accepts a dict of
/// string→bool, or a list of flag names (each treated as `true`). Anything
/// else yields an empty map.
fn parse_flags(value: Option<&VmValue>) -> BTreeMap<String, bool> {
    let mut flags = BTreeMap::new();
    match value {
        Some(VmValue::Dict(map)) => {
            for (key, val) in map.iter() {
                if let VmValue::Bool(flag) = val {
                    flags.insert(key.to_string(), *flag);
                }
            }
        }
        Some(VmValue::List(items)) => {
            for item in items.iter() {
                let name = item.display();
                if !name.trim().is_empty() {
                    flags.insert(name, true);
                }
            }
        }
        _ => {}
    }
    flags
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

fn coerce_usize(value: &VmValue) -> Option<usize> {
    match value {
        VmValue::Int(raw) if *raw >= 0 => Some(*raw as usize),
        VmValue::Float(raw) if raw.is_finite() && *raw >= 0.0 => Some(*raw as usize),
        _ => None,
    }
}

fn coerce_finite_f64(value: &VmValue) -> Option<f64> {
    match value {
        VmValue::Int(raw) => Some(*raw as f64),
        VmValue::Float(raw) if raw.is_finite() => Some(*raw),
        _ => None,
    }
}

fn option_string(options: Option<&crate::value::DictMap>, key: &str) -> Option<String> {
    options
        .and_then(|opts| opts.get(key))
        .map(VmValue::display)
        .filter(|value| !value.trim().is_empty())
}

fn option_bool(options: Option<&crate::value::DictMap>, key: &str) -> Option<bool> {
    match options.and_then(|opts| opts.get(key))? {
        VmValue::Bool(value) => Some(*value),
        VmValue::Nil => None,
        _ => None,
    }
}

fn memory_root(options: Option<&crate::value::DictMap>) -> PathBuf {
    resolve_memory_root(option_string(options, "root").as_deref())
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

/// Synchronous, BM25-blended recall over a namespace, returning the stored
/// record `value` payloads for records whose schema is the canonical
/// `harn.fact.v1`. Records that BM25 cannot score against the query are
/// retained and ranked by recency, so callers always get up to `limit`
/// facts when any exist. Reminder providers call this from their sync
/// `evaluate` impls without straddling the runtime.
pub(crate) fn lexical_recall_fact_values(
    root: &Path,
    namespace: &str,
    query: &str,
    limit: usize,
) -> Result<Vec<JsonValue>, VmError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let records = active_records(root, namespace)?;
    if records.is_empty() {
        return Ok(Vec::new());
    }
    let fact_records: Vec<(usize, MemoryRecord)> = records
        .into_iter()
        .filter(|(_, record)| {
            record.value.get("schema").and_then(JsonValue::as_str) == Some("harn.fact.v1")
        })
        .collect();
    if fact_records.is_empty() {
        return Ok(Vec::new());
    }
    let scored: BTreeMap<String, f64> = score_bm25(fact_records.clone(), query)
        .into_iter()
        .map(|item| (item.record.id.clone(), item.score))
        .collect();
    let mut ranked: Vec<ScoredRecord> = fact_records
        .into_iter()
        .map(|(sequence, record)| ScoredRecord {
            score: scored.get(&record.id).copied().unwrap_or(0.0),
            sequence,
            record,
        })
        .collect();
    ranked.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| newest_first(left, right))
    });
    Ok(ranked
        .into_iter()
        .take(limit)
        .map(|item| item.record.value)
        .collect())
}

/// Resolve a memory root from an optional explicit path, falling back to
/// `HARN_MEMORY_ROOT` and finally the project-local `.harn/memory`. Shared
/// with reminder providers so they pick up the same default the Harn-side
/// `memory_store` / `memory_recall` builtins use.
pub(crate) fn resolve_memory_root(explicit: Option<&str>) -> PathBuf {
    explicit
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| std::env::var("HARN_MEMORY_ROOT").ok())
        .map(|root| crate::stdlib::process::resolve_source_relative_path(&root))
        .unwrap_or_else(|| crate::stdlib::process::runtime_root_base().join(".harn/memory"))
}

fn event_log_path(root: &Path, namespace: &str) -> Result<PathBuf, VmError> {
    Ok(namespace_dir(root, namespace)?.join(EVENT_LOG_FILE))
}

fn vector_cache_path(
    root: &Path,
    namespace: &str,
    model_hint: &str,
    content_hash: &str,
) -> Result<PathBuf, VmError> {
    let sanitized = sanitize_model_hint(model_hint);
    Ok(namespace_dir(root, namespace)?
        .join(VECTOR_CACHE_DIR)
        .join(sanitized)
        .join(format!("{content_hash}.json")))
}

fn sanitize_model_hint(hint: &str) -> String {
    let trimmed = hint.trim();
    if trimmed.is_empty() {
        return DEFAULT_EMBED_MODEL_HINT.to_string();
    }
    // Path traversal hardening: replace every character that is not
    // alphanumeric, dash, or underscore with `_`. Dots are excluded so that
    // a maliciously crafted hint like `../escape` cannot resolve a parent
    // directory once joined into the cache path.
    let sanitized: String = trimmed
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.is_empty() {
        DEFAULT_EMBED_MODEL_HINT.to_string()
    } else {
        sanitized
    }
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

/// Overlay an update's present fields onto a record. Absent patch fields leave
/// the record untouched; `flags` are merged key-by-key.
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

fn read_namespace_config(root: &Path, namespace: &str) -> Result<NamespaceConfig, VmError> {
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

#[allow(clippy::too_many_arguments)]
async fn score_records_async(
    records: Vec<(usize, MemoryRecord)>,
    query: &str,
    mode: RecallMode,
    root: &Path,
    namespace: &str,
    model_hint: &str,
    config: &NamespaceConfig,
) -> Result<Vec<ScoredRecord>, VmError> {
    if records.is_empty() {
        return Ok(Vec::new());
    }
    match mode {
        RecallMode::Lexical => Ok(score_bm25(records, query)),
        RecallMode::Semantic => {
            score_semantic(records, query, root, namespace, model_hint, config).await
        }
        RecallMode::Hybrid => {
            let bm25_by_id = score_bm25(records.clone(), query)
                .into_iter()
                .map(|item| (item.record.id.clone(), item.score))
                .collect::<HashMap<_, _>>();
            let cosine_by_id =
                score_semantic(records.clone(), query, root, namespace, model_hint, config)
                    .await?
                    .into_iter()
                    .map(|item| (item.record.id.clone(), item.score))
                    .collect::<HashMap<_, _>>();
            let (bm25_weight, cosine_weight) = config.hybrid_weights();
            let max_bm25 = bm25_by_id
                .values()
                .copied()
                .fold(0.0_f64, f64::max)
                .max(1.0);
            // Score every active record so that records strong on only one
            // signal (e.g. high cosine, zero BM25) still surface — that is
            // the whole point of running hybrid over lexical.
            let mut blended = Vec::with_capacity(records.len());
            for (sequence, record) in records {
                let bm25_raw = bm25_by_id.get(&record.id).copied().unwrap_or(0.0);
                let cosine_raw = cosine_by_id.get(&record.id).copied().unwrap_or(0.0);
                if bm25_raw == 0.0 && cosine_raw <= 0.0 {
                    continue;
                }
                let score = bm25_weight * (bm25_raw / max_bm25) + cosine_weight * cosine_raw;
                blended.push(ScoredRecord {
                    record,
                    score,
                    sequence,
                });
            }
            blended.sort_by(|left, right| {
                right
                    .score
                    .partial_cmp(&left.score)
                    .unwrap_or(Ordering::Equal)
                    .then_with(|| newest_first(left, right))
            });
            Ok(blended)
        }
    }
}

fn score_bm25(records: Vec<(usize, MemoryRecord)>, query: &str) -> Vec<ScoredRecord> {
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

async fn score_semantic(
    records: Vec<(usize, MemoryRecord)>,
    query: &str,
    root: &Path,
    namespace: &str,
    model_hint: &str,
    config: &NamespaceConfig,
) -> Result<Vec<ScoredRecord>, VmError> {
    let query_vector = ensure_embedding(root, namespace, query, model_hint).await?;
    if query_vector.is_empty() {
        return Err(VmError::Runtime(
            "memory: memory.embed returned an empty vector for the query".to_string(),
        ));
    }
    if let Some(expected) = config.embed_dim {
        if query_vector.len() != expected {
            return Err(VmError::Runtime(format!(
                "memory: memory.embed returned a {}-dim query vector but the namespace was opened with embed_dim={expected}",
                query_vector.len()
            )));
        }
    }
    let mut scored = Vec::with_capacity(records.len());
    for (sequence, record) in records {
        let record_vector =
            ensure_embedding(root, namespace, &searchable_text(&record), model_hint).await?;
        if record_vector.len() != query_vector.len() {
            return Err(VmError::Runtime(format!(
                "memory: embedding dimension mismatch for record {} (query={}, record={})",
                record.id,
                query_vector.len(),
                record_vector.len()
            )));
        }
        let score = cosine_similarity(&query_vector, &record_vector);
        if score > 0.0 {
            scored.push(ScoredRecord {
                record,
                score,
                sequence,
            });
        }
    }
    scored.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| newest_first(left, right))
    });
    Ok(scored)
}

async fn ensure_embedding(
    root: &Path,
    namespace: &str,
    text: &str,
    model_hint: &str,
) -> Result<Vec<f64>, VmError> {
    let hint = if model_hint.trim().is_empty() {
        DEFAULT_EMBED_MODEL_HINT
    } else {
        model_hint
    };
    let content_hash = sha256_hex(text);
    let path = vector_cache_path(root, namespace, hint, &content_hash)?;
    if let Some(cached) = read_cached_embedding(&path)? {
        return Ok(cached.vector);
    }
    let mut params = crate::value::DictMap::new();
    params.put_str("text", text);
    params.put_str("model_hint", hint);
    let result = dispatch_host_operation("memory", "embed", &params).await?;
    let cached = parse_embedding_response(result, hint)?;
    write_cached_embedding(&path, &cached)?;
    Ok(cached.vector)
}

fn read_cached_embedding(path: &Path) -> Result<Option<CachedEmbedding>, VmError> {
    match fs::read(path) {
        Ok(bytes) => {
            let cached: CachedEmbedding = serde_json::from_slice(&bytes).map_err(|error| {
                VmError::Runtime(format!(
                    "memory: failed to parse cached embedding {}: {error}",
                    path.display()
                ))
            })?;
            Ok(Some(cached))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(VmError::Runtime(format!(
            "memory: failed to read cached embedding {}: {error}",
            path.display()
        ))),
    }
}

fn write_cached_embedding(path: &Path, embedding: &CachedEmbedding) -> Result<(), VmError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            VmError::Runtime(format!(
                "memory: failed to create {}: {error}",
                parent.display()
            ))
        })?;
    }
    let bytes = serde_json::to_vec(embedding)
        .map_err(|error| VmError::Runtime(format!("memory: encode embedding error: {error}")))?;
    fs::write(path, bytes).map_err(|error| {
        VmError::Runtime(format!(
            "memory: failed to write {}: {error}",
            path.display()
        ))
    })?;
    Ok(())
}

fn parse_embedding_response(
    value: VmValue,
    fallback_model: &str,
) -> Result<CachedEmbedding, VmError> {
    let dict = value.as_dict().ok_or_else(|| {
        VmError::Runtime(
            "memory.embed: host must return a dict with {vector, model?, dim?}".to_string(),
        )
    })?;
    let raw_vector = dict.get("vector").ok_or_else(|| {
        VmError::Runtime("memory.embed: host response missing `vector` field".to_string())
    })?;
    let vector = match raw_vector {
        VmValue::List(items) => items
            .iter()
            .map(|item| match item {
                VmValue::Int(raw) => Ok(*raw as f64),
                VmValue::Float(raw) if raw.is_finite() => Ok(*raw),
                VmValue::Float(_) => Err(VmError::Runtime(
                    "memory.embed: vector entries must be finite numbers".to_string(),
                )),
                other => Err(VmError::Runtime(format!(
                    "memory.embed: vector entries must be numbers, got {}",
                    other.type_name()
                ))),
            })
            .collect::<Result<Vec<_>, _>>()?,
        other => {
            return Err(VmError::Runtime(format!(
                "memory.embed: vector must be a list, got {}",
                other.type_name()
            )))
        }
    };
    if vector.is_empty() {
        return Err(VmError::Runtime(
            "memory.embed: host returned an empty vector".to_string(),
        ));
    }
    let dim = match dict.get("dim").and_then(coerce_usize) {
        Some(declared) if declared != vector.len() => {
            return Err(VmError::Runtime(format!(
                "memory.embed: declared dim={declared} does not match vector length={}",
                vector.len()
            )))
        }
        Some(declared) => declared,
        None => vector.len(),
    };
    let model = dict
        .get("model")
        .map(VmValue::display)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| fallback_model.to_string());
    Ok(CachedEmbedding { model, dim, vector })
}

fn cosine_similarity(a: &[f64], b: &[f64]) -> f64 {
    debug_assert_eq!(a.len(), b.len());
    let mut dot = 0.0;
    let mut norm_a = 0.0;
    let mut norm_b = 0.0;
    for (left, right) in a.iter().zip(b.iter()) {
        dot += left * right;
        norm_a += left * left;
        norm_b += right * right;
    }
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a.sqrt() * norm_b.sqrt())
}

fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
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
            let idf = ((total_docs - df + 0.5) / (df + 0.5)).ln_1p();
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
        score_bm25(records, &query)
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
    let mut map = crate::value::DictMap::new();
    map.put_str("_type", MEMORY_TYPE);
    map.put_str("id", record.id.as_str());
    map.put_str("namespace", record.namespace.as_str());
    map.put_str("key", record.key.as_str());
    map.insert(
        crate::value::intern_key("value"),
        crate::stdlib::json_to_vm_value(&record.value),
    );
    map.put_str("text", record.text.as_str());
    map.insert(
        crate::value::intern_key("tags"),
        VmValue::List(std::sync::Arc::new(
            record
                .tags
                .iter()
                .map(|tag| VmValue::String(arcstr::ArcStr::from(tag.as_str())))
                .collect(),
        )),
    );
    map.put_str("stored_at", record.stored_at.as_str());
    map.insert(
        crate::value::intern_key("provenance"),
        record
            .provenance
            .as_ref()
            .map(crate::stdlib::json_to_vm_value)
            .unwrap_or(VmValue::Nil),
    );
    map.insert(
        crate::value::intern_key("status"),
        record
            .status
            .as_deref()
            .map(|status| VmValue::String(arcstr::ArcStr::from(status)))
            .unwrap_or(VmValue::Nil),
    );
    map.insert(
        crate::value::intern_key("scope"),
        record
            .scope
            .as_deref()
            .map(|scope| VmValue::String(arcstr::ArcStr::from(scope)))
            .unwrap_or(VmValue::Nil),
    );
    let mut flag_map = crate::value::DictMap::new();
    for (key, value) in &record.flags {
        flag_map.insert(crate::value::intern_key(key), VmValue::Bool(*value));
    }
    map.insert(crate::value::intern_key("flags"), VmValue::dict(flag_map));
    if let Some(score) = score {
        map.insert(crate::value::intern_key("score"), VmValue::Float(score));
    }
    VmValue::dict(map)
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
    let mut map = crate::value::DictMap::new();
    map.put_str("_type", "memory_summary");
    map.put_str("namespace", namespace);
    map.insert(
        crate::value::intern_key("count"),
        VmValue::Int(records.len() as i64),
    );
    map.put_str("text", text);
    map.insert(
        crate::value::intern_key("records"),
        VmValue::List(std::sync::Arc::new(
            records
                .iter()
                .map(|record| memory_record_to_vm(record, None))
                .collect(),
        )),
    );
    VmValue::dict(map)
}

fn forget_result_to_vm(event: &ForgetEvent) -> VmValue {
    let mut map = crate::value::DictMap::new();
    map.put_str("_type", "memory_forget");
    map.put_str("id", event.id.as_str());
    map.put_str("namespace", event.namespace.as_str());
    map.insert(
        crate::value::intern_key("forgotten"),
        VmValue::Int(event.forgotten_ids.len() as i64),
    );
    map.insert(
        crate::value::intern_key("forgotten_ids"),
        VmValue::List(std::sync::Arc::new(
            event
                .forgotten_ids
                .iter()
                .map(|id| VmValue::String(arcstr::ArcStr::from(id.as_str())))
                .collect(),
        )),
    );
    map.put_str("forgotten_at", event.forgotten_at.as_str());
    VmValue::dict(map)
}

fn memory_open_to_vm(event: &OpenEvent) -> VmValue {
    let mut map = crate::value::DictMap::new();
    map.put_str("_type", "memory_open");
    map.put_str("id", event.id.as_str());
    map.put_str("namespace", event.namespace.as_str());
    let backend = match event.backend {
        MemoryBackend::Bm25 => "bm25",
        MemoryBackend::Vector => "vector",
        MemoryBackend::Hybrid => "hybrid",
    };
    map.put_str("backend", backend);
    map.insert(
        crate::value::intern_key("embed_model_hint"),
        event
            .embed_model_hint
            .as_deref()
            .map(|hint| VmValue::String(arcstr::ArcStr::from(hint)))
            .unwrap_or(VmValue::Nil),
    );
    map.insert(
        crate::value::intern_key("embed_dim"),
        event
            .embed_dim
            .map(|dim| VmValue::Int(dim as i64))
            .unwrap_or(VmValue::Nil),
    );
    map.insert(
        crate::value::intern_key("bm25_weight"),
        event
            .bm25_weight
            .map(VmValue::Float)
            .unwrap_or(VmValue::Nil),
    );
    map.insert(
        crate::value::intern_key("cosine_weight"),
        event
            .cosine_weight
            .map(VmValue::Float)
            .unwrap_or(VmValue::Nil),
    );
    map.put_str("opened_at", event.opened_at.as_str());
    VmValue::dict(map)
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
    use crate::value::VmDictExt;

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
            status: None,
            scope: None,
            flags: BTreeMap::new(),
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
            status: None,
            scope: None,
            flags: BTreeMap::new(),
        };
        append_event(&root, namespace, &MemoryEvent::Store(first)).unwrap();
        append_event(&root, namespace, &MemoryEvent::Store(second)).unwrap();

        let recalled = score_bm25(active_records(&root, namespace).unwrap(), "rust profile");
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

    #[test]
    fn open_event_records_backend_config_and_overrides_replace_prior() {
        let root = temp_root("open");
        let namespace = "agent/cfg";
        append_event(
            &root,
            namespace,
            &MemoryEvent::Open(OpenEvent {
                id: "open-1".to_string(),
                namespace: namespace.to_string(),
                backend: MemoryBackend::Vector,
                embed_model_hint: Some("test-model".to_string()),
                embed_dim: Some(3),
                bm25_weight: None,
                cosine_weight: None,
                opened_at: "2026-05-01T00:00:00Z".to_string(),
            }),
        )
        .unwrap();
        append_event(
            &root,
            namespace,
            &MemoryEvent::Open(OpenEvent {
                id: "open-2".to_string(),
                namespace: namespace.to_string(),
                backend: MemoryBackend::Hybrid,
                embed_model_hint: Some("test-model".to_string()),
                embed_dim: Some(3),
                bm25_weight: Some(0.4),
                cosine_weight: Some(0.6),
                opened_at: "2026-05-02T00:00:00Z".to_string(),
            }),
        )
        .unwrap();
        let config = read_namespace_config(&root, namespace).unwrap();
        assert_eq!(config.backend, MemoryBackend::Hybrid);
        assert_eq!(config.bm25_weight, Some(0.4));
        assert_eq!(config.cosine_weight, Some(0.6));
        assert_eq!(config.embed_dim, Some(3));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn cached_embedding_round_trips() {
        let root = temp_root("cache");
        let namespace = "agent/cache";
        let hash = sha256_hex("hello world");
        let path = vector_cache_path(&root, namespace, "voyage-2", &hash).unwrap();
        let embedding = CachedEmbedding {
            model: "voyage-2".to_string(),
            dim: 3,
            vector: vec![0.1, 0.2, 0.3],
        };
        write_cached_embedding(&path, &embedding).unwrap();
        let restored = read_cached_embedding(&path).unwrap().unwrap();
        assert_eq!(restored.dim, 3);
        assert_eq!(restored.vector, vec![0.1, 0.2, 0.3]);
        assert!(path.components().any(|c| c.as_os_str() == VECTOR_CACHE_DIR));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn cosine_similarity_handles_orthogonal_and_parallel_vectors() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        let c = vec![2.0, 0.0, 0.0];
        assert!(cosine_similarity(&a, &b).abs() < 1e-9);
        assert!((cosine_similarity(&a, &c) - 1.0).abs() < 1e-9);
        let zero = vec![0.0, 0.0, 0.0];
        assert_eq!(cosine_similarity(&a, &zero), 0.0);
    }

    #[test]
    fn sanitize_model_hint_strips_path_separators() {
        assert_eq!(sanitize_model_hint(""), DEFAULT_EMBED_MODEL_HINT);
        assert_eq!(sanitize_model_hint("voyage-2"), "voyage-2");
        assert_eq!(sanitize_model_hint("../escape"), "___escape");
        assert_eq!(sanitize_model_hint("ns/model"), "ns_model");
    }

    #[test]
    fn parse_embedding_response_validates_dim_and_vector_types() {
        let mut dict = crate::value::DictMap::new();
        dict.insert(
            crate::value::intern_key("vector"),
            VmValue::List(std::sync::Arc::new(vec![
                VmValue::Float(0.1),
                VmValue::Float(0.2),
                VmValue::Int(1),
            ])),
        );
        dict.insert(crate::value::intern_key("dim"), VmValue::Int(3));
        dict.put_str("model", "test-model");
        let value = VmValue::dict(dict);
        let parsed = parse_embedding_response(value, "fallback").unwrap();
        assert_eq!(parsed.dim, 3);
        assert_eq!(parsed.model, "test-model");
        assert_eq!(parsed.vector, vec![0.1, 0.2, 1.0]);

        let mut bad = crate::value::DictMap::new();
        bad.insert(
            crate::value::intern_key("vector"),
            VmValue::List(std::sync::Arc::new(vec![VmValue::Float(0.1)])),
        );
        bad.insert(crate::value::intern_key("dim"), VmValue::Int(2));
        let err = parse_embedding_response(VmValue::dict(bad), "fallback")
            .expect_err("dim mismatch must error");
        assert!(err.to_string().contains("dim=2"));
    }

    fn record(id: &str, key: &str, at: &str) -> MemoryRecord {
        MemoryRecord {
            id: id.to_string(),
            namespace: "agent/mem".to_string(),
            key: key.to_string(),
            value: serde_json::json!({"text": key}),
            text: key.to_string(),
            tags: Vec::new(),
            stored_at: at.to_string(),
            provenance: None,
            status: None,
            scope: None,
            flags: BTreeMap::new(),
        }
    }

    #[test]
    fn store_without_new_fields_is_byte_identical() {
        // A record that sets none of status/scope/flags must serialize exactly
        // as it did before these fields existed (no new keys on the line).
        let line = serde_json::to_string(&MemoryEvent::Store(record(
            "m1",
            "alice",
            "2026-04-29T00:00:00Z",
        )))
        .unwrap();
        assert!(!line.contains("status"), "line: {line}");
        assert!(!line.contains("scope"), "line: {line}");
        assert!(!line.contains("flags"), "line: {line}");
    }

    #[test]
    fn update_overlays_fields_and_merges_flags() {
        let root = temp_root("update");
        let ns = "agent/mem";
        let mut rec = record("m1", "alice", "2026-04-29T00:00:00Z");
        rec.status = Some("pending".to_string());
        rec.flags.insert("auto_surface".to_string(), false);
        append_event(&root, ns, &MemoryEvent::Store(rec)).unwrap();

        let update = UpdateEvent {
            id: "m1".to_string(),
            namespace: ns.to_string(),
            value: None,
            text: None,
            tags: None,
            status: Some("accepted".to_string()),
            scope: Some("project".to_string()),
            flags: BTreeMap::from([("auto_surface".to_string(), true)]),
            provenance: None,
            updated_at: "2026-04-29T00:00:01Z".to_string(),
        };
        append_event(&root, ns, &MemoryEvent::Update(update)).unwrap();

        let records = active_records(&root, ns).unwrap();
        assert_eq!(records.len(), 1);
        let projected = &records[0].1;
        assert_eq!(projected.status.as_deref(), Some("accepted"));
        assert_eq!(projected.scope.as_deref(), Some("project"));
        assert_eq!(projected.flags.get("auto_surface"), Some(&true));
    }

    #[test]
    fn update_to_forgotten_or_unknown_id_is_ignored() {
        let root = temp_root("update-missing");
        let ns = "agent/mem";
        // No store for m9 → active_records still empty after the update.
        let update = UpdateEvent {
            id: "m9".to_string(),
            namespace: ns.to_string(),
            value: None,
            text: None,
            tags: None,
            status: Some("accepted".to_string()),
            scope: None,
            flags: BTreeMap::new(),
            provenance: None,
            updated_at: "2026-04-29T00:00:01Z".to_string(),
        };
        append_event(&root, ns, &MemoryEvent::Update(update)).unwrap();
        assert!(active_records(&root, ns).unwrap().is_empty());
    }

    #[test]
    fn parse_flags_accepts_dict_and_list() {
        let mut dict = crate::value::DictMap::new();
        dict.insert(crate::value::intern_key("a"), VmValue::Bool(true));
        dict.insert(crate::value::intern_key("b"), VmValue::Bool(false));
        let from_dict = parse_flags(Some(&VmValue::dict(dict)));
        assert_eq!(from_dict.get("a"), Some(&true));
        assert_eq!(from_dict.get("b"), Some(&false));

        let list = VmValue::List(std::sync::Arc::new(vec![VmValue::String(
            arcstr::ArcStr::from("auto_surface"),
        )]));
        let from_list = parse_flags(Some(&list));
        assert_eq!(from_list.get("auto_surface"), Some(&true));
    }
}
