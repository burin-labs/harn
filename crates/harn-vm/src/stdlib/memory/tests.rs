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
    let hash = sha256_hex(b"hello world");
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
fn vector_cache_model_keys_are_distinct_and_confined() {
    let root = Path::new("memory-root");
    let first = vector_cache_path(root, "agent", "ns/model", "text-hash").unwrap();
    let second = vector_cache_path(root, "agent", "ns_model", "text-hash").unwrap();
    assert_ne!(first, second);
    let escape = vector_cache_path(root, "agent", "../escape", "text-hash").unwrap();
    assert!(escape.starts_with(root.join("agent").join(VECTOR_CACHE_DIR)));
    assert_eq!(escape.parent().unwrap().file_name().unwrap().len(), 64);
}

#[test]
fn parse_embedding_response_validates_dim_and_vector_types() {
    let mut dict = crate::value::DictMap::new();
    dict.put(
        "vector",
        VmValue::list(vec![
            VmValue::Float(0.1),
            VmValue::Float(0.2),
            VmValue::Int(1),
        ]),
    );
    dict.put("dim", VmValue::Int(3));
    dict.put_str("model", "test-model");
    let value = VmValue::dict(dict);
    let parsed = parse_embedding_response(value, "fallback").unwrap();
    assert_eq!(parsed.dim, 3);
    assert_eq!(parsed.model, "test-model");
    assert_eq!(parsed.vector, vec![0.1, 0.2, 1.0]);

    let mut bad = crate::value::DictMap::new();
    bad.put("vector", VmValue::list(vec![VmValue::Float(0.1)]));
    bad.put("dim", VmValue::Int(2));
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
    dict.put("a", VmValue::Bool(true));
    dict.put("b", VmValue::Bool(false));
    let from_dict = parse_flags(Some(&VmValue::dict(dict)));
    assert_eq!(from_dict.get("a"), Some(&true));
    assert_eq!(from_dict.get("b"), Some(&false));

    let list = VmValue::strings(["auto_surface"]);
    let from_list = parse_flags(Some(&list));
    assert_eq!(from_list.get("auto_surface"), Some(&true));
}
