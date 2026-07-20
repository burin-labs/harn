//! Persistent cache primitives used by `std/cache` and LLM wrappers.

use crate::value::VmDictExt;
use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::runtime_limits::RuntimeLimits;
use crate::runtime_sqlite::{initialize_runtime_sqlite, RuntimeSqliteSchema, DEFAULT_BUSY_TIMEOUT};
use crate::stdlib::clock::now_wall_ms;
use crate::stdlib::macros::{harn_builtin, register_builtin_defs, VmBuiltinDef};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

const DEFAULT_NAMESPACE: &str = "default";
const DEFAULT_TTL_SECONDS: u64 = 600;
const DEFAULT_MAX_ENTRIES: usize = RuntimeLimits::DEFAULT.max_std_cache_entries;
const SQLITE_SCHEMA: RuntimeSqliteSchema = RuntimeSqliteSchema::new(
    "llm_cache",
    1,
    concat!(
        "CREATE TABLE IF NOT EXISTS cache_entries (",
        "namespace TEXT NOT NULL,",
        "cache_key TEXT NOT NULL,",
        "value_json TEXT NOT NULL,",
        "created_at_ms INTEGER NOT NULL,",
        "expires_at_ms INTEGER,",
        "last_accessed_ms INTEGER NOT NULL,",
        "PRIMARY KEY(namespace, cache_key)",
        ");",
        "CREATE INDEX IF NOT EXISTS idx_cache_entries_lru ",
        "ON cache_entries(namespace, last_accessed_ms);",
    ),
);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CacheBackend {
    Sqlite,
    Fs,
    Mem,
}

impl CacheBackend {
    fn name(&self) -> &'static str {
        match self {
            CacheBackend::Sqlite => "sqlite",
            CacheBackend::Fs => "fs",
            CacheBackend::Mem => "mem",
        }
    }
}

#[derive(Clone, Debug)]
struct CacheOptions {
    backend: CacheBackend,
    namespace: String,
    path: PathBuf,
    ttl_seconds: u64,
    max_entries: usize,
}

#[derive(Debug, Serialize, Deserialize)]
struct CacheRecord {
    key: String,
    value: serde_json::Value,
    created_at_ms: i64,
    expires_at_ms: Option<i64>,
    last_accessed_ms: i64,
}

impl CacheRecord {
    fn new(key: &str, value: serde_json::Value, now_ms: i64, ttl_seconds: u64) -> Self {
        Self {
            key: key.to_string(),
            value,
            created_at_ms: now_ms,
            expires_at_ms: Some(now_ms.saturating_add(ttl_ms(ttl_seconds))),
            last_accessed_ms: now_ms,
        }
    }

    fn is_expired(&self, now_ms: i64) -> bool {
        self.expires_at_ms
            .is_some_and(|expires_at_ms| expires_at_ms <= now_ms)
    }
}

pub(crate) fn register_cache_builtins(vm: &mut Vm) {
    register_builtin_defs(vm, CACHE_BUILTINS);
}

const CACHE_BUILTINS: &[&VmBuiltinDef] = &[
    &CACHE_GET_BUILTIN_DEF,
    &CACHE_PUT_BUILTIN_DEF,
    &CACHE_CLEAR_BUILTIN_DEF,
    &CACHE_STATS_BUILTIN_DEF,
    &CACHE_STATS_RESET_BUILTIN_DEF,
    &LLM_CACHE_KEY_BUILTIN_DEF,
];

fn cache_envelope_base(options: &CacheOptions) -> crate::value::DictMap {
    let mut envelope = crate::value::DictMap::new();
    envelope.put_str("backend", options.backend.name());
    envelope.put_str("namespace", options.namespace.clone());
    envelope
}

/// Return a persistent cache hit envelope for key.
#[harn_builtin(
    sig = "__cache_get(key: string, options?: dict|nil) -> dict",
    category = "cache"
)]
fn cache_get_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let key = required_string_arg(args, 0, "__cache_get(key, options?)")?;
    let options = parse_cache_options(args.get(1))?;
    let hit = cache_get_at(&options, &key, now_wall_ms())?;
    record_lookup(&options, hit.is_some());

    let mut envelope = cache_envelope_base(&options);
    envelope.insert(
        crate::value::intern_key("hit"),
        VmValue::Bool(hit.is_some()),
    );
    if let Some(value) = hit {
        envelope.insert(
            crate::value::intern_key("value"),
            crate::stdlib::json_to_vm_value(&value),
        );
    }
    Ok(VmValue::dict(envelope))
}

/// Persist a cache value with TTL and LRU eviction.
#[harn_builtin(
    sig = "__cache_put(key: string, value: any, options?: dict|nil) -> dict",
    category = "cache"
)]
fn cache_put_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let key = required_string_arg(args, 0, "__cache_put(key, value, options?)")?;
    let value = args.get(1).ok_or_else(|| {
        VmError::Runtime("__cache_put(key, value, options?): value is required".to_string())
    })?;
    let options = parse_cache_options(args.get(2))?;
    cache_put_at(
        &options,
        &key,
        super::helpers::vm_value_to_json(value),
        now_wall_ms(),
    )?;

    let mut envelope = cache_envelope_base(&options);
    envelope.insert(crate::value::intern_key("stored"), VmValue::Bool(true));
    envelope.put_str("key", key);
    Ok(VmValue::dict(envelope))
}

/// Clear one persistent cache namespace.
#[harn_builtin(sig = "__cache_clear(options?: dict|nil) -> nil", category = "cache")]
fn cache_clear_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let options = parse_cache_options(args.first())?;
    match options.backend {
        CacheBackend::Sqlite => sqlite_clear(&options)?,
        CacheBackend::Fs => fs_clear(&options)?,
        CacheBackend::Mem => mem_clear(&options),
    }
    reset_metrics_for(&options);
    Ok(VmValue::Nil)
}

/// Return {hits, misses, lookups, hit_rate} for a cache namespace.
#[harn_builtin(sig = "__cache_stats(options?: dict|nil) -> dict", category = "cache")]
fn cache_stats_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let options = parse_cache_options(args.first())?;
    let snapshot = metrics_snapshot(&options);
    let total = snapshot.hits.saturating_add(snapshot.misses);
    let mut dict = cache_envelope_base(&options);
    dict.insert(
        crate::value::intern_key("hits"),
        VmValue::Int(saturating_u64_to_i64(snapshot.hits)),
    );
    dict.insert(
        crate::value::intern_key("misses"),
        VmValue::Int(saturating_u64_to_i64(snapshot.misses)),
    );
    dict.insert(
        crate::value::intern_key("lookups"),
        VmValue::Int(saturating_u64_to_i64(total)),
    );
    let hit_rate = if total == 0 {
        0.0
    } else {
        snapshot.hits as f64 / total as f64
    };
    dict.insert(
        crate::value::intern_key("hit_rate"),
        VmValue::Float(hit_rate),
    );
    Ok(VmValue::dict(dict))
}

fn saturating_u64_to_i64(value: u64) -> i64 {
    value.min(i64::MAX as u64) as i64
}

/// Reset the in-process hit/miss counters for a cache namespace.
#[harn_builtin(
    sig = "__cache_stats_reset(options?: dict|nil) -> nil",
    category = "cache"
)]
fn cache_stats_reset_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let options = parse_cache_options(args.first())?;
    reset_metrics_for(&options);
    Ok(VmValue::Nil)
}

/// Derive the canonical LLM with_cache key.
#[harn_builtin(
    sig = "__llm_cache_key(prompt: any, system?: any, options?: dict|nil) -> string",
    category = "cache"
)]
fn llm_cache_key_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let explicit_options = match args.get(2) {
        Some(VmValue::Dict(dict)) => Some((**dict).clone()),
        Some(VmValue::Nil) | None => None,
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "__llm_cache_key(prompt, system?, options?): options must be a dict; got {}",
                other.type_name()
            )))
        }
    };
    let options = super::cost_route::merge_context_options(explicit_options);

    let provider = super::helpers::vm_resolve_provider(&options);
    let model = super::helpers::vm_resolve_model(&options, &provider);
    let model_defaults = crate::llm_config::model_params_for_route(&provider, &model);
    let default_float =
        |key: &str| -> Option<f64> { model_defaults.get(key).and_then(|v| v.as_float()) };
    let default_int =
        |key: &str| -> Option<i64> { model_defaults.get(key).and_then(|v| v.as_integer()) };

    let max_tokens = super::helpers::opt_int(&options, "max_tokens")
        .or_else(|| default_int("max_tokens"))
        .unwrap_or(16384);
    let temperature =
        super::helpers::opt_float(&options, "temperature").or_else(|| default_float("temperature"));
    let top_p = super::helpers::opt_float(&options, "top_p").or_else(|| default_float("top_p"));
    let top_k = super::helpers::opt_int(&options, "top_k").or_else(|| default_int("top_k"));
    let frequency_penalty = super::helpers::opt_float(&options, "frequency_penalty")
        .or_else(|| default_float("frequency_penalty"));
    let presence_penalty = super::helpers::opt_float(&options, "presence_penalty")
        .or_else(|| default_float("presence_penalty"));
    let caps = crate::llm::capabilities::lookup(&provider, &model);
    let enforce_capability_gates = !crate::llm::mock::cli_llm_mock_replay_active()
        && !crate::llm::mock::builtin_llm_mock_active();
    let thinking = super::helpers::resolve_thinking_config(
        options.as_ref(),
        &model_defaults,
        &provider,
        &model,
        &caps,
        enforce_capability_gates,
    )?;

    let prompt = args
        .first()
        .map(super::helpers::vm_value_to_json)
        .unwrap_or(serde_json::Value::Null);
    let system = args
        .get(1)
        .map(super::helpers::vm_value_to_json)
        .unwrap_or(serde_json::Value::Null);

    let mut identity = std::collections::BTreeMap::new();
    identity.insert("max_tokens", serde_json::json!(max_tokens));
    identity.insert("model", serde_json::Value::String(model));
    identity.insert("prompt", prompt);
    identity.insert("provider", serde_json::Value::String(provider));
    identity.insert("system", system);
    identity.insert("temperature", json_float_or_null(temperature));
    identity.insert("top_p", json_float_or_null(top_p));
    identity.insert("top_k", serde_json::json!(top_k));
    identity.insert("frequency_penalty", json_float_or_null(frequency_penalty));
    identity.insert("presence_penalty", json_float_or_null(presence_penalty));
    if !thinking.is_disabled() {
        identity.insert(
            "thinking",
            serde_json::to_value(thinking).map_err(|error| {
                VmError::Runtime(format!(
                    "__llm_cache_key: failed to encode thinking identity: {error}"
                ))
            })?,
        );
    }

    // Tools, structured-output schema, and stop sequences all change the
    // model's output, so two calls that differ ONLY in one of these must NOT
    // collide on the same cache key. They were previously omitted, which
    // returned a wrong cached response (e.g. a tool-less reply served to a
    // tool-bearing call, or a free-text reply served to a json_schema call).
    // Inserted only when present so plain-text calls keep stable keys.
    let opt_json = |key: &str| -> Option<serde_json::Value> {
        options
            .as_ref()
            .and_then(|map| map.get(key))
            .filter(|v| !matches!(v, VmValue::Nil))
            .map(super::helpers::vm_value_to_json)
    };
    if let Some(tools) = opt_json("tools") {
        identity.insert("tools", tools);
    }
    // Structured-output identity: any of the schema-carrying option keys
    // (set by `llm_call_structured` and friends) participate in the key.
    for schema_key in [
        "output_schema",
        "json_schema",
        "output_format",
        "response_format",
    ] {
        if let Some(schema) = opt_json(schema_key) {
            identity.insert(schema_key, schema);
        }
    }
    for stop_key in ["stop", "stop_sequences"] {
        if let Some(stop) = opt_json(stop_key) {
            identity.insert(stop_key, stop);
        }
    }
    if let Some(mock_scope) = opt_json("mock_scope") {
        identity.insert("mock_scope", mock_scope);
    }

    let canonical = crate::canonical_json::of(&identity).map_err(|error| {
        VmError::Runtime(format!(
            "__llm_cache_key: failed to encode identity: {error}"
        ))
    })?;
    let digest = Sha256::digest(canonical.as_bytes());
    Ok(VmValue::String(arcstr::ArcStr::from(format!(
        "sha256:{}",
        hex::encode(digest)
    ))))
}

fn cache_get_at(
    options: &CacheOptions,
    key: &str,
    now_ms: i64,
) -> Result<Option<serde_json::Value>, VmError> {
    match options.backend {
        CacheBackend::Sqlite => sqlite_get(options, key, now_ms),
        CacheBackend::Fs => fs_get(options, key, now_ms),
        CacheBackend::Mem => Ok(mem_get(options, key, now_ms)),
    }
}

fn cache_put_at(
    options: &CacheOptions,
    key: &str,
    value: serde_json::Value,
    now_ms: i64,
) -> Result<(), VmError> {
    match options.backend {
        CacheBackend::Sqlite => sqlite_put(options, key, value, now_ms),
        CacheBackend::Fs => fs_put(options, key, value, now_ms),
        CacheBackend::Mem => {
            mem_put(options, key, value, now_ms);
            Ok(())
        }
    }
}

fn parse_cache_options(value: Option<&VmValue>) -> Result<CacheOptions, VmError> {
    let dict = match value {
        Some(VmValue::Dict(dict)) => Some(&**dict),
        Some(VmValue::Nil) | None => None,
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "cache options must be a dict or nil; got {}",
                other.type_name()
            )))
        }
    };

    let store_dict = match dict.and_then(|dict| dict.get("store")) {
        Some(VmValue::Dict(store)) => Some(&**store),
        Some(VmValue::String(_)) | Some(VmValue::Nil) | None => None,
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "cache options.store must be a string, dict, or nil; got {}",
                other.type_name()
            )))
        }
    };

    let backend = read_string_field(store_dict, "backend")
        .or_else(|| read_string_field(dict, "backend"))
        .map(|backend| parse_backend(&backend))
        .transpose()?
        .unwrap_or(CacheBackend::Sqlite);

    let namespace = read_string_field(store_dict, "namespace")
        .or_else(|| read_string_field(store_dict, "name"))
        .or_else(|| match dict.and_then(|dict| dict.get("store")) {
            Some(VmValue::String(name)) => Some(name.to_string()),
            _ => None,
        })
        .or_else(|| read_string_field(dict, "namespace"))
        .or_else(|| read_string_field(dict, "name"))
        .unwrap_or_else(|| DEFAULT_NAMESPACE.to_string());

    let path = read_string_field(store_dict, "path")
        .or_else(|| read_string_field(store_dict, "cache_dir"))
        .or_else(|| read_string_field(dict, "path"))
        .or_else(|| read_string_field(dict, "cache_dir"))
        .map(resolve_cache_path)
        .unwrap_or_else(|| default_cache_path(backend));

    let ttl_seconds = read_duration_field(store_dict, "ttl")
        .transpose()?
        .or(read_duration_field(store_dict, "ttl_seconds").transpose()?)
        .or(read_duration_field(dict, "ttl").transpose()?)
        .or(read_duration_field(dict, "ttl_seconds").transpose()?)
        .or(read_duration_field(dict, "max_age_seconds").transpose()?)
        .unwrap_or(DEFAULT_TTL_SECONDS);

    let max_entries = read_usize_field(store_dict, "max_entries")
        .transpose()?
        .or(read_usize_field(dict, "max_entries").transpose()?)
        .unwrap_or(DEFAULT_MAX_ENTRIES)
        .clamp(1, 100_000);

    Ok(CacheOptions {
        backend,
        namespace: sanitize_namespace(&namespace),
        path,
        ttl_seconds,
        max_entries,
    })
}

fn parse_backend(value: &str) -> Result<CacheBackend, VmError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "" | "sqlite" => Ok(CacheBackend::Sqlite),
        "fs" | "file" | "files" => Ok(CacheBackend::Fs),
        "mem" | "memory" | "lru" => Ok(CacheBackend::Mem),
        other => Err(VmError::Runtime(format!(
            "cache backend must be \"mem\", \"fs\", or \"sqlite\"; got {other:?}"
        ))),
    }
}

fn read_string_field(dict: Option<&crate::value::DictMap>, key: &str) -> Option<String> {
    dict.and_then(|dict| dict.get(key))
        .and_then(|value| match value {
            VmValue::String(text) if !text.is_empty() => Some(text.to_string()),
            _ => None,
        })
}

fn read_duration_field(
    dict: Option<&crate::value::DictMap>,
    key: &str,
) -> Option<Result<u64, VmError>> {
    dict.and_then(|dict| dict.get(key))
        .and_then(|value| match value {
            VmValue::Nil => None,
            VmValue::Int(seconds) => Some(Ok((*seconds).max(0) as u64)),
            VmValue::Float(seconds) => Some(Ok(seconds.max(0.0) as u64)),
            VmValue::String(text) => Some(parse_duration_seconds(text)),
            other => Some(Err(VmError::Runtime(format!(
                "cache option {key} must be seconds or a duration string; got {}",
                other.type_name()
            )))),
        })
}

fn read_usize_field(
    dict: Option<&crate::value::DictMap>,
    key: &str,
) -> Option<Result<usize, VmError>> {
    dict.and_then(|dict| dict.get(key))
        .and_then(|value| match value {
            VmValue::Nil => None,
            VmValue::Int(value) => Some(Ok((*value).max(1) as usize)),
            other => Some(Err(VmError::Runtime(format!(
                "cache option {key} must be an int; got {}",
                other.type_name()
            )))),
        })
}

fn parse_duration_seconds(text: &str) -> Result<u64, VmError> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(DEFAULT_TTL_SECONDS);
    }
    if let Ok(seconds) = trimmed.parse::<u64>() {
        return Ok(seconds);
    }
    let lower = trimmed.to_ascii_lowercase();
    for (suffix, multiplier) in [
        ("secs", 1.0),
        ("sec", 1.0),
        ("ms", 1.0 / 1000.0),
        ("mins", 60.0),
        ("min", 60.0),
        ("hrs", 3600.0),
        ("hr", 3600.0),
        ("s", 1.0),
        ("m", 60.0),
        ("h", 3600.0),
        ("d", 86_400.0),
    ] {
        if let Some(number) = lower.strip_suffix(suffix) {
            let value = number.trim().parse::<f64>().map_err(|_| {
                VmError::Runtime(format!("cache duration {text:?} has an invalid number"))
            })?;
            return Ok((value.max(0.0) * multiplier).ceil() as u64);
        }
    }
    Err(VmError::Runtime(format!(
        "cache duration {text:?} must use ms, s, m, h, or d"
    )))
}

fn ttl_ms(ttl_seconds: u64) -> i64 {
    let max_safe_seconds = (i64::MAX as u64) / 1000;
    ttl_seconds.min(max_safe_seconds).saturating_mul(1000) as i64
}

fn default_cache_path(backend: CacheBackend) -> PathBuf {
    let root = crate::runtime_paths::state_root(&crate::stdlib::process::runtime_root_base())
        .join("cache");
    match backend {
        CacheBackend::Sqlite => root.join("llm.sqlite"),
        CacheBackend::Fs => root.join("llm"),
        CacheBackend::Mem => PathBuf::new(),
    }
}

fn resolve_cache_path(path: String) -> PathBuf {
    let candidate = PathBuf::from(path);
    if candidate.is_absolute() {
        candidate
    } else {
        crate::stdlib::process::resolve_source_relative_path(&candidate.to_string_lossy())
    }
}

fn sanitize_namespace(namespace: &str) -> String {
    let mut out = String::with_capacity(namespace.len());
    for ch in namespace.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        DEFAULT_NAMESPACE.to_string()
    } else {
        out
    }
}

fn required_string_arg(args: &[VmValue], index: usize, signature: &str) -> Result<String, VmError> {
    match args.get(index) {
        Some(VmValue::String(text)) if !text.is_empty() => Ok(text.to_string()),
        Some(other) => Err(VmError::Runtime(format!(
            "{signature}: argument {} must be a non-empty string; got {}",
            index + 1,
            other.type_name()
        ))),
        None => Err(VmError::Runtime(format!(
            "{signature}: argument {} is required",
            index + 1
        ))),
    }
}

fn sqlite_connection(path: &Path) -> Result<Connection, VmError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            VmError::Runtime(format!(
                "cache sqlite: failed to create {}: {error}",
                parent.display()
            ))
        })?;
    }
    let conn = Connection::open(path).map_err(sqlite_error)?;
    initialize_runtime_sqlite(&conn, DEFAULT_BUSY_TIMEOUT, &SQLITE_SCHEMA)
        .map_err(|error| VmError::Runtime(format!("cache sqlite setup error: {error}")))?;
    Ok(conn)
}

fn sqlite_get(
    options: &CacheOptions,
    key: &str,
    now_ms: i64,
) -> Result<Option<serde_json::Value>, VmError> {
    let mut conn = sqlite_connection(&options.path)?;
    let tx = conn.transaction().map_err(sqlite_error)?;
    let row: Option<(String, Option<i64>)> = tx
        .query_row(
            "SELECT value_json, expires_at_ms FROM cache_entries WHERE namespace = ?1 AND cache_key = ?2",
            params![&options.namespace, key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some((value_json, expires_at_ms)) = row else {
        return Ok(None);
    };
    if expires_at_ms.is_some_and(|expires_at_ms| expires_at_ms <= now_ms) {
        tx.execute(
            "DELETE FROM cache_entries WHERE namespace = ?1 AND cache_key = ?2",
            params![&options.namespace, key],
        )
        .map_err(sqlite_error)?;
        tx.commit().map_err(sqlite_error)?;
        return Ok(None);
    }
    let value = match serde_json::from_str(&value_json) {
        Ok(value) => value,
        Err(_) => {
            tx.execute(
                "DELETE FROM cache_entries WHERE namespace = ?1 AND cache_key = ?2",
                params![&options.namespace, key],
            )
            .map_err(sqlite_error)?;
            tx.commit().map_err(sqlite_error)?;
            return Ok(None);
        }
    };
    let last_accessed_ms = next_access_ms_for(options, now_ms);
    tx.execute(
        "UPDATE cache_entries SET last_accessed_ms = ?3 WHERE namespace = ?1 AND cache_key = ?2",
        params![&options.namespace, key, last_accessed_ms],
    )
    .map_err(sqlite_error)?;
    tx.commit().map_err(sqlite_error)?;
    Ok(Some(value))
}

fn sqlite_put(
    options: &CacheOptions,
    key: &str,
    value: serde_json::Value,
    now_ms: i64,
) -> Result<(), VmError> {
    let mut conn = sqlite_connection(&options.path)?;
    let tx = conn.transaction().map_err(sqlite_error)?;
    let mut record = CacheRecord::new(key, value, now_ms, options.ttl_seconds);
    record.last_accessed_ms = next_access_ms_for(options, now_ms);
    let value_json = serde_json::to_string(&record.value).map_err(|error| {
        VmError::Runtime(format!("cache sqlite: failed to encode value: {error}"))
    })?;
    tx.execute(
        "INSERT OR REPLACE INTO cache_entries
         (namespace, cache_key, value_json, created_at_ms, expires_at_ms, last_accessed_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            &options.namespace,
            record.key,
            value_json,
            record.created_at_ms,
            record.expires_at_ms,
            record.last_accessed_ms
        ],
    )
    .map_err(sqlite_error)?;
    sqlite_evict(&tx, options, now_ms)?;
    tx.commit().map_err(sqlite_error)
}

fn sqlite_evict(conn: &Connection, options: &CacheOptions, now_ms: i64) -> Result<(), VmError> {
    conn.execute(
        "DELETE FROM cache_entries WHERE namespace = ?1 AND expires_at_ms IS NOT NULL AND expires_at_ms <= ?2",
        params![&options.namespace, now_ms],
    )
    .map_err(sqlite_error)?;

    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM cache_entries WHERE namespace = ?1",
            params![&options.namespace],
            |row| row.get(0),
        )
        .map_err(sqlite_error)?;
    let excess = (count as usize).saturating_sub(options.max_entries);
    if excess == 0 {
        return Ok(());
    }
    let keys = {
        let mut stmt = conn
            .prepare(
                "SELECT cache_key FROM cache_entries
                 WHERE namespace = ?1
                 ORDER BY last_accessed_ms ASC, created_at_ms ASC
                 LIMIT ?2",
            )
            .map_err(sqlite_error)?;
        let keys = stmt
            .query_map(params![&options.namespace, excess as i64], |row| {
                row.get::<_, String>(0)
            })
            .map_err(sqlite_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_error)?;
        keys
    };
    for key in keys {
        conn.execute(
            "DELETE FROM cache_entries WHERE namespace = ?1 AND cache_key = ?2",
            params![&options.namespace, key],
        )
        .map_err(sqlite_error)?;
    }
    Ok(())
}

fn sqlite_clear(options: &CacheOptions) -> Result<(), VmError> {
    let mut conn = sqlite_connection(&options.path)?;
    let tx = conn.transaction().map_err(sqlite_error)?;
    tx.execute(
        "DELETE FROM cache_entries WHERE namespace = ?1",
        params![&options.namespace],
    )
    .map_err(sqlite_error)?;
    tx.commit().map_err(sqlite_error)
}

fn sqlite_error(error: rusqlite::Error) -> VmError {
    VmError::Runtime(format!("cache sqlite error: {error}"))
}

fn fs_key_path(options: &CacheOptions, key: &str) -> PathBuf {
    options
        .path
        .join(&options.namespace)
        .join(format!("{}.json", sha256_hex(key.as_bytes())))
}

fn fs_get(
    options: &CacheOptions,
    key: &str,
    now_ms: i64,
) -> Result<Option<serde_json::Value>, VmError> {
    let path = fs_key_path(options, key);
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return Ok(None);
    };
    let Ok(mut record) = serde_json::from_str::<CacheRecord>(&contents) else {
        let _ = std::fs::remove_file(path);
        return Ok(None);
    };
    if record.key != key || record.is_expired(now_ms) {
        let _ = std::fs::remove_file(path);
        return Ok(None);
    }
    record.last_accessed_ms = next_access_ms_for(options, now_ms);
    write_fs_record(&path, &record)?;
    Ok(Some(record.value))
}

fn fs_put(
    options: &CacheOptions,
    key: &str,
    value: serde_json::Value,
    now_ms: i64,
) -> Result<(), VmError> {
    let path = fs_key_path(options, key);
    let mut record = CacheRecord::new(key, value, now_ms, options.ttl_seconds);
    record.last_accessed_ms = next_access_ms_for(options, now_ms);
    write_fs_record(&path, &record)?;
    fs_evict(options, now_ms)
}

fn fs_evict(options: &CacheOptions, now_ms: i64) -> Result<(), VmError> {
    let dir = options.path.join(&options.namespace);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Ok(());
    };
    let mut live = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            VmError::Runtime(format!(
                "cache fs: failed to read {}: {error}",
                dir.display()
            ))
        })?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Ok(contents) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(record) = serde_json::from_str::<CacheRecord>(&contents) else {
            let _ = std::fs::remove_file(path);
            continue;
        };
        if record.is_expired(now_ms) {
            let _ = std::fs::remove_file(path);
        } else {
            live.push((path, record.last_accessed_ms, record.created_at_ms));
        }
    }
    let excess = live.len().saturating_sub(options.max_entries);
    if excess == 0 {
        return Ok(());
    }
    live.sort_by_key(|(_, last_accessed_ms, created_at_ms)| (*last_accessed_ms, *created_at_ms));
    for (path, _, _) in live.into_iter().take(excess) {
        let _ = std::fs::remove_file(path);
    }
    Ok(())
}

fn fs_clear(options: &CacheOptions) -> Result<(), VmError> {
    let dir = options.path.join(&options.namespace);
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(VmError::Runtime(format!(
            "cache fs: failed to clear {}: {error}",
            dir.display()
        ))),
    }
}

fn write_fs_record(path: &Path, record: &CacheRecord) -> Result<(), VmError> {
    let serialized = serde_json::to_vec_pretty(record)
        .map_err(|error| VmError::Runtime(format!("cache fs: failed to encode record: {error}")))?;
    crate::atomic_io::atomic_write(path, &serialized).map_err(|error| {
        VmError::Runtime(format!(
            "cache fs: failed to write {}: {error}",
            path.display()
        ))
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn json_float_or_null(value: Option<f64>) -> serde_json::Value {
    value
        .and_then(serde_json::Number::from_f64)
        .map(serde_json::Value::Number)
        .unwrap_or(serde_json::Value::Null)
}

thread_local! {
    static ACCESS_CLOCK: RefCell<BTreeMap<String, i64>> =
        const { RefCell::new(std::collections::BTreeMap::new()) };
}

fn access_clock_key(options: &CacheOptions) -> String {
    format!(
        "{}:{}:{}",
        options.backend.name(),
        options.namespace,
        options.path.display()
    )
}

fn next_access_ms_for(options: &CacheOptions, now_ms: i64) -> i64 {
    let key = access_clock_key(options);
    ACCESS_CLOCK.with(|clock| {
        let mut clock = clock.borrow_mut();
        let previous = clock.get(&key).copied().unwrap_or(i64::MIN);
        let next = now_ms.max(previous.saturating_add(1));
        clock.insert(key, next);
        next
    })
}

// -------------------------------------------------------------------------
// in-process LRU backend
// -------------------------------------------------------------------------

struct MemEntry {
    value: serde_json::Value,
    expires_at_ms: Option<i64>,
}

thread_local! {
    static MEM_STORE: RefCell<BTreeMap<String, MemNamespace>> =
        const { RefCell::new(std::collections::BTreeMap::new()) };
}

#[derive(Default)]
struct MemNamespace {
    /// LRU order: front is least-recently used, back is most-recently used.
    order: VecDeque<String>,
    entries: BTreeMap<String, MemEntry>,
}

fn mem_get(options: &CacheOptions, key: &str, now_ms: i64) -> Option<serde_json::Value> {
    MEM_STORE.with(|store| {
        let mut store = store.borrow_mut();
        let ns = store.entry(options.namespace.clone()).or_default();
        let expired = match ns.entries.get(key) {
            Some(entry) if entry.expires_at_ms.is_some_and(|exp| exp <= now_ms) => true,
            Some(_) => false,
            None => return None,
        };
        if expired {
            ns.entries.remove(key);
            ns.order.retain(|k| k != key);
            return None;
        }
        let value = ns.entries.get(key).map(|entry| entry.value.clone());
        ns.order.retain(|k| k != key);
        ns.order.push_back(key.to_string());
        value
    })
}

fn mem_put(options: &CacheOptions, key: &str, value: serde_json::Value, now_ms: i64) {
    MEM_STORE.with(|store| {
        let mut store = store.borrow_mut();
        let max_entries = options.max_entries;
        let expires_at_ms = Some(now_ms.saturating_add(ttl_ms(options.ttl_seconds)));
        let ns = store.entry(options.namespace.clone()).or_default();
        ns.entries.insert(
            key.to_string(),
            MemEntry {
                value,
                expires_at_ms,
            },
        );
        ns.order.retain(|k| k != key);
        ns.order.push_back(key.to_string());
        while ns.entries.len() > max_entries {
            if let Some(evicted) = ns.order.pop_front() {
                ns.entries.remove(&evicted);
            } else {
                break;
            }
        }
    });
}

fn mem_clear(options: &CacheOptions) {
    MEM_STORE.with(|store| {
        store.borrow_mut().remove(&options.namespace);
    });
}

// -------------------------------------------------------------------------
// in-process metrics
// -------------------------------------------------------------------------

#[derive(Default, Clone, Copy)]
struct MetricsSnapshot {
    hits: u64,
    misses: u64,
}

#[derive(Default)]
struct MetricsStore {
    by_namespace: BTreeMap<String, MetricsSnapshot>,
}

thread_local! {
    static METRICS: RefCell<MetricsStore> = const { RefCell::new(MetricsStore { by_namespace: std::collections::BTreeMap::new() }) };
}

fn metrics_key(options: &CacheOptions) -> String {
    format!("{}:{}", options.backend.name(), options.namespace)
}

fn record_lookup(options: &CacheOptions, hit: bool) {
    let key = metrics_key(options);
    METRICS.with(|metrics| {
        let mut metrics = metrics.borrow_mut();
        let entry = metrics.by_namespace.entry(key).or_default();
        if hit {
            entry.hits = entry.hits.saturating_add(1);
        } else {
            entry.misses = entry.misses.saturating_add(1);
        }
    });
}

fn metrics_snapshot(options: &CacheOptions) -> MetricsSnapshot {
    let key = metrics_key(options);
    METRICS.with(|metrics| {
        metrics
            .borrow()
            .by_namespace
            .get(&key)
            .copied()
            .unwrap_or_default()
    })
}

fn reset_metrics_for(options: &CacheOptions) {
    let key = metrics_key(options);
    METRICS.with(|metrics| {
        metrics.borrow_mut().by_namespace.remove(&key);
    });
}

/// Drop the per-thread in-memory cache and metrics. Called by
/// `reset_stdlib_state` between top-level VM runs so the LRU store and
/// hit/miss counters do not leak across test cases.
pub fn reset_in_process_cache_state() {
    MEM_STORE.with(|store| store.borrow_mut().clear());
    METRICS.with(|metrics| metrics.borrow_mut().by_namespace.clear());
    ACCESS_CLOCK.with(|clock| clock.borrow_mut().clear());
}

#[cfg(test)]
#[path = "cache_tests.rs"]
mod tests;
