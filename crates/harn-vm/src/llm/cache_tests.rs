use std::path::PathBuf;

use rusqlite::params;

use super::*;

fn sqlite_options(path: PathBuf) -> CacheOptions {
    CacheOptions {
        backend: CacheBackend::Sqlite,
        namespace: "test".to_string(),
        path,
        ttl_seconds: 60,
        max_entries: 2,
    }
}

#[test]
fn sqlite_cache_hits_update_lru_and_evict_oldest() {
    let dir = tempfile::tempdir().expect("tempdir");
    let options = sqlite_options(dir.path().join("cache.sqlite"));

    cache_put_at(&options, "a", serde_json::json!({"value": "a"}), 1_000).unwrap();
    cache_put_at(&options, "b", serde_json::json!({"value": "b"}), 2_000).unwrap();
    assert_eq!(
        cache_get_at(&options, "a", 3_000).unwrap(),
        Some(serde_json::json!({"value": "a"}))
    );
    cache_put_at(&options, "c", serde_json::json!({"value": "c"}), 4_000).unwrap();

    assert_eq!(cache_get_at(&options, "b", 5_000).unwrap(), None);
    assert!(cache_get_at(&options, "a", 5_000).unwrap().is_some());
    assert!(cache_get_at(&options, "c", 5_000).unwrap().is_some());
}

#[test]
fn sqlite_cache_expires_entries() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut options = sqlite_options(dir.path().join("cache.sqlite"));
    options.ttl_seconds = 1;

    cache_put_at(&options, "a", serde_json::json!("cached"), 1_000).unwrap();

    assert_eq!(
        cache_get_at(&options, "a", 1_999).unwrap(),
        Some(serde_json::json!("cached"))
    );
    assert_eq!(cache_get_at(&options, "a", 2_000).unwrap(), None);
}

#[test]
fn fs_cache_hits_and_evicts_oldest() {
    let dir = tempfile::tempdir().expect("tempdir");
    let options = CacheOptions {
        backend: CacheBackend::Fs,
        namespace: "test".to_string(),
        path: dir.path().join("fs-cache"),
        ttl_seconds: 60,
        max_entries: 1,
    };

    cache_put_at(&options, "a", serde_json::json!({"value": "a"}), 1_000).unwrap();
    assert_eq!(
        cache_get_at(&options, "a", 2_000).unwrap(),
        Some(serde_json::json!({"value": "a"}))
    );
    cache_put_at(&options, "b", serde_json::json!({"value": "b"}), 3_000).unwrap();

    assert_eq!(cache_get_at(&options, "a", 4_000).unwrap(), None);
    assert_eq!(
        cache_get_at(&options, "b", 4_000).unwrap(),
        Some(serde_json::json!({"value": "b"}))
    );
}

#[test]
fn fs_cache_evicts_lru_when_operations_share_wall_millisecond() {
    reset_in_process_cache_state();
    let dir = tempfile::tempdir().expect("tempdir");
    let options = CacheOptions {
        backend: CacheBackend::Fs,
        namespace: "test".to_string(),
        path: dir.path().join("fs-cache"),
        ttl_seconds: 60,
        max_entries: 2,
    };

    cache_put_at(&options, "a", serde_json::json!({"value": "a"}), 1_000).unwrap();
    cache_put_at(&options, "b", serde_json::json!({"value": "b"}), 1_000).unwrap();
    cache_put_at(&options, "c", serde_json::json!({"value": "c"}), 1_000).unwrap();

    assert_eq!(cache_get_at(&options, "a", 1_000).unwrap(), None);
    assert_eq!(
        cache_get_at(&options, "b", 1_000).unwrap(),
        Some(serde_json::json!({"value": "b"}))
    );
    assert_eq!(
        cache_get_at(&options, "c", 1_000).unwrap(),
        Some(serde_json::json!({"value": "c"}))
    );
}

#[test]
fn sqlite_cache_evicts_lru_when_operations_share_wall_millisecond() {
    reset_in_process_cache_state();
    let dir = tempfile::tempdir().expect("tempdir");
    let options = sqlite_options(dir.path().join("cache.sqlite"));

    cache_put_at(&options, "a", serde_json::json!({"value": "a"}), 1_000).unwrap();
    cache_put_at(&options, "b", serde_json::json!({"value": "b"}), 1_000).unwrap();
    cache_put_at(&options, "c", serde_json::json!({"value": "c"}), 1_000).unwrap();

    assert_eq!(cache_get_at(&options, "a", 1_000).unwrap(), None);
    assert_eq!(
        cache_get_at(&options, "b", 1_000).unwrap(),
        Some(serde_json::json!({"value": "b"}))
    );
    assert_eq!(
        cache_get_at(&options, "c", 1_000).unwrap(),
        Some(serde_json::json!({"value": "c"}))
    );
}

#[test]
fn corrupt_cache_entries_are_misses() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sqlite = sqlite_options(dir.path().join("cache.sqlite"));
    {
        let conn = sqlite_connection(&sqlite.path).unwrap();
        conn.execute(
            "INSERT INTO cache_entries
             (namespace, cache_key, value_json, created_at_ms, expires_at_ms, last_accessed_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params!["test", "bad", "{not json", 1_000, 61_000, 1_000],
        )
        .unwrap();
    }
    assert_eq!(cache_get_at(&sqlite, "bad", 2_000).unwrap(), None);
    assert_eq!(cache_get_at(&sqlite, "bad", 2_000).unwrap(), None);

    let fs = CacheOptions {
        backend: CacheBackend::Fs,
        namespace: "test".to_string(),
        path: dir.path().join("fs-cache"),
        ttl_seconds: 60,
        max_entries: 1,
    };
    let path = fs_key_path(&fs, "bad");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"{not json").unwrap();

    assert_eq!(cache_get_at(&fs, "bad", 2_000).unwrap(), None);
    assert!(!path.exists());
}

#[test]
fn canonical_json_sorts_nested_object_keys() {
    let first = serde_json::json!({"b": 2, "a": {"d": 4, "c": 3}});
    let second = serde_json::json!({"a": {"c": 3, "d": 4}, "b": 2});

    assert_eq!(
        canonical_json_bytes(&first).unwrap(),
        canonical_json_bytes(&second).unwrap()
    );
}

#[test]
fn cache_record_large_ttl_saturates_forward() {
    let record = CacheRecord::new("a", serde_json::json!(true), 1_000, u64::MAX);

    assert_eq!(record.expires_at_ms, Some(i64::MAX));
}

fn mem_options(namespace: &str, max_entries: usize, ttl_seconds: u64) -> CacheOptions {
    CacheOptions {
        backend: CacheBackend::Mem,
        namespace: namespace.to_string(),
        path: PathBuf::new(),
        ttl_seconds,
        max_entries,
    }
}

#[test]
fn mem_cache_hits_update_lru_and_evict_oldest() {
    reset_in_process_cache_state();
    let options = mem_options("mem_lru", 2, 60);

    cache_put_at(&options, "a", serde_json::json!({"value": "a"}), 1_000).unwrap();
    cache_put_at(&options, "b", serde_json::json!({"value": "b"}), 2_000).unwrap();
    assert_eq!(
        cache_get_at(&options, "a", 3_000).unwrap(),
        Some(serde_json::json!({"value": "a"}))
    );
    cache_put_at(&options, "c", serde_json::json!({"value": "c"}), 4_000).unwrap();

    assert_eq!(cache_get_at(&options, "b", 5_000).unwrap(), None);
    assert!(cache_get_at(&options, "a", 5_000).unwrap().is_some());
    assert!(cache_get_at(&options, "c", 5_000).unwrap().is_some());
}

#[test]
fn mem_cache_expires_entries() {
    reset_in_process_cache_state();
    let options = mem_options("mem_ttl", 4, 1);

    cache_put_at(&options, "a", serde_json::json!("cached"), 1_000).unwrap();
    assert_eq!(
        cache_get_at(&options, "a", 1_999).unwrap(),
        Some(serde_json::json!("cached"))
    );
    assert_eq!(cache_get_at(&options, "a", 2_000).unwrap(), None);
}

#[test]
fn mem_cache_clear_resets_metrics_and_entries() {
    reset_in_process_cache_state();
    let options = mem_options("mem_clear", 4, 60);

    cache_put_at(&options, "a", serde_json::json!(1), 1_000).unwrap();
    cache_get_at(&options, "a", 2_000).unwrap();
    cache_get_at(&options, "missing", 2_000).unwrap();
    record_lookup(&options, true);
    record_lookup(&options, false);

    assert_eq!(metrics_snapshot(&options).hits, 1);
    assert_eq!(metrics_snapshot(&options).misses, 1);

    mem_clear(&options);
    reset_metrics_for(&options);

    assert_eq!(cache_get_at(&options, "a", 3_000).unwrap(), None);
    assert_eq!(metrics_snapshot(&options).hits, 0);
    assert_eq!(metrics_snapshot(&options).misses, 0);
}

// Multi-session hardening: the sqlite cache must open in WAL mode so two
// sessions in the same project can run read-heavy LLM roles concurrently
// without a writer blocking readers into "database is locked".
#[test]
fn sqlite_cache_connection_uses_wal_journal_mode() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("cache.sqlite");
    // Force the file (and its pragmas) to be created via the production path.
    let options = sqlite_options(path.clone());
    cache_put_at(&options, "a", serde_json::json!({"value": "a"}), 1_000).unwrap();

    let conn = sqlite_connection(&path).expect("open cache connection");
    let mode: String = conn
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .expect("read journal_mode");
    assert_eq!(
        mode.to_lowercase(),
        "wal",
        "cache sqlite must be WAL for cross-session concurrency"
    );
}

// Regression for the cache-key omission bug: tools, structured-output schema,
// and stop sequences each change the model's output, so two calls differing
// only in one of them must produce DIFFERENT cache keys (otherwise a wrong
// cached response is returned). Calls that don't carry these keep stable keys.
#[cfg(test)]
mod cache_key_identity_tests {
    use super::*;
    use std::sync::Arc;

    fn key(prompt: &str, options: VmValue) -> String {
        let mut out = String::new();
        let result = llm_cache_key_builtin(
            &[
                VmValue::String(arcstr::ArcStr::from(prompt)),
                VmValue::Nil,
                options,
            ],
            &mut out,
        )
        .expect("cache key");
        match result {
            VmValue::String(s) => s.to_string(),
            other => panic!("expected string key, got {other:?}"),
        }
    }

    fn base_options() -> crate::value::DictMap {
        let mut map = crate::value::DictMap::new();
        map.insert(
            crate::value::intern_key("provider"),
            VmValue::String(arcstr::ArcStr::from("mock")),
        );
        map.insert(
            crate::value::intern_key("model"),
            VmValue::String(arcstr::ArcStr::from("mock")),
        );
        map
    }

    fn gpt_oss_options() -> crate::value::DictMap {
        let mut map = crate::value::DictMap::new();
        map.insert(
            crate::value::intern_key("provider"),
            VmValue::String(arcstr::ArcStr::from("fireworks")),
        );
        map.insert(
            crate::value::intern_key("model"),
            VmValue::String(arcstr::ArcStr::from(
                "accounts/fireworks/models/gpt-oss-120b",
            )),
        );
        map
    }

    fn dict(map: crate::value::DictMap) -> VmValue {
        VmValue::dict(map)
    }

    #[test]
    fn identical_options_produce_identical_keys() {
        let a = key("hello", dict(base_options()));
        let b = key("hello", dict(base_options()));
        assert_eq!(a, b);
    }

    #[test]
    fn logical_generation_defaults_participate_in_effective_cache_identity() {
        let _guard = crate::llm::env_guard();
        crate::llm_config::clear_user_overrides();
        let mut overlay = crate::llm_config::ProvidersConfig::default();
        overlay.model_defaults.insert(
            "fireworks/accounts/fireworks/models/gpt-oss-120b".to_string(),
            std::collections::BTreeMap::from_iter([
                ("max_tokens".to_string(), toml::Value::Integer(2048)),
                ("top_k".to_string(), toml::Value::Integer(40)),
                ("frequency_penalty".to_string(), toml::Value::Float(0.2)),
                ("presence_penalty".to_string(), toml::Value::Float(0.3)),
            ]),
        );
        crate::llm_config::set_user_overrides(Some(overlay));
        let default_key = key("hello", dict(gpt_oss_options()));

        let mut explicit_same = gpt_oss_options();
        explicit_same.insert(crate::value::intern_key("max_tokens"), VmValue::Int(2048));
        explicit_same.insert(crate::value::intern_key("temperature"), VmValue::Float(1.0));
        explicit_same.insert(crate::value::intern_key("top_p"), VmValue::Float(1.0));
        explicit_same.insert(crate::value::intern_key("top_k"), VmValue::Int(40));
        explicit_same.insert(
            crate::value::intern_key("frequency_penalty"),
            VmValue::Float(0.2),
        );
        explicit_same.insert(
            crate::value::intern_key("presence_penalty"),
            VmValue::Float(0.3),
        );
        explicit_same.insert(
            crate::value::intern_key("reasoning_effort"),
            VmValue::String(arcstr::ArcStr::from("high")),
        );
        assert_eq!(default_key, key("hello", dict(explicit_same)));

        let mut low_effort = gpt_oss_options();
        low_effort.insert(
            crate::value::intern_key("reasoning_effort"),
            VmValue::String(arcstr::ArcStr::from("low")),
        );
        assert_ne!(default_key, key("hello", dict(low_effort)));

        let mut disabled = gpt_oss_options();
        disabled.insert(crate::value::intern_key("thinking"), VmValue::Bool(false));
        assert_ne!(default_key, key("hello", dict(disabled)));

        crate::llm_config::clear_user_overrides();
    }

    #[test]
    fn differing_tools_produce_different_keys() {
        let without = key("hello", dict(base_options()));
        let mut with_tools = base_options();
        with_tools.insert(
            crate::value::intern_key("tools"),
            VmValue::List(Arc::new(vec![VmValue::String(arcstr::ArcStr::from(
                "read_file",
            ))])),
        );
        let with = key("hello", dict(with_tools));
        assert_ne!(without, with, "tools must participate in the cache key");
    }

    #[test]
    fn differing_schema_produces_different_keys() {
        let without = key("hello", dict(base_options()));
        let mut with_schema = base_options();
        with_schema.insert(
            crate::value::intern_key("json_schema"),
            VmValue::String(arcstr::ArcStr::from(r#"{"type":"object"}"#)),
        );
        let with = key("hello", dict(with_schema));
        assert_ne!(without, with, "schema must participate in the cache key");
    }

    #[test]
    fn differing_stop_produces_different_keys() {
        let without = key("hello", dict(base_options()));
        let mut with_stop = base_options();
        with_stop.insert(
            crate::value::intern_key("stop"),
            VmValue::List(Arc::new(vec![VmValue::String(arcstr::ArcStr::from(
                "\n\n",
            ))])),
        );
        let with = key("hello", dict(with_stop));
        assert_ne!(without, with, "stop must participate in the cache key");
    }
}
