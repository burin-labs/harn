//! Integration tests for the live workspace state added in #776.
//!
//! Covers live code-index state: agent registry + locks, the append-only
//! version log, file table accessors, cached read paths, and the snapshot
//! recovery flow. The cross-process concurrency stress test exercises
//! `agent_register/heartbeat/unregister + lock_try/release` from a
//! handful of native threads to make sure the in-process mutex serialises
//! everyone correctly.

use std::fs;
use std::sync::Arc;
use std::thread;

use harn_hostlib::{
    code_index::CodeIndexCapability, BuiltinRegistry, HostlibCapability, RegisteredBuiltin,
};
use harn_vm::VmValue;

fn build() -> (BuiltinRegistry, CodeIndexCapability) {
    let cap = CodeIndexCapability::new();
    let mut registry = BuiltinRegistry::new();
    cap.register_builtins(&mut registry);
    (registry, cap)
}

fn dict(entries: &[(&str, VmValue)]) -> VmValue {
    let mut map: harn_vm::value::DictMap = Default::default();
    for (k, v) in entries {
        map.insert(harn_vm::value::intern_key(k), v.clone());
    }
    VmValue::dict(map)
}

fn call(registry: &BuiltinRegistry, name: &str, payload: VmValue) -> VmValue {
    let entry: &RegisteredBuiltin = registry
        .find(name)
        .unwrap_or_else(|| panic!("builtin {name} not registered"));
    (entry.handler)(&[payload]).unwrap_or_else(|err| panic!("builtin {name} failed: {err:?}"))
}

fn try_call(
    registry: &BuiltinRegistry,
    name: &str,
    payload: VmValue,
) -> Result<VmValue, harn_hostlib::HostlibError> {
    let entry = registry.find(name).expect("builtin not registered");
    (entry.handler)(&[payload])
}

fn extract_dict(value: &VmValue) -> Arc<harn_vm::value::DictMap> {
    match value {
        VmValue::Dict(d) => d.clone(),
        other => panic!("expected dict, got {other:?}"),
    }
}

fn extract_list(value: &VmValue) -> Arc<Vec<VmValue>> {
    match value {
        VmValue::List(l) => l.clone(),
        other => panic!("expected list, got {other:?}"),
    }
}

fn extract_int(value: &VmValue) -> i64 {
    match value {
        VmValue::Int(n) => *n,
        other => panic!("expected int, got {other:?}"),
    }
}

fn extract_bool(value: &VmValue) -> bool {
    match value {
        VmValue::Bool(b) => *b,
        other => panic!("expected bool, got {other:?}"),
    }
}

fn extract_str(value: &VmValue) -> String {
    match value {
        VmValue::String(s) => s.to_string(),
        other => panic!("expected string, got {other:?}"),
    }
}

fn rebuild_in(dir: &std::path::Path, registry: &BuiltinRegistry) {
    call(
        registry,
        "hostlib_code_index_rebuild",
        dict(&[(
            "root",
            VmValue::String(arcstr::ArcStr::from(dir.to_string_lossy().to_string())),
        )]),
    );
}

fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src/main.ts"),
        "import { helper } from \"./util\";\nexport const x = 1;\n",
    )
    .unwrap();
    fs::write(
        root.join("src/util.ts"),
        "export function helper() { return 42; }\n",
    )
    .unwrap();
    fs::write(root.join("README.md"), "# project\n").unwrap();
    dir
}

fn fnv1a64_string(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h.to_string()
}

fn snapshot_entry(snapshot: &VmValue, path: &str) -> Arc<harn_vm::value::DictMap> {
    let snapshot = extract_dict(snapshot);
    let files = extract_list(snapshot.get("files").unwrap());
    files
        .iter()
        .map(extract_dict)
        .find(|entry| extract_str(entry.get("path").unwrap()) == path)
        .unwrap_or_else(|| panic!("snapshot entry for {path} not found"))
}

fn snapshot_hash(snapshot: &VmValue, path: &str) -> Option<String> {
    let snapshot = extract_dict(snapshot);
    let hashes = extract_dict(snapshot.get("snapshot").unwrap());
    hashes.get(path).map(extract_str)
}

// === File table accessors ===

#[test]
fn path_to_id_and_id_to_path_round_trip() {
    let dir = workspace();
    let (registry, _) = build();
    rebuild_in(dir.path(), &registry);

    let id_value = call(
        &registry,
        "hostlib_code_index_path_to_id",
        dict(&[("path", VmValue::String(arcstr::ArcStr::from("src/main.ts")))]),
    );
    let id = extract_int(&id_value);
    assert!(id >= 1);

    let path = call(
        &registry,
        "hostlib_code_index_id_to_path",
        dict(&[("file_id", VmValue::Int(id))]),
    );
    assert_eq!(extract_str(&path), "src/main.ts");

    // Unknown path → null.
    let none = call(
        &registry,
        "hostlib_code_index_path_to_id",
        dict(&[("path", VmValue::String(arcstr::ArcStr::from("not/here.rs")))]),
    );
    assert!(matches!(none, VmValue::Nil));

    // Unknown id → null.
    let none = call(
        &registry,
        "hostlib_code_index_id_to_path",
        dict(&[("file_id", VmValue::Int(99_999))]),
    );
    assert!(matches!(none, VmValue::Nil));
}

#[test]
fn file_ids_returns_sorted_ascending() {
    let dir = workspace();
    let (registry, _) = build();
    rebuild_in(dir.path(), &registry);

    let ids_value = call(&registry, "hostlib_code_index_file_ids", dict(&[]));
    let ids = extract_list(&ids_value);
    let nums: Vec<i64> = ids.iter().map(extract_int).collect();
    assert!(!nums.is_empty());
    let mut sorted = nums.clone();
    sorted.sort_unstable();
    assert_eq!(nums, sorted);
}

#[test]
fn file_meta_returns_metadata_for_path_and_id() {
    let dir = workspace();
    let (registry, _) = build();
    rebuild_in(dir.path(), &registry);

    let by_path = call(
        &registry,
        "hostlib_code_index_file_meta",
        dict(&[("path", VmValue::String(arcstr::ArcStr::from("src/util.ts")))]),
    );
    let m = extract_dict(&by_path);
    assert_eq!(extract_str(m.get("path").unwrap()), "src/util.ts");
    assert_eq!(extract_str(m.get("language").unwrap()), "typescript");
    assert!(extract_int(m.get("size").unwrap()) > 0);
    assert!(extract_int(m.get("line_count").unwrap()) >= 1);
    assert!(!extract_str(m.get("hash").unwrap()).is_empty());

    let id = extract_int(m.get("id").unwrap());
    let by_id = call(
        &registry,
        "hostlib_code_index_file_meta",
        dict(&[("file_id", VmValue::Int(id))]),
    );
    let m2 = extract_dict(&by_id);
    assert_eq!(
        extract_str(m.get("hash").unwrap()),
        extract_str(m2.get("hash").unwrap())
    );

    // Unknown path → null.
    let nil = call(
        &registry,
        "hostlib_code_index_file_meta",
        dict(&[("path", VmValue::String(arcstr::ArcStr::from("ghost.rs")))]),
    );
    assert!(matches!(nil, VmValue::Nil));
}

#[test]
fn file_hash_reads_the_file_off_disk() {
    let dir = workspace();
    let (registry, _) = build();
    rebuild_in(dir.path(), &registry);

    let h = call(
        &registry,
        "hostlib_code_index_file_hash",
        dict(&[("path", VmValue::String(arcstr::ArcStr::from("README.md")))]),
    );
    let s = extract_str(&h);
    // FNV-1a of `# project\n` is deterministic; pre-computed by hand
    // FNV-1a reference value for `hello world`.
    let expected: u64 = {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in b"# project\n" {
            h ^= *b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        h
    };
    assert_eq!(s, expected.to_string());
}

#[test]
fn file_hash_snapshot_batches_current_hashes_and_seq_binding() {
    let dir = workspace();
    let (registry, _) = build();
    rebuild_in(dir.path(), &registry);
    fs::write(dir.path().join("src/new.ts"), "export const y = 2;\n").unwrap();

    let initial = call(
        &registry,
        "hostlib_code_index_file_hash_snapshot",
        dict(&[(
            "paths",
            VmValue::List(Arc::new(vec![
                VmValue::string("README.md"),
                VmValue::string("src/new.ts"),
                VmValue::string("src/util.ts"),
            ])),
        )]),
    );
    let initial_dict = extract_dict(&initial);
    assert_eq!(extract_int(initial_dict.get("seq").unwrap()), 0);
    assert_eq!(
        extract_str(initial_dict.get("algorithm").unwrap()),
        "fnv1a64"
    );
    assert!(extract_list(initial_dict.get("missing").unwrap()).is_empty());

    let readme = snapshot_entry(&initial, "README.md");
    assert!(extract_bool(readme.get("known").unwrap()));
    assert!(extract_bool(readme.get("readable").unwrap()));
    assert_eq!(extract_str(readme.get("hash_source").unwrap()), "indexed");
    assert_eq!(
        extract_str(readme.get("hash").unwrap()),
        fnv1a64_string(b"# project\n")
    );
    assert_eq!(
        snapshot_hash(&initial, "README.md").unwrap(),
        fnv1a64_string(b"# project\n")
    );
    assert_eq!(
        extract_str(readme.get("indexed_hash").unwrap()),
        fnv1a64_string(b"# project\n")
    );
    assert_eq!(extract_int(readme.get("last_edit_seq").unwrap()), 0);

    let new_file = snapshot_entry(&initial, "src/new.ts");
    assert!(!extract_bool(new_file.get("known").unwrap()));
    assert!(extract_bool(new_file.get("readable").unwrap()));
    assert_eq!(extract_str(new_file.get("hash_source").unwrap()), "disk");
    assert_eq!(
        extract_str(new_file.get("hash").unwrap()),
        fnv1a64_string(b"export const y = 2;\n")
    );
    assert_eq!(
        snapshot_hash(&initial, "src/new.ts").unwrap(),
        fnv1a64_string(b"export const y = 2;\n")
    );
    assert!(matches!(
        new_file.get("indexed_hash").unwrap(),
        VmValue::Nil
    ));

    let util = snapshot_entry(&initial, "src/util.ts");
    let util_indexed_hash = extract_str(util.get("hash").unwrap());
    let changed_util_source = "export function helper() { return 4300; }\n";
    fs::write(dir.path().join("src/util.ts"), changed_util_source).unwrap();
    let changed = call(
        &registry,
        "hostlib_code_index_file_hash_snapshot",
        dict(&[(
            "paths",
            VmValue::List(Arc::new(vec![VmValue::string("src/util.ts")])),
        )]),
    );
    let changed_util = snapshot_entry(&changed, "src/util.ts");
    let changed_hash = extract_str(changed_util.get("hash").unwrap());
    assert_eq!(
        extract_str(changed_util.get("hash_source").unwrap()),
        "disk"
    );
    assert_ne!(changed_hash, util_indexed_hash);
    assert_eq!(
        extract_str(changed_util.get("indexed_hash").unwrap()),
        util_indexed_hash
    );

    let agent_id = extract_int(&call(
        &registry,
        "hostlib_code_index_agent_register",
        dict(&[("name", VmValue::string("verification"))]),
    ));
    let seq = extract_int(&call(
        &registry,
        "hostlib_code_index_version_record",
        dict(&[
            ("agent_id", VmValue::Int(agent_id)),
            ("path", VmValue::string("src/util.ts")),
            ("op", VmValue::string("write")),
            ("hash", VmValue::string(&changed_hash)),
            ("size", VmValue::Int(changed_util_source.len() as i64)),
        ]),
    ));
    let bound = call(
        &registry,
        "hostlib_code_index_file_hash_snapshot",
        dict(&[(
            "paths",
            VmValue::List(Arc::new(vec![VmValue::string("src/util.ts")])),
        )]),
    );
    let bound_dict = extract_dict(&bound);
    assert_eq!(extract_int(bound_dict.get("seq").unwrap()), seq);
    let bound_util = snapshot_entry(&bound, "src/util.ts");
    assert_eq!(extract_int(bound_util.get("last_edit_seq").unwrap()), seq);
    assert_eq!(extract_str(bound_util.get("hash").unwrap()), changed_hash);
    assert_eq!(snapshot_hash(&bound, "src/util.ts").unwrap(), changed_hash);

    let changes = call(
        &registry,
        "hostlib_code_index_changes_since",
        dict(&[("seq", VmValue::Int(0))]),
    );
    let records = extract_list(&changes);
    let util_record = records
        .iter()
        .map(extract_dict)
        .find(|record| extract_str(record.get("path").unwrap()) == "src/util.ts")
        .expect("version log records util edit");
    assert_eq!(extract_int(util_record.get("seq").unwrap()), seq);
    assert_eq!(extract_str(util_record.get("hash").unwrap()), changed_hash);
}

#[test]
fn file_hash_rejects_absolute_paths_outside_workspace() {
    let dir = workspace();
    let outside = tempfile::NamedTempFile::new().unwrap();
    fs::write(outside.path(), "outside workspace\n").unwrap();
    let (registry, _) = build();
    rebuild_in(dir.path(), &registry);

    let value = call(
        &registry,
        "hostlib_code_index_file_hash",
        dict(&[(
            "path",
            VmValue::String(arcstr::ArcStr::from(
                outside.path().to_string_lossy().to_string(),
            )),
        )]),
    );

    assert!(matches!(value, VmValue::Nil));
}

// === Cached reads ===

#[test]
fn read_range_returns_full_or_sliced_content() {
    let dir = workspace();
    let (registry, _) = build();
    rebuild_in(dir.path(), &registry);

    let full = call(
        &registry,
        "hostlib_code_index_read_range",
        dict(&[("path", VmValue::String(arcstr::ArcStr::from("src/util.ts")))]),
    );
    let full = extract_dict(&full);
    let body = extract_str(full.get("content").unwrap());
    assert!(body.contains("helper"));

    let sliced = call(
        &registry,
        "hostlib_code_index_read_range",
        dict(&[
            ("path", VmValue::String(arcstr::ArcStr::from("src/util.ts"))),
            ("start", VmValue::Int(1)),
            ("end", VmValue::Int(1)),
        ]),
    );
    let sliced = extract_dict(&sliced);
    let line = extract_str(sliced.get("content").unwrap());
    assert!(line.contains("export"));
    assert_eq!(extract_int(sliced.get("start").unwrap()), 1);
    assert_eq!(extract_int(sliced.get("end").unwrap()), 1);
}

#[test]
fn read_range_errors_when_file_missing() {
    let dir = workspace();
    let (registry, _) = build();
    rebuild_in(dir.path(), &registry);

    let err = try_call(
        &registry,
        "hostlib_code_index_read_range",
        dict(&[(
            "path",
            VmValue::String(arcstr::ArcStr::from("not/a/real/file.txt")),
        )]),
    )
    .expect_err("missing file should error");
    let msg = format!("{err}");
    assert!(msg.contains("file not found"));
}

#[test]
fn read_range_rejects_paths_outside_workspace() {
    let dir = workspace();
    let outside = tempfile::NamedTempFile::new().unwrap();
    fs::write(outside.path(), "outside workspace\n").unwrap();
    let (registry, _) = build();
    rebuild_in(dir.path(), &registry);

    let err = try_call(
        &registry,
        "hostlib_code_index_read_range",
        dict(&[(
            "path",
            VmValue::String(arcstr::ArcStr::from(
                outside.path().to_string_lossy().to_string(),
            )),
        )]),
    )
    .expect_err("outside workspace path should error");
    let msg = format!("{err}");
    assert!(msg.contains("indexed workspace root"));
}

#[test]
fn reindex_file_picks_up_changes_via_builtin() {
    let dir = workspace();
    let (registry, _) = build();
    rebuild_in(dir.path(), &registry);

    let path = "src/util.ts";
    let id_before = extract_int(&call(
        &registry,
        "hostlib_code_index_path_to_id",
        dict(&[("path", VmValue::String(arcstr::ArcStr::from(path)))]),
    ));

    fs::write(
        dir.path().join(path),
        "export const ZetaToken = \"refreshed\";\nexport function helper() { return 0; }\n",
    )
    .unwrap();
    let res = call(
        &registry,
        "hostlib_code_index_reindex_file",
        dict(&[("path", VmValue::String(arcstr::ArcStr::from(path)))]),
    );
    let res = extract_dict(&res);
    assert!(extract_bool(res.get("indexed").unwrap()));
    let id_after = extract_int(res.get("file_id").unwrap());
    assert_eq!(id_before, id_after);

    // Trigram index reflects the new content.
    let q = call(
        &registry,
        "hostlib_code_index_query",
        dict(&[("needle", VmValue::String(arcstr::ArcStr::from("ZetaToken")))]),
    );
    let q = extract_dict(&q);
    let results = extract_list(q.get("results").unwrap());
    let paths: Vec<String> = results
        .iter()
        .map(|hit| {
            let d = extract_dict(hit);
            extract_str(d.get("path").unwrap())
        })
        .collect();
    assert!(paths.contains(&path.to_string()));
}

#[test]
fn reindex_file_drops_entries_when_file_disappears() {
    let dir = workspace();
    let (registry, _) = build();
    rebuild_in(dir.path(), &registry);

    fs::remove_file(dir.path().join("src/util.ts")).unwrap();
    let res = call(
        &registry,
        "hostlib_code_index_reindex_file",
        dict(&[("path", VmValue::String(arcstr::ArcStr::from("src/util.ts")))]),
    );
    let res = extract_dict(&res);
    assert!(!extract_bool(res.get("indexed").unwrap()));
    assert!(matches!(res.get("file_id").unwrap(), VmValue::Nil));

    let id = call(
        &registry,
        "hostlib_code_index_path_to_id",
        dict(&[("path", VmValue::String(arcstr::ArcStr::from("src/util.ts")))]),
    );
    assert!(matches!(id, VmValue::Nil));
}

#[test]
fn extract_trigrams_matches_indexer() {
    let (registry, _) = build();
    let result = call(
        &registry,
        "hostlib_code_index_extract_trigrams",
        dict(&[("query", VmValue::String(arcstr::ArcStr::from("foo")))]),
    );
    let list = extract_list(&result);
    assert_eq!(list.len(), 1);
    // Packed (a << 16) | (b << 8) | c with ASCII case-fold:
    // 'f' = 0x66, 'o' = 0x6f, 'o' = 0x6f -> 0x666f6f.
    assert_eq!(extract_int(&list[0]), 0x66_6f_6f);
}

#[test]
fn trigram_query_intersects_postings() {
    let dir = workspace();
    let (registry, _) = build();
    rebuild_in(dir.path(), &registry);

    let trigrams = call(
        &registry,
        "hostlib_code_index_extract_trigrams",
        dict(&[("query", VmValue::String(arcstr::ArcStr::from("helper")))]),
    );
    let result = call(
        &registry,
        "hostlib_code_index_trigram_query",
        dict(&[("trigrams", trigrams)]),
    );
    let list = extract_list(&result);
    assert!(!list.is_empty());
}

#[test]
fn word_get_returns_per_line_hits() {
    let dir = workspace();
    let (registry, _) = build();
    rebuild_in(dir.path(), &registry);

    let hits = call(
        &registry,
        "hostlib_code_index_word_get",
        dict(&[("word", VmValue::String(arcstr::ArcStr::from("helper")))]),
    );
    let list = extract_list(&hits);
    assert!(list.iter().all(|h| {
        let d = extract_dict(h);
        d.get("file_id")
            .is_some_and(|v| matches!(v, VmValue::Int(_)))
            && d.get("line").is_some_and(|v| matches!(v, VmValue::Int(_)))
    }));
}

#[test]
fn deps_get_returns_neighbours() {
    let dir = workspace();
    let (registry, _) = build();
    rebuild_in(dir.path(), &registry);

    let main_id = extract_int(&call(
        &registry,
        "hostlib_code_index_path_to_id",
        dict(&[("path", VmValue::String(arcstr::ArcStr::from("src/main.ts")))]),
    ));
    let util_id = extract_int(&call(
        &registry,
        "hostlib_code_index_path_to_id",
        dict(&[("path", VmValue::String(arcstr::ArcStr::from("src/util.ts")))]),
    ));

    let imports_of_main = call(
        &registry,
        "hostlib_code_index_deps_get",
        dict(&[
            ("file_id", VmValue::Int(main_id)),
            (
                "direction",
                VmValue::String(arcstr::ArcStr::from("imports")),
            ),
        ]),
    );
    let imp_list = extract_list(&imports_of_main);
    assert!(imp_list.iter().any(|v| extract_int(v) == util_id));

    let importers_of_util = call(
        &registry,
        "hostlib_code_index_deps_get",
        dict(&[
            ("file_id", VmValue::Int(util_id)),
            (
                "direction",
                VmValue::String(arcstr::ArcStr::from("importers")),
            ),
        ]),
    );
    let importers = extract_list(&importers_of_util);
    assert!(importers.iter().any(|v| extract_int(v) == main_id));
}

#[test]
fn outline_get_returns_empty_for_unknown_id() {
    let dir = workspace();
    let (registry, _) = build();
    rebuild_in(dir.path(), &registry);

    let outline = call(
        &registry,
        "hostlib_code_index_outline_get",
        dict(&[("file_id", VmValue::Int(99_999))]),
    );
    assert!(extract_list(&outline).is_empty());
}

#[test]
fn outline_get_populates_symbols_after_rebuild() {
    // Issue #2456: rebuild used to leave `IndexedFile::symbols` empty
    // even though the typed symbol graph did parse the file. Now the
    // same parse populates both; `outline_get` must surface the
    // resulting flat outline.
    let dir = workspace();
    let (registry, _) = build();
    rebuild_in(dir.path(), &registry);

    let util_id = extract_int(&call(
        &registry,
        "hostlib_code_index_path_to_id",
        dict(&[("path", VmValue::String(arcstr::ArcStr::from("src/util.ts")))]),
    ));
    let outline = call(
        &registry,
        "hostlib_code_index_outline_get",
        dict(&[("file_id", VmValue::Int(util_id))]),
    );
    let entries = extract_list(&outline);
    assert!(
        !entries.is_empty(),
        "outline_get should return at least one symbol for src/util.ts after rebuild"
    );
    let helper = entries
        .iter()
        .find(|v| {
            let d = extract_dict(v);
            matches!(d.get("name"), Some(VmValue::String(s)) if s.as_str() == "helper")
        })
        .expect("expected a `helper` symbol in src/util.ts");
    let helper_dict = extract_dict(helper);
    let kind = match helper_dict.get("kind") {
        Some(VmValue::String(s)) => s.to_string(),
        other => panic!("expected string kind, got {other:?}"),
    };
    assert_eq!(kind, "function", "`helper` should be a function symbol");
    assert!(
        matches!(helper_dict.get("access_level"), Some(VmValue::Nil)),
        "function without known access should expose access_level:null"
    );
    let start_line = extract_int(helper_dict.get("start_line").expect("start_line"));
    assert!(
        start_line >= 1,
        "start_line should be 1-based, got {start_line}"
    );
}

// === Change log ===

#[test]
fn version_record_then_changes_since_round_trips() {
    let dir = workspace();
    let (registry, cap) = build();
    rebuild_in(dir.path(), &registry);

    // Register an agent so the registry has a record for it.
    let agent_id = extract_int(&call(
        &registry,
        "hostlib_code_index_agent_register",
        dict(&[("name", VmValue::String(arcstr::ArcStr::from("editor")))]),
    ));

    let seq1 = extract_int(&call(
        &registry,
        "hostlib_code_index_version_record",
        dict(&[
            ("agent_id", VmValue::Int(agent_id)),
            ("path", VmValue::String(arcstr::ArcStr::from("src/util.ts"))),
            ("op", VmValue::String(arcstr::ArcStr::from("write"))),
            ("hash", VmValue::String(arcstr::ArcStr::from("12345"))),
            ("size", VmValue::Int(42)),
        ]),
    ));
    let seq2 = extract_int(&call(
        &registry,
        "hostlib_code_index_version_record",
        dict(&[
            ("agent_id", VmValue::Int(agent_id)),
            ("path", VmValue::String(arcstr::ArcStr::from("src/main.ts"))),
            ("op", VmValue::String(arcstr::ArcStr::from("patch"))),
            ("hash", VmValue::Int(99)),
        ]),
    ));
    assert!(seq2 > seq1);
    let current = extract_int(&call(
        &registry,
        "hostlib_code_index_current_seq",
        dict(&[]),
    ));
    assert_eq!(current, seq2);

    let changes = call(
        &registry,
        "hostlib_code_index_changes_since",
        dict(&[("seq", VmValue::Int(0))]),
    );
    let changes = extract_list(&changes);
    assert_eq!(changes.len(), 2);
    let first = extract_dict(&changes[0]);
    let second = extract_dict(&changes[1]);
    assert_eq!(extract_int(first.get("seq").unwrap()), seq1);
    assert_eq!(extract_int(second.get("seq").unwrap()), seq2);
    assert_eq!(extract_str(first.get("op").unwrap()), "write");
    assert_eq!(extract_str(second.get("op").unwrap()), "patch");

    // The registry's `note_edit` should have bumped the agent's edit
    // counter — surfaced via `status`.
    let status = call(&registry, "hostlib_code_index_status", dict(&[]));
    let status = extract_dict(&status);
    let agents = extract_list(status.get("agents").unwrap());
    let me = agents
        .iter()
        .find(|a| extract_int(extract_dict(a).get("id").unwrap()) == agent_id)
        .expect("registered agent appears in status");
    let me = extract_dict(me);
    assert_eq!(extract_int(me.get("edit_count").unwrap()), 2);
    let _ = cap;
}

#[test]
fn changes_since_respects_limit() {
    let dir = workspace();
    let (registry, _) = build();
    rebuild_in(dir.path(), &registry);
    let agent_id = 1_i64;
    for i in 0..5 {
        call(
            &registry,
            "hostlib_code_index_version_record",
            dict(&[
                ("agent_id", VmValue::Int(agent_id)),
                (
                    "path",
                    VmValue::String(arcstr::ArcStr::from(format!("f{i}.rs"))),
                ),
            ]),
        );
    }
    let limited = call(
        &registry,
        "hostlib_code_index_changes_since",
        dict(&[("seq", VmValue::Int(0)), ("limit", VmValue::Int(2))]),
    );
    assert_eq!(extract_list(&limited).len(), 2);
}

#[test]
fn code_index_rejects_schema_invalid_numeric_bounds() {
    let dir = workspace();
    let (registry, _) = build();
    rebuild_in(dir.path(), &registry);

    let cases = [
        (
            "hostlib_code_index_id_to_path",
            dict(&[("file_id", VmValue::Int(0))]),
            "file_id",
        ),
        (
            "hostlib_code_index_query",
            dict(&[
                ("needle", VmValue::String(arcstr::ArcStr::from("main"))),
                ("max_results", VmValue::Int(0)),
            ]),
            "max_results",
        ),
        (
            "hostlib_code_index_read_range",
            dict(&[
                ("path", VmValue::String(arcstr::ArcStr::from("src/main.ts"))),
                ("start", VmValue::Int(0)),
            ]),
            "start",
        ),
        (
            "hostlib_code_index_file_meta",
            dict(&[("file_id", VmValue::Int(0))]),
            "file_id",
        ),
        (
            "hostlib_code_index_trigram_query",
            dict(&[("trigrams", VmValue::List(Arc::new(vec![VmValue::Int(-1)])))]),
            "trigrams",
        ),
        (
            "hostlib_code_index_trigram_query",
            dict(&[
                ("trigrams", VmValue::List(Arc::new(Vec::new()))),
                ("max_files", VmValue::Int(0)),
            ]),
            "max_files",
        ),
        (
            "hostlib_code_index_changes_since",
            dict(&[("seq", VmValue::Int(-1))]),
            "seq",
        ),
        (
            "hostlib_code_index_changes_since",
            dict(&[("limit", VmValue::Int(0))]),
            "limit",
        ),
        (
            "hostlib_code_index_version_record",
            dict(&[
                ("agent_id", VmValue::Int(-1)),
                ("path", VmValue::String(arcstr::ArcStr::from("src/main.ts"))),
            ]),
            "agent_id",
        ),
        (
            "hostlib_code_index_version_record",
            dict(&[
                ("agent_id", VmValue::Int(1)),
                ("path", VmValue::String(arcstr::ArcStr::from("src/main.ts"))),
                ("hash", VmValue::Int(-1)),
            ]),
            "hash",
        ),
        (
            "hostlib_code_index_agent_register",
            dict(&[("agent_id", VmValue::Int(0))]),
            "agent_id",
        ),
        (
            "hostlib_code_index_lock_try",
            dict(&[
                ("agent_id", VmValue::Int(1)),
                ("path", VmValue::String(arcstr::ArcStr::from("src/main.ts"))),
                ("ttl_ms", VmValue::Int(0)),
            ]),
            "ttl_ms",
        ),
    ];

    for (name, payload, expected_param) in cases {
        let err = match try_call(&registry, name, payload) {
            Ok(value) => panic!("{name} accepted invalid {expected_param}: {value:?}"),
            Err(error) => error,
        };
        match err {
            harn_hostlib::HostlibError::InvalidParameter { param, .. } => {
                assert_eq!(param, expected_param, "{name}");
            }
            other => panic!("{name} returned wrong error for {expected_param}: {other:?}"),
        }
    }
}

// === Agent registry + locks ===

#[test]
fn agent_register_with_explicit_id_round_trips_through_status() {
    let dir = workspace();
    let (registry, _) = build();
    rebuild_in(dir.path(), &registry);

    let id = extract_int(&call(
        &registry,
        "hostlib_code_index_agent_register",
        dict(&[
            ("name", VmValue::String(arcstr::ArcStr::from("daemon"))),
            ("agent_id", VmValue::Int(42)),
        ]),
    ));
    assert_eq!(id, 42);

    let status = call(&registry, "hostlib_code_index_status", dict(&[]));
    let status = extract_dict(&status);
    let agents = extract_list(status.get("agents").unwrap());
    let me = agents
        .iter()
        .find(|a| extract_int(extract_dict(a).get("id").unwrap()) == 42)
        .expect("agent surfaces in status");
    let d = extract_dict(me);
    assert_eq!(extract_str(d.get("name").unwrap()), "daemon");
    assert_eq!(extract_str(d.get("state").unwrap()), "active");
}

#[test]
fn lock_try_returns_holder_when_blocked() {
    let dir = workspace();
    let (registry, _) = build();
    rebuild_in(dir.path(), &registry);

    let _alice = call(
        &registry,
        "hostlib_code_index_agent_register",
        dict(&[
            ("name", VmValue::String(arcstr::ArcStr::from("alice"))),
            ("agent_id", VmValue::Int(1)),
        ]),
    );
    let _bob = call(
        &registry,
        "hostlib_code_index_agent_register",
        dict(&[
            ("name", VmValue::String(arcstr::ArcStr::from("bob"))),
            ("agent_id", VmValue::Int(2)),
        ]),
    );

    let alice_grab = call(
        &registry,
        "hostlib_code_index_lock_try",
        dict(&[
            ("agent_id", VmValue::Int(1)),
            ("path", VmValue::String(arcstr::ArcStr::from("src/main.ts"))),
            ("ttl_ms", VmValue::Int(60_000)),
        ]),
    );
    let alice_grab = extract_dict(&alice_grab);
    assert!(extract_bool(alice_grab.get("locked").unwrap()));
    assert_eq!(extract_int(alice_grab.get("holder").unwrap()), 1);

    let bob_grab = call(
        &registry,
        "hostlib_code_index_lock_try",
        dict(&[
            ("agent_id", VmValue::Int(2)),
            ("path", VmValue::String(arcstr::ArcStr::from("src/main.ts"))),
        ]),
    );
    let bob_grab = extract_dict(&bob_grab);
    assert!(!extract_bool(bob_grab.get("locked").unwrap()));
    assert_eq!(extract_int(bob_grab.get("holder").unwrap()), 1);

    // Alice releases — Bob can grab.
    let release = call(
        &registry,
        "hostlib_code_index_lock_release",
        dict(&[
            ("agent_id", VmValue::Int(1)),
            ("path", VmValue::String(arcstr::ArcStr::from("src/main.ts"))),
        ]),
    );
    assert!(matches!(release, VmValue::Bool(true)));
    let bob_again = call(
        &registry,
        "hostlib_code_index_lock_try",
        dict(&[
            ("agent_id", VmValue::Int(2)),
            ("path", VmValue::String(arcstr::ArcStr::from("src/main.ts"))),
        ]),
    );
    let bob_again = extract_dict(&bob_again);
    assert!(extract_bool(bob_again.get("locked").unwrap()));
}

#[test]
fn agent_unregister_removes_from_status() {
    let dir = workspace();
    let (registry, _) = build();
    rebuild_in(dir.path(), &registry);
    let id = extract_int(&call(
        &registry,
        "hostlib_code_index_agent_register",
        dict(&[("name", VmValue::String(arcstr::ArcStr::from("worker")))]),
    ));
    call(
        &registry,
        "hostlib_code_index_agent_heartbeat",
        dict(&[("agent_id", VmValue::Int(id))]),
    );
    call(
        &registry,
        "hostlib_code_index_agent_unregister",
        dict(&[("agent_id", VmValue::Int(id))]),
    );
    let status = extract_dict(&call(&registry, "hostlib_code_index_status", dict(&[])));
    let agents = extract_list(status.get("agents").unwrap());
    assert!(agents
        .iter()
        .all(|a| { extract_int(extract_dict(a).get("id").unwrap()) != id }));
}

#[test]
fn current_agent_id_reads_capability_slot() {
    let (registry, cap) = build();
    let initial = call(&registry, "hostlib_code_index_current_agent_id", dict(&[]));
    assert!(matches!(initial, VmValue::Nil));

    cap.set_current_agent(Some(7));
    let bound = call(&registry, "hostlib_code_index_current_agent_id", dict(&[]));
    assert_eq!(extract_int(&bound), 7);

    cap.set_current_agent(None);
    let cleared = call(&registry, "hostlib_code_index_current_agent_id", dict(&[]));
    assert!(matches!(cleared, VmValue::Nil));
}

// === Snapshot recovery ===

#[test]
fn persist_and_restore_round_trips_state() {
    let dir = workspace();
    let (registry_a, cap_a) = build();
    rebuild_in(dir.path(), &registry_a);

    let agent_id = extract_int(&call(
        &registry_a,
        "hostlib_code_index_agent_register",
        dict(&[("name", VmValue::String(arcstr::ArcStr::from("editor")))]),
    ));
    call(
        &registry_a,
        "hostlib_code_index_version_record",
        dict(&[
            ("agent_id", VmValue::Int(agent_id)),
            ("path", VmValue::String(arcstr::ArcStr::from("src/util.ts"))),
            ("op", VmValue::String(arcstr::ArcStr::from("write"))),
        ]),
    );
    cap_a.persist_to_disk().expect("snapshot saved");

    // Fresh capability — restore from disk.
    let cap_b = CodeIndexCapability::new();
    let mut registry_b = BuiltinRegistry::new();
    cap_b.register_builtins(&mut registry_b);
    let restored = cap_b
        .restore_from_disk(dir.path())
        .expect("snapshot loadable");
    assert!(restored);

    let seq = extract_int(&call(
        &registry_b,
        "hostlib_code_index_current_seq",
        dict(&[]),
    ));
    assert!(seq >= 1);

    let id = extract_int(&call(
        &registry_b,
        "hostlib_code_index_path_to_id",
        dict(&[("path", VmValue::String(arcstr::ArcStr::from("src/util.ts")))]),
    ));
    assert!(id >= 1);

    // Snapshot didn't capture an "active" agent — recovery should have
    // either kept the agent (if young enough) or downgraded it. Either
    // way the entry should still exist and be addressable.
    let status = extract_dict(&call(&registry_b, "hostlib_code_index_status", dict(&[])));
    let agents = extract_list(status.get("agents").unwrap());
    assert!(agents
        .iter()
        .any(|a| { extract_int(extract_dict(a).get("id").unwrap()) == agent_id }));
}

#[test]
fn restore_from_disk_returns_false_when_no_snapshot_exists() {
    let dir = tempfile::tempdir().unwrap();
    let cap = CodeIndexCapability::new();
    assert!(!cap.restore_from_disk(dir.path()).unwrap());
}

// === Concurrency stress ===

#[test]
fn concurrent_agents_register_heartbeat_lock_release_does_not_corrupt_state() {
    let dir = workspace();
    let cap = CodeIndexCapability::new();
    let mut registry = BuiltinRegistry::new();
    cap.register_builtins(&mut registry);
    rebuild_in(dir.path(), &registry);

    // Run a handful of native threads through register/heartbeat/lock_try
    // /lock_release/unregister cycles. Build per-thread registry handles
    // — they all share the same SharedIndex via the cloned capability.
    const THREADS: u64 = 8;
    const ITERATIONS: u64 = 100;
    let registry = Arc::new(registry);

    let mut handles = Vec::new();
    for thread_idx in 0..THREADS {
        let r = registry.clone();
        handles.push(thread::spawn(move || {
            let agent_id = thread_idx + 1;
            let path = "src/main.ts".to_string();
            // Register up front under an explicit id so we can unregister
            // cleanly at the end.
            let entry = r.find("hostlib_code_index_agent_register").unwrap();
            (entry.handler)(&[dict(&[
                (
                    "name",
                    VmValue::String(arcstr::ArcStr::from(format!("worker-{thread_idx}"))),
                ),
                ("agent_id", VmValue::Int(agent_id as i64)),
            ])])
            .expect("register");

            for _ in 0..ITERATIONS {
                let h = r.find("hostlib_code_index_agent_heartbeat").unwrap();
                (h.handler)(&[dict(&[("agent_id", VmValue::Int(agent_id as i64))])])
                    .expect("heartbeat");

                let lt = r.find("hostlib_code_index_lock_try").unwrap();
                let lr = r.find("hostlib_code_index_lock_release").unwrap();
                let _ = (lt.handler)(&[dict(&[
                    ("agent_id", VmValue::Int(agent_id as i64)),
                    ("path", VmValue::String(arcstr::ArcStr::from(path.clone()))),
                    ("ttl_ms", VmValue::Int(50)),
                ])]);
                let _ = (lr.handler)(&[dict(&[
                    ("agent_id", VmValue::Int(agent_id as i64)),
                    ("path", VmValue::String(arcstr::ArcStr::from(path.clone()))),
                ])]);
            }

            let u = r.find("hostlib_code_index_agent_unregister").unwrap();
            (u.handler)(&[dict(&[("agent_id", VmValue::Int(agent_id as i64))])])
                .expect("unregister");
        }));
    }

    for h in handles {
        h.join().expect("thread joined cleanly");
    }

    // After every thread unregisters, status should report no live agents
    // and the lock must not be held.
    let status = extract_dict(&call(
        registry.as_ref(),
        "hostlib_code_index_status",
        dict(&[]),
    ));
    let agents = extract_list(status.get("agents").unwrap());
    assert!(
        agents.is_empty(),
        "no agents should remain registered, got {agents:?}"
    );

    // Acquiring a fresh lock now should succeed since the file is free.
    let ok = call(
        registry.as_ref(),
        "hostlib_code_index_lock_try",
        dict(&[
            ("agent_id", VmValue::Int(999)),
            ("path", VmValue::String(arcstr::ArcStr::from("src/main.ts"))),
            ("ttl_ms", VmValue::Int(60_000)),
        ]),
    );
    let ok = extract_dict(&ok);
    assert!(extract_bool(ok.get("locked").unwrap()));
}

#[test]
fn concurrent_version_record_assigns_unique_seqs() {
    let dir = workspace();
    let cap = CodeIndexCapability::new();
    let mut registry = BuiltinRegistry::new();
    cap.register_builtins(&mut registry);
    rebuild_in(dir.path(), &registry);

    const THREADS: u64 = 8;
    const ITERATIONS: u64 = 50;
    let registry = Arc::new(registry);

    let mut handles = Vec::new();
    for thread_idx in 0..THREADS {
        let r = registry.clone();
        handles.push(thread::spawn(move || {
            let entry = r.find("hostlib_code_index_version_record").unwrap();
            let mut seqs = Vec::with_capacity(ITERATIONS as usize);
            for i in 0..ITERATIONS {
                let path = format!("src/f{thread_idx}_{i}.rs");
                let value = (entry.handler)(&[dict(&[
                    ("agent_id", VmValue::Int(thread_idx as i64 + 1)),
                    ("path", VmValue::String(arcstr::ArcStr::from(path))),
                    ("op", VmValue::String(arcstr::ArcStr::from("write"))),
                ])])
                .expect("version_record");
                seqs.push(extract_int(&value));
            }
            seqs
        }));
    }

    let mut all_seqs: Vec<i64> = Vec::new();
    for h in handles {
        all_seqs.extend(h.join().unwrap());
    }
    let unique: std::collections::HashSet<_> = all_seqs.iter().collect();
    assert_eq!(
        all_seqs.len(),
        unique.len(),
        "every version_record call must produce a unique seq"
    );
    let current = extract_int(&call(
        registry.as_ref(),
        "hostlib_code_index_current_seq",
        dict(&[]),
    ));
    assert_eq!(current, all_seqs.iter().copied().max().unwrap_or(0));
}
