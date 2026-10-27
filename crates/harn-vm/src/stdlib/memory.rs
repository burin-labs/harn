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
use harn_kernel::pure::sha256_hex;
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::stdlib::host::dispatch_host_operation;
use crate::stdlib::macros::harn_builtin;
use crate::value::{VmError, VmValue};

mod storage;
#[cfg(test)]
use storage::event_log_path;
use storage::{active_records, append_event, read_namespace_config, vector_cache_path};

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

harn_vm_core::stdlib_module_installer!(Host, pub(crate) fn register_memory_builtins);

/// Reserved cache namespace for standalone `__embed` calls. Keeps the
/// content-addressed embedding cache for ad-hoc embeds (e.g. the semantic
/// response cache) separate from per-memory-namespace record embeddings.
const EMBED_CACHE_NAMESPACE: &str = "__embed";

#[harn_builtin(
    exposure = "runtime_internal",
    sig = "__embed(text: string, options?: dict) -> dict",
    kind = "async",
    category = "memory"
)]
async fn embed_impl(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let text = required_string(&args, 0, "__embed", "text")?;
    let options = args.get(1).and_then(VmValue::as_dict);
    let root = memory_root(options);
    let model_hint = option_string(options, "embed_model_hint")
        .or_else(|| option_string(options, "model_hint"))
        .unwrap_or_else(|| DEFAULT_EMBED_MODEL_HINT.to_string());
    let embedding =
        embed_cached(Some(&ctx), &root, EMBED_CACHE_NAMESPACE, &text, &model_hint).await?;
    let mut map = crate::value::DictMap::new();
    map.put(
        "vector",
        VmValue::list(
            embedding
                .vector
                .iter()
                .map(|value| VmValue::Float(*value))
                .collect(),
        ),
    );
    map.put_str("model", embedding.model.as_str());
    map.put_int("dim", embedding.dim as i64);
    Ok(VmValue::dict(map))
}

#[harn_builtin(
    exposure = "runtime_internal",
    sig = "__memory_store(namespace: string, key: string, value: any, tags?: any, options?: dict) -> dict",
    kind = "async",
    category = "memory"
)]
async fn memory_store_impl(
    ctx: crate::vm::AsyncBuiltinCtx,
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
        let _ = ensure_embedding(
            Some(&ctx),
            &root,
            &namespace,
            &searchable_text(&record),
            &model_hint,
        )
        .await?;
    }

    Ok(memory_record_to_vm(&record, None))
}

#[harn_builtin(
    exposure = "runtime_internal",
    sig = "__memory_recall(namespace: string, query: string, limit?: int, options?: dict) -> list",
    kind = "async",
    category = "memory"
)]
async fn memory_recall_impl(
    ctx: crate::vm::AsyncBuiltinCtx,
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
        Some(&ctx),
        records,
        &query,
        mode,
        &root,
        &namespace,
        &model_hint,
        &config,
    )
    .await?;
    Ok(VmValue::list(
        scored
            .into_iter()
            .take(limit)
            .map(|item| memory_record_to_vm(&item.record, Some(item.score)))
            .collect(),
    ))
}

#[harn_builtin(
    exposure = "runtime_internal",
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
    exposure = "runtime_internal",
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
    exposure = "runtime_internal",
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
    exposure = "runtime_internal",
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
    exposure = "runtime_internal",
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
    Ok(VmValue::list(items))
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
        .or_else(|| std::env::var(crate::runtime_paths::HARN_MEMORY_ROOT_ENV).ok())
        .map(|root| crate::stdlib::process::resolve_source_relative_path(&root))
        .unwrap_or_else(|| {
            crate::stdlib::process::runtime_root_base()
                .join(".harn")
                .join(crate::runtime_paths::MEMORY_DIR)
        })
}

#[allow(clippy::too_many_arguments)]
async fn score_records_async(
    ctx: Option<&crate::vm::AsyncBuiltinCtx>,
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
            score_semantic(ctx, records, query, root, namespace, model_hint, config).await
        }
        RecallMode::Hybrid => {
            let bm25_by_id = score_bm25(records.clone(), query)
                .into_iter()
                .map(|item| (item.record.id.clone(), item.score))
                .collect::<HashMap<_, _>>();
            let cosine_by_id = score_semantic(
                ctx,
                records.clone(),
                query,
                root,
                namespace,
                model_hint,
                config,
            )
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
    ctx: Option<&crate::vm::AsyncBuiltinCtx>,
    records: Vec<(usize, MemoryRecord)>,
    query: &str,
    root: &Path,
    namespace: &str,
    model_hint: &str,
    config: &NamespaceConfig,
) -> Result<Vec<ScoredRecord>, VmError> {
    let query_vector = ensure_embedding(ctx, root, namespace, query, model_hint).await?;
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
            ensure_embedding(ctx, root, namespace, &searchable_text(&record), model_hint).await?;
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
    ctx: Option<&crate::vm::AsyncBuiltinCtx>,
    root: &Path,
    namespace: &str,
    text: &str,
    model_hint: &str,
) -> Result<Vec<f64>, VmError> {
    Ok(embed_cached(ctx, root, namespace, text, model_hint)
        .await?
        .vector)
}

/// Embed `text` via the `memory.embed` host capability, returning the full
/// `{vector, model, dim}` envelope. Reuses the per-`(model_hint, content_hash)`
/// on-disk cache so a repeated text embeds once and replays deterministically.
/// This is the shared substrate behind both `memory_recall`'s semantic scoring
/// and the standalone `__embed` builtin.
async fn embed_cached(
    ctx: Option<&crate::vm::AsyncBuiltinCtx>,
    root: &Path,
    namespace: &str,
    text: &str,
    model_hint: &str,
) -> Result<CachedEmbedding, VmError> {
    let hint = if model_hint.trim().is_empty() {
        DEFAULT_EMBED_MODEL_HINT
    } else {
        model_hint
    };
    let content_hash = sha256_hex(text.as_bytes());
    let path = vector_cache_path(root, namespace, hint, &content_hash)?;
    if let Some(cached) = read_cached_embedding(&path)? {
        return Ok(cached);
    }
    let mut params = crate::value::DictMap::new();
    params.put_str("text", text);
    params.put_str("model_hint", hint);
    params.put_str("root", root.to_string_lossy());
    let fixture_result = crate::harness::context_fixture(
        ctx,
        harn_builtin_meta::CapabilityId::Embed,
        "text_response",
        &[VmValue::dict(params.clone())],
    )?;
    let result = match fixture_result {
        Some(result) => result,
        None => dispatch_host_operation("memory", "embed", &params).await?,
    };
    let cached = parse_embedding_response(result, hint)?;
    write_cached_embedding(&path, &cached)?;
    Ok(cached)
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
    map.put("value", crate::stdlib::json_to_vm_value(&record.value));
    map.put_str("text", record.text.as_str());
    map.put(
        "tags",
        VmValue::strings(record.tags.iter().map(|tag| tag.as_str())),
    );
    map.put_str("stored_at", record.stored_at.as_str());
    map.put_nullable(
        "provenance",
        record
            .provenance
            .as_ref()
            .map(crate::stdlib::json_to_vm_value),
    );
    map.put_nullable_str("status", record.status.as_deref());
    map.put_nullable_str("scope", record.scope.as_deref());
    let mut flag_map = crate::value::DictMap::new();
    for (key, value) in &record.flags {
        flag_map.insert(crate::value::intern_key(key), VmValue::Bool(*value));
    }
    map.put("flags", VmValue::dict(flag_map));
    if let Some(score) = score {
        map.put("score", VmValue::Float(score));
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
    map.put_int("count", records.len() as i64);
    map.put_str("text", text);
    map.put(
        "records",
        VmValue::list(
            records
                .iter()
                .map(|record| memory_record_to_vm(record, None))
                .collect(),
        ),
    );
    VmValue::dict(map)
}

fn forget_result_to_vm(event: &ForgetEvent) -> VmValue {
    let mut map = crate::value::DictMap::new();
    map.put_str("_type", "memory_forget");
    map.put_str("id", event.id.as_str());
    map.put_str("namespace", event.namespace.as_str());
    map.put_int("forgotten", event.forgotten_ids.len() as i64);
    map.put(
        "forgotten_ids",
        VmValue::strings(event.forgotten_ids.iter().map(|id| id.as_str())),
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
    map.put_nullable_str("embed_model_hint", event.embed_model_hint.as_deref());
    map.put_nullable(
        "embed_dim",
        event.embed_dim.map(|dim| VmValue::Int(dim as i64)),
    );
    map.put_nullable("bm25_weight", event.bm25_weight.map(VmValue::Float));
    map.put_nullable("cosine_weight", event.cosine_weight.map(VmValue::Float));
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
mod tests;
