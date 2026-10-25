use super::*;

#[test]
fn failed_hash_refresh_preserves_all_previous_classifications() {
    let base = temp_path("failed-refresh");
    std::fs::create_dir(base.path().join("a-valid")).unwrap();
    let stored = compute_structure_hash(&base.path().join("a-valid")).unwrap();
    let mut state = MetadataState::new(base.path());
    for dir in ["a-valid", "z\0unreadable"] {
        state.set_namespace(
            dir,
            "classification",
            BTreeMap::from([("structureHash".into(), serde_json::json!(stored))]),
        );
    }
    assert_eq!(
        metadata_inspect_stale_flags(base.path(), "a-valid", &state.entries["a-valid"]),
        (false, false)
    );
    std::fs::write(base.path().join("a-valid/new.txt"), b"changed").unwrap();
    assert_ne!(
        compute_structure_hash(&base.path().join("a-valid")).unwrap(),
        stored
    );
    state.dirty = false;
    assert!(refresh_metadata_hashes(&mut state).is_err());
    assert!(!state.dirty);
    for dir in ["a-valid", "z\0unreadable"] {
        assert_eq!(
            classification_string(&state.entries[dir], "structureHash").as_deref(),
            Some(stored.as_str())
        );
    }
    let stale = metadata_stale_value(&state, base.path());
    assert!(matches!(
        stale.as_dict().unwrap()["any_stale"],
        VmValue::Bool(true)
    ));
    let VmValue::List(directories) = &stale.as_dict().unwrap()["tier1"] else {
        panic!("stale directory inventory must be a list");
    };
    assert_eq!(
        directories.iter().map(VmValue::display).collect::<Vec<_>>(),
        ["a-valid", "z\0unreadable"]
    );
    let flags = metadata_inspect_stale_flags(
        base.path(),
        "z\0unreadable",
        &state.entries["z\0unreadable"],
    );
    assert_eq!(flags, (true, true));
}

#[test]
fn retired_directories_stay_stale_without_blocking_live_refreshes() {
    for replaced_by_file in [false, true] {
        let base = temp_path("retired-refresh");
        for dir in ["gone", "live"] {
            std::fs::create_dir(base.path().join(dir)).unwrap();
        }
        let stored = compute_structure_hash(&base.path().join("live")).unwrap();
        let mut state = MetadataState::new(base.path());
        for dir in ["gone", "live"] {
            state.set_namespace(
                dir,
                "classification",
                BTreeMap::from([("structureHash".into(), serde_json::json!(stored))]),
            );
        }
        assert!(matches!(
            metadata_stale_value(&state, base.path()).as_dict().unwrap()["any_stale"],
            VmValue::Bool(false)
        ));
        std::fs::remove_dir(base.path().join("gone")).unwrap();
        if replaced_by_file {
            std::fs::write(base.path().join("gone"), b"replacement").unwrap();
        }
        std::fs::write(base.path().join("live/new.txt"), b"changed").unwrap();
        let updated = compute_structure_hash(&base.path().join("live")).unwrap();
        assert_ne!(updated, stored);
        refresh_metadata_hashes(&mut state).unwrap();
        assert_eq!(
            classification_string(&state.entries["live"], "structureHash").as_deref(),
            Some(updated.as_str())
        );
        assert_eq!(
            classification_string(&state.entries["gone"], "structureHash").as_deref(),
            Some(stored.as_str())
        );
        let stale = metadata_stale_value(&state, base.path());
        let VmValue::List(directories) = &stale.as_dict().unwrap()["tier1"] else {
            panic!("stale directory inventory must be a list");
        };
        assert_eq!(
            directories.iter().map(VmValue::display).collect::<Vec<_>>(),
            ["gone"]
        );
    }
}

#[test]
fn scan_pattern_matching_globs_and_substrings() {
    // Historical recursive-scan form: `*` crosses directory separators.
    assert!(glob_match("*.rs", "src/nested/main.rs"));
    // `**` forms match too (the previous hand-rolled matcher rejected
    // `**/*.rs` because it compared the second `*` literally).
    assert!(glob_match("**/*.rs", "src/nested/main.rs"));
    assert!(glob_match("src/**", "src/nested/main.rs"));
    assert!(!glob_match("*.toml", "src/main.rs"));
    // Wildcard-free patterns keep substring semantics.
    assert!(glob_match("nested", "src/nested/main.rs"));
    assert!(!glob_match("missing", "src/nested/main.rs"));
}

fn temp_path(name: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(&format!("harn-metadata-{name}-"))
        .tempdir()
        .expect("metadata fixture directory")
}

#[test]
fn metadata_resolve_preserves_namespace_structure() {
    let base = temp_path("resolve");
    let mut state = MetadataState::new(base.path());
    state.set_namespace(
        "",
        "classification",
        BTreeMap::from([("language".into(), serde_json::json!("rust"))]),
    );
    state.set_namespace(
        "src",
        "classification",
        BTreeMap::from([("owner".into(), serde_json::json!("vm"))]),
    );

    let resolved = state.resolve("src");
    let classification = resolved.namespaces.get("classification").unwrap();
    assert_eq!(
        classification.get("language"),
        Some(&serde_json::json!("rust"))
    );
    assert_eq!(classification.get("owner"), Some(&serde_json::json!("vm")));
}

#[test]
fn metadata_save_writes_namespace_shards() {
    let base = temp_path("save");
    let mut state = MetadataState::new(base.path());
    state.set_namespace(
        ".",
        "classification",
        BTreeMap::from([("language".into(), serde_json::json!("rust"))]),
    );
    state.set_namespace(
        "src",
        "coding-enrichment-v1",
        BTreeMap::from([("_deep_scan".into(), serde_json::json!({"version": 1}))]),
    );
    state.save().expect("save");

    let metadata_root = crate::runtime_paths::metadata_dir(base.path());
    let classification = std::fs::read_to_string(
        metadata_root
            .join("classification")
            .join(NAMESPACE_ENTRIES_FILE),
    )
    .expect("classification shard");
    let parsed = serde_json::from_str::<serde_json::Value>(&classification).expect("json");
    assert_eq!(
        parsed.get("namespace").and_then(|value| value.as_str()),
        Some("classification")
    );
    assert!(parsed
        .get("entries")
        .and_then(|value| value.get("."))
        .is_some());

    let enrichment = std::fs::read_to_string(
        metadata_root
            .join("coding-enrichment-v1")
            .join(NAMESPACE_ENTRIES_FILE),
    )
    .expect("enrichment shard");
    let parsed = serde_json::from_str::<serde_json::Value>(&enrichment).expect("json");
    assert!(parsed
        .get("entries")
        .and_then(|value| value.get("src"))
        .is_some());
}

#[test]
fn metadata_load_merges_legacy_and_namespace_shards() {
    let base = temp_path("load");
    let metadata_root = crate::runtime_paths::metadata_dir(base.path());
    std::fs::create_dir_all(metadata_root.join("facts")).unwrap();
    std::fs::write(
        metadata_root.join(LEGACY_SHARD_NAME),
        serde_json::json!({
            "version": 2,
            "entries": {
                ".": {
                    "namespaces": {
                        "classification": {
                            "language": "rust"
                        }
                    }
                }
            }
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        metadata_root.join("facts").join(NAMESPACE_ENTRIES_FILE),
        serde_json::json!({
            "version": 1,
            "namespace": "facts",
            "entries": {
                "src": {
                    "kind": "module"
                }
            }
        })
        .to_string(),
    )
    .unwrap();

    let mut state = MetadataState::new(base.path());
    state.ensure_loaded();
    assert_eq!(
        state
            .entries
            .get(".")
            .and_then(|meta| meta.namespaces.get("classification"))
            .and_then(|fields| fields.get("language")),
        Some(&serde_json::json!("rust"))
    );
    assert_eq!(
        state
            .entries
            .get("src")
            .and_then(|meta| meta.namespaces.get("facts"))
            .and_then(|fields| fields.get("kind")),
        Some(&serde_json::json!("module"))
    );
}

#[test]
fn path_metadata_file_round_trip_does_not_inherit() {
    let base = temp_path("path_file_roundtrip");
    let mut state = MetadataState::new(base.path());

    // Dir-level fact set on a parent — should not leak into file lookup.
    state.set_namespace(
        "src",
        "facts",
        BTreeMap::from([("owner".into(), serde_json::json!("vm"))]),
    );
    state.set_file_namespace(
        "src/foo.rs",
        "facts",
        BTreeMap::from([("summary".into(), serde_json::json!("entry point"))]),
    );

    let file_fields = state.file_namespace("src/foo.rs", "facts").expect("file");
    assert_eq!(
        file_fields.get("summary"),
        Some(&serde_json::json!("entry point"))
    );
    // File lookup must NOT inherit "owner" from the parent dir entry.
    assert!(!file_fields.contains_key("owner"));

    // Missing file path returns None.
    assert!(state.file_namespace("src/missing.rs", "facts").is_none());
    // Missing namespace on a known file returns None.
    assert!(state
        .file_namespace("src/foo.rs", "other_namespace")
        .is_none());
}

#[test]
fn path_metadata_persists_files_alongside_dirs() {
    let base = temp_path("path_persist");
    let mut state = MetadataState::new(base.path());
    state.set_namespace(
        ".",
        "classification",
        BTreeMap::from([("language".into(), serde_json::json!("rust"))]),
    );
    state.set_file_namespace(
        "src/foo.rs",
        "facts",
        BTreeMap::from([("summary".into(), serde_json::json!("entry point"))]),
    );
    state.set_file_namespace(
        "src/bar.rs",
        "facts",
        BTreeMap::from([("summary".into(), serde_json::json!("helpers"))]),
    );
    state.save().expect("save");

    let facts_shard = std::fs::read_to_string(
        crate::runtime_paths::metadata_dir(base.path())
            .join("facts")
            .join(NAMESPACE_ENTRIES_FILE),
    )
    .expect("facts shard");
    let parsed = serde_json::from_str::<serde_json::Value>(&facts_shard).expect("json");
    let files = parsed.get("files").and_then(|v| v.as_object()).unwrap();
    assert!(files.contains_key("src/foo.rs"));
    assert!(files.contains_key("src/bar.rs"));

    // Dir-only namespace must not write a `files` field.
    let class_shard = std::fs::read_to_string(
        crate::runtime_paths::metadata_dir(base.path())
            .join("classification")
            .join(NAMESPACE_ENTRIES_FILE),
    )
    .expect("classification shard");
    let parsed = serde_json::from_str::<serde_json::Value>(&class_shard).expect("json");
    assert!(parsed.get("files").is_none());

    // Reload from disk and verify file entries round-trip.
    let mut reloaded = MetadataState::new(base.path());
    let fields = reloaded
        .file_namespace("src/foo.rs", "facts")
        .expect("reloaded");
    assert_eq!(
        fields.get("summary"),
        Some(&serde_json::json!("entry point"))
    );
}

#[test]
fn path_metadata_load_tolerates_stale_snapshot_without_files_section() {
    let base = temp_path("path_stale");
    let metadata_root = crate::runtime_paths::metadata_dir(base.path());
    std::fs::create_dir_all(metadata_root.join("facts")).unwrap();
    // Pre-v2 shard with only `entries`, no `files` — must still load.
    std::fs::write(
        metadata_root.join("facts").join(NAMESPACE_ENTRIES_FILE),
        serde_json::json!({
            "version": 1,
            "namespace": "facts",
            "entries": {
                "src": {"kind": "module"}
            }
        })
        .to_string(),
    )
    .unwrap();

    let mut state = MetadataState::new(base.path());
    state.ensure_loaded();
    assert_eq!(
        state
            .entries
            .get("src")
            .and_then(|meta| meta.namespaces.get("facts"))
            .and_then(|f| f.get("kind")),
        Some(&serde_json::json!("module"))
    );
    assert!(state.files.is_empty());
    assert!(state.file_namespace("src/foo.rs", "facts").is_none());
}

#[test]
fn normalize_file_key_handles_common_inputs() {
    assert_eq!(normalize_file_key("src/foo.rs"), Some("src/foo.rs".into()));
    assert_eq!(
        normalize_file_key("./src/foo.rs"),
        Some("src/foo.rs".into())
    );
    assert_eq!(
        normalize_file_key("src\\nested\\foo.rs"),
        Some("src/nested/foo.rs".into())
    );
    assert_eq!(normalize_file_key("src/foo.rs/"), Some("src/foo.rs".into()));
    assert_eq!(normalize_file_key(""), None);
    assert_eq!(normalize_file_key("."), None);
    assert_eq!(normalize_file_key(".."), None);
}

#[test]
fn scan_options_filter_hidden_and_depth() {
    let base = temp_path("scan");
    std::fs::create_dir_all(base.path().join("project/deep")).unwrap();
    std::fs::write(base.path().join("project/root.txt"), "root").unwrap();
    std::fs::write(base.path().join("project/.hidden.txt"), "hidden").unwrap();
    std::fs::write(base.path().join("project/deep/nested.txt"), "nested").unwrap();

    let options = ScanOptions {
        pattern: Some(".txt".into()),
        max_depth: 0,
        include_hidden: false,
        include_dirs: false,
        include_files: true,
    };
    let mut results = Vec::new();
    let project = base.path().join("project");
    scan_dir_recursive(&project, base.path(), &options, &mut results, 0);
    let paths: Vec<String> = results
        .into_iter()
        .map(|value| match value {
            VmValue::Dict(dict) => dict.get("path").unwrap().display(),
            _ => String::new(),
        })
        .collect();
    assert_eq!(paths, vec!["project/root.txt".to_string()]);
}
