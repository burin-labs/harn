//! Persistent cache primitives used by `std/cache` and LLM wrappers.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::stdlib::registration::{register_builtin_group, BuiltinGroup, SyncBuiltin};
use crate::value::{VmError, VmValue};
use crate::vm::{Vm, VmBuiltinArity};

const DEFAULT_NAMESPACE: &str = "default";
const DEFAULT_TTL_SECONDS: u64 = 600;
const DEFAULT_MAX_ENTRIES: usize = 256;
const SQLITE_CREATE_TABLE: &str = concat!(
    "CREATE TABLE IF NOT EXISTS cache_entries (",
    "namespace TEXT NOT NULL,",
    "cache_key TEXT NOT NULL,",
    "value_json TEXT NOT NULL,",
    "created_at_ms INTEGER NOT NULL,",
    "expires_at_ms INTEGER,",
    "last_accessed_ms INTEGER NOT NULL,",
    "PRIMARY KEY(namespace, cache_key)",
    ");",
);
const SQLITE_CREATE_LRU_INDEX: &str = concat!(
    "CREATE INDEX IF NOT EXISTS idx_cache_entries_lru ",
    "ON cache_entries(namespace, last_accessed_ms);",
);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CacheBackend {
    Sqlite,
    Fs,
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
    register_builtin_group(
        vm,
        BuiltinGroup::new()
            .category("cache")
            .sync(CACHE_SYNC_PRIMITIVES),
    );
}

const CACHE_SYNC_PRIMITIVES: &[SyncBuiltin] = &[
    SyncBuiltin::new("__cache_get", cache_get_builtin)
        .signature("__cache_get(key, options?)")
        .arity(VmBuiltinArity::Range { min: 1, max: 2 })
        .doc("Return a persistent cache hit envelope for key."),
    SyncBuiltin::new("__cache_put", cache_put_builtin)
        .signature("__cache_put(key, value, options?)")
        .arity(VmBuiltinArity::Range { min: 2, max: 3 })
        .doc("Persist a cache value with TTL and LRU eviction."),
    SyncBuiltin::new("__cache_clear", cache_clear_builtin)
        .signature("__cache_clear(options?)")
        .arity(VmBuiltinArity::Range { min: 0, max: 1 })
        .doc("Clear one persistent cache namespace."),
    SyncBuiltin::new("__llm_cache_key", llm_cache_key_builtin)
        .signature("__llm_cache_key(prompt, system?, options?)")
        .arity(VmBuiltinArity::Range { min: 1, max: 3 })
        .doc("Derive the canonical LLM with_cache key."),
];

fn cache_get_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let key = required_string_arg(args, 0, "__cache_get(key, options?)")?;
    let options = parse_cache_options(args.get(1))?;
    let hit = cache_get_at(&options, &key, now_ms()?)?;

    let mut envelope = BTreeMap::new();
    envelope.insert("hit".to_string(), VmValue::Bool(hit.is_some()));
    if let Some(value) = hit {
        envelope.insert("value".to_string(), crate::stdlib::json_to_vm_value(&value));
    }
    Ok(VmValue::Dict(Rc::new(envelope)))
}

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
        now_ms()?,
    )?;

    let mut envelope = BTreeMap::new();
    envelope.insert("stored".to_string(), VmValue::Bool(true));
    envelope.insert("key".to_string(), VmValue::String(Rc::from(key)));
    Ok(VmValue::Dict(Rc::new(envelope)))
}

fn cache_clear_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let options = parse_cache_options(args.first())?;
    match options.backend {
        CacheBackend::Sqlite => sqlite_clear(&options)?,
        CacheBackend::Fs => fs_clear(&options)?,
    }
    Ok(VmValue::Nil)
}

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
    let model_defaults = crate::llm_config::model_params(&model);
    let default_float =
        |key: &str| -> Option<f64> { model_defaults.get(key).and_then(|v| v.as_float()) };

    let max_tokens = super::helpers::opt_int(&options, "max_tokens").unwrap_or(16384);
    let temperature =
        super::helpers::opt_float(&options, "temperature").or_else(|| default_float("temperature"));
    let top_p = super::helpers::opt_float(&options, "top_p").or_else(|| default_float("top_p"));

    let prompt = args
        .first()
        .map(super::helpers::vm_value_to_json)
        .unwrap_or(serde_json::Value::Null);
    let system = args
        .get(1)
        .map(super::helpers::vm_value_to_json)
        .unwrap_or(serde_json::Value::Null);

    let mut identity = BTreeMap::new();
    identity.insert("max_tokens", serde_json::json!(max_tokens));
    identity.insert("model", serde_json::Value::String(model));
    identity.insert("prompt", prompt);
    identity.insert("provider", serde_json::Value::String(provider));
    identity.insert("system", system);
    identity.insert("temperature", json_float_or_null(temperature));
    identity.insert("top_p", json_float_or_null(top_p));

    let canonical = canonical_json_bytes(&serde_json::to_value(identity).map_err(|error| {
        VmError::Runtime(format!(
            "__llm_cache_key: failed to encode identity: {error}"
        ))
    })?)?;
    let digest = Sha256::digest(&canonical);
    Ok(VmValue::String(Rc::from(format!(
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
        other => Err(VmError::Runtime(format!(
            "cache backend must be \"sqlite\" or \"fs\"; got {other:?}"
        ))),
    }
}

fn read_string_field(dict: Option<&BTreeMap<String, VmValue>>, key: &str) -> Option<String> {
    dict.and_then(|dict| dict.get(key))
        .and_then(|value| match value {
            VmValue::String(text) if !text.is_empty() => Some(text.to_string()),
            _ => None,
        })
}

fn read_duration_field(
    dict: Option<&BTreeMap<String, VmValue>>,
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
    dict: Option<&BTreeMap<String, VmValue>>,
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

fn now_ms() -> Result<i64, VmError> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| VmError::Runtime(format!("cache clock error: {error}")))?;
    Ok(duration.as_millis().min(i64::MAX as u128) as i64)
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
    conn.busy_timeout(Duration::from_secs(5))
        .map_err(sqlite_error)?;
    conn.execute_batch(SQLITE_CREATE_TABLE)
        .map_err(sqlite_error)?;
    conn.execute_batch(SQLITE_CREATE_LRU_INDEX)
        .map_err(sqlite_error)?;
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
    tx.execute(
        "UPDATE cache_entries SET last_accessed_ms = ?3 WHERE namespace = ?1 AND cache_key = ?2",
        params![&options.namespace, key, now_ms],
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
    let record = CacheRecord::new(key, value, now_ms, options.ttl_seconds);
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
    record.last_accessed_ms = now_ms;
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
    let record = CacheRecord::new(key, value, now_ms, options.ttl_seconds);
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

fn canonical_json_bytes(value: &serde_json::Value) -> Result<Vec<u8>, VmError> {
    serde_json::to_vec(&canonicalize_json_value(value)).map_err(|error| {
        VmError::Runtime(format!("failed to encode canonical cache JSON: {error}"))
    })
}

fn canonicalize_json_value(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(canonicalize_json_value).collect())
        }
        serde_json::Value::Object(map) => {
            let mut keys = map.keys().collect::<Vec<_>>();
            keys.sort();
            let mut out = serde_json::Map::new();
            for key in keys {
                out.insert(key.clone(), canonicalize_json_value(&map[key]));
            }
            serde_json::Value::Object(out)
        }
        other => other.clone(),
    }
}

#[cfg(test)]
#[path = "cache_tests.rs"]
mod tests;
