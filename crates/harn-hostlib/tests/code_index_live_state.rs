//! Integration tests for the live workspace state added in #776.
//!
//! Covers everything the burin-code Swift bridge previously round-tripped
//! through `BurinCodeIndex/`: agent registry + locks, the append-only
//! version log, file table accessors, cached read paths, and the snapshot
//! recovery flow. The cross-process concurrency stress test exercises
//! `agent_register/heartbeat/unregister + lock_try/release` from a
//! handful of native threads to make sure the in-process mutex serialises
//! everyone correctly.

use std::collections::BTreeMap;
use std::fs;
use std::rc::Rc;
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
    let mut map: BTreeMap<String, VmValue> = BTreeMap::new();
    for (k, v) in entries {
        map.insert((*k).to_string(), v.clone());
    }
    VmValue::Dict(Rc::new(map))
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

fn extract_dict(value: &VmValue) -> Rc<BTreeMap<String, VmValue>> {
    match value {
        VmValue::Dict(d) => d.clone(),
        other => panic!("expected dict, got {other:?}"),
    }
}

fn extract_list(value: &VmValue) -> Rc<Vec<VmValue>> {
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
            VmValue::String(Rc::from(dir.to_string_lossy().to_string())),
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

// === File table accessors ===

#[test]
fn path_to_id_and_id_to_path_round_trip() {
    let dir = workspace();
    let (registry, _) = build();
    rebuild_in(dir.path(), &registry);

    let id_value = call(
        &registry,
        "hostlib_code_index_path_to_id",
        dict(&[("path", VmValue::String(Rc::from("src/main.ts")))]),
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
        dict(&[("path", VmValue::String(Rc::from("not/here.rs")))]),
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
        dict(&[("path", VmValue::String(Rc::from("src/util.ts")))]),
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
        dict(&[("path", VmValue::String(Rc::from("ghost.rs")))]),
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
        dict(&[("path", VmValue::String(Rc::from("README.md")))]),
    );
    let s = extract_str(&h);
    // FNV-1a of `# project\n` is deterministic; pre-computed by hand
    // (mirroring the Swift `ContentHasher.hash` reference implementation).
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

// === Cached reads ===

#[test]
fn read_range_returns_full_or_sliced_content() {
    let dir = workspace();
    let (registry, _) = build();
    rebuild_in(dir.path(), &registry);

    let full = call(
        &registry,
        "hostlib_code_index_read_range",
        dict(&[("path", VmValue::String(Rc::from("src/util.ts")))]),
    );
    let full = extract_dict(&full);
    let body = extract_str(full.get("content").unwrap());
    assert!(body.contains("helper"));

    let sliced = call(
        &registry,
        "hostlib_code_index_read_range",
        dict(&[
            ("path", VmValue::String(Rc::from("src/util.ts"))),
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
        dict(&[("path", VmValue::String(Rc::from("not/a/real/file.txt")))]),
    )
    .expect_err("missing file should error");
    let msg = format!("{err}");
    assert!(msg.contains("file not found"));
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
        dict(&[("path", VmValue::String(Rc::from(path)))]),
    ));

    fs::write(
        dir.path().join(path),
        "export const ZetaToken = \"refreshed\";\nexport function helper() { return 0; }\n",
    )
    .unwrap();
    let res = call(
        &registry,
        "hostlib_code_index_reindex_file",
        dict(&[("path", VmValue::String(Rc::from(path)))]),
    );
    let res = extract_dict(&res);
    assert!(extract_bool(res.get("indexed").unwrap()));
    let id_after = extract_int(res.get("file_id").unwrap());
    assert_eq!(id_before, id_after);

    // Trigram index reflects the new content.
    let q = call(
        &registry,
        "hostlib_code_index_query",
        dict(&[("needle", VmValue::String(Rc::from("ZetaToken")))]),
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
        dict(&[("path", VmValue::String(Rc::from("src/util.ts")))]),
    );
    let res = extract_dict(&res);
    assert!(!extract_bool(res.get("indexed").unwrap()));
    assert!(matches!(res.get("file_id").unwrap(), VmValue::Nil));

    let id = call(
        &registry,
        "hostlib_code_index_path_to_id",
        dict(&[("path", VmValue::String(Rc::from("src/util.ts")))]),
    );
    assert!(matches!(id, VmValue::Nil));
}

#[test]
fn extract_trigrams_matches_indexer() {
    let (registry, _) = build();
    let result = call(
        &registry,
        "hostlib_code_index_extract_trigrams",
        dict(&[("query", VmValue::String(Rc::from("foo")))]),
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
        dict(&[("query", VmValue::String(Rc::from("helper")))]),
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
        dict(&[("word", VmValue::String(Rc::from("helper")))]),
    );
    let list = extract_list(&hits);
    assert!(list.iter().all(|h| {
        let d = extract_dict(h);
        d.get("file_id")
            .filter(|v| matches!(v, VmValue::Int(_)))
            .is_some()
            && d.get("line")
                .filter(|v| matches!(v, VmValue::Int(_)))
                .is_some()
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
        dict(&[("path", VmValue::String(Rc::from("src/main.ts")))]),
    ));
    let util_id = extract_int(&call(
        &registry,
        "hostlib_code_index_path_to_id",
        dict(&[("path", VmValue::String(Rc::from("src/util.ts")))]),
    ));

    let imports_of_main = call(
        &registry,
        "hostlib_code_index_deps_get",
        dict(&[
            ("file_id", VmValue::Int(main_id)),
            ("direction", VmValue::String(Rc::from("imports"))),
        ]),
    );
    let imp_list = extract_list(&imports_of_main);
    assert!(imp_list.iter().any(|v| extract_int(v) == util_id));

    let importers_of_util = call(
        &registry,
        "hostlib_code_index_deps_get",
        dict(&[
            ("file_id", VmValue::Int(util_id)),
            ("direction", VmValue::String(Rc::from("importers"))),
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
        dict(&[("name", VmValue::String(Rc::from("editor")))]),
    ));

    let seq1 = extract_int(&call(
        &registry,
        "hostlib_code_index_version_record",
        dict(&[
            ("agent_id", VmValue::Int(agent_id)),
            ("path", VmValue::String(Rc::from("src/util.ts"))),
            ("op", VmValue::String(Rc::from("write"))),
            ("hash", VmValue::String(Rc::from("12345"))),
            ("size", VmValue::Int(42)),
        ]),
    ));
    let seq2 = extract_int(&call(
        &registry,
        "hostlib_code_index_version_record",
        dict(&[
            ("agent_id", VmValue::Int(agent_id)),
            ("path", VmValue::String(Rc::from("src/main.ts"))),
            ("op", VmValue::String(Rc::from("patch"))),
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
                ("path", VmValue::String(Rc::from(format!("f{i}.rs")))),
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
            ("name", VmValue::String(Rc::from("daemon"))),
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
            ("name", VmValue::String(Rc::from("alice"))),
            ("agent_id", VmValue::Int(1)),
        ]),
    );
    let _bob = call(
        &registry,
        "hostlib_code_index_agent_register",
        dict(&[
            ("name", VmValue::String(Rc::from("bob"))),
            ("agent_id", VmValue::Int(2)),
        ]),
    );

    let alice_grab = call(
        &registry,
        "hostlib_code_index_lock_try",
        dict(&[
            ("agent_id", VmValue::Int(1)),
            ("path", VmValue::String(Rc::from("src/main.ts"))),
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
            ("path", VmValue::String(Rc::from("src/main.ts"))),
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
            ("path", VmValue::String(Rc::from("src/main.ts"))),
        ]),
    );
    assert!(matches!(release, VmValue::Bool(true)));
    let bob_again = call(
        &registry,
        "hostlib_code_index_lock_try",
        dict(&[
            ("agent_id", VmValue::Int(2)),
            ("path", VmValue::String(Rc::from("src/main.ts"))),
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
        dict(&[("name", VmValue::String(Rc::from("worker")))]),
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
        dict(&[("name", VmValue::String(Rc::from("editor")))]),
    ));
    call(
        &registry_a,
        "hostlib_code_index_version_record",
        dict(&[
            ("agent_id", VmValue::Int(agent_id)),
            ("path", VmValue::String(Rc::from("src/util.ts"))),
            ("op", VmValue::String(Rc::from("write"))),
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
        dict(&[("path", VmValue::String(Rc::from("src/util.ts")))]),
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
                    VmValue::String(Rc::from(format!("worker-{thread_idx}"))),
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
                    ("path", VmValue::String(Rc::from(path.clone()))),
                    ("ttl_ms", VmValue::Int(50)),
                ])]);
                let _ = (lr.handler)(&[dict(&[
                    ("agent_id", VmValue::Int(agent_id as i64)),
                    ("path", VmValue::String(Rc::from(path.clone()))),
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
            ("path", VmValue::String(Rc::from("src/main.ts"))),
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
                let path = format!("src/f{}_{}.rs", thread_idx, i);
                let value = (entry.handler)(&[dict(&[
                    ("agent_id", VmValue::Int(thread_idx as i64 + 1)),
                    ("path", VmValue::String(Rc::from(path))),
                    ("op", VmValue::String(Rc::from("write"))),
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
