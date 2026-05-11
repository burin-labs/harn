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
