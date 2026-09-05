//! Integration tests for incremental index maintenance (issue #7973).
//!
//! Before this suite existed, `hostlib_code_index_rebuild` walked, read,
//! and tree-sitter-parsed every file in the workspace on **every** index
//! read, whether or not anything had changed. On a 7,000-file repository
//! that was a 71-second operation, measured twice back to back with an
//! identical result, so the entire index-backed tier was unusable in
//! practice.
//!
//! Two properties are asserted here, and both are counted rather than
//! timed. A wall-clock threshold on a shared build machine is a flake; a
//! count of files re-read is exact and cannot pass vacuously.
//!
//! 1. **An unchanged tree costs nothing.** A second read re-reads zero
//!    files.
//! 2. **A changed tree is still noticed.** Every shape of change — edited
//!    content, an added file, a deleted file, and an edit that preserves
//!    the file's size — re-reads exactly the files it should and no
//!    others.
//!
//! Known blind spot, asserted nowhere because it is a deliberate design
//! limit: the freshness gate reads mtime and size. A change that restores
//! both to their previous values is invisible to it. `force: true` on the
//! rebuild builtin is the escape hatch for that case.

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicI64, Ordering};

use harn_hostlib::{
    code_index::{CodeIndexCapability, IndexState},
    BuiltinRegistry, HostlibCapability, RegisteredBuiltin,
};
use harn_vm::VmValue;

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

fn field(value: &VmValue, key: &str) -> i64 {
    let VmValue::Dict(d) = value else {
        panic!("expected dict, got {value:?}");
    };
    match d.get(key) {
        Some(VmValue::Int(i)) => *i,
        other => panic!("expected int at `{key}`, got {other:?}"),
    }
}

fn mode(value: &VmValue) -> String {
    let VmValue::Dict(d) = value else {
        panic!("expected dict, got {value:?}");
    };
    match d.get("mode") {
        Some(VmValue::String(s)) => s.to_string(),
        other => panic!("expected string at `mode`, got {other:?}"),
    }
}

fn str_value(s: &str) -> VmValue {
    VmValue::String(arcstr::ArcStr::from(s))
}

/// Fixed timestamp these tests stamp onto files, one second later on
/// every write. Nothing here reads the wall clock: the freshness gate
/// compares a file's mtime against the value the index recorded, so all
/// that matters is that each write is distinguishable from the last, and
/// a deterministic counter says that more exactly than `now()` does.
/// Writing twice inside one filesystem timestamp tick would otherwise
/// let a test assert that a real change was noticed while the gate was
/// being handed identical metadata — a green that proves nothing.
static NEXT_MTIME_SECS: AtomicI64 = AtomicI64::new(1_700_000_000);

/// Write `contents` to `path` and stamp it with an mtime strictly newer
/// than every mtime this suite has handed out so far.
fn write_with_fresh_mtime(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
    let secs = NEXT_MTIME_SECS.fetch_add(1, Ordering::Relaxed);
    filetime::set_file_mtime(path, filetime::FileTime::from_unix_time(secs, 0)).unwrap();
}

/// A small workspace with resolvable imports and parseable symbols in two
/// languages, so a refresh has a symbol graph and a dep graph to keep
/// consistent rather than just a file table.
fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_with_fresh_mtime(
        &root.join("src/main.rs"),
        "mod util;\npub fn main() { util::helper(); }\n",
    );
    write_with_fresh_mtime(
        &root.join("src/util.rs"),
        "pub fn helper() -> i32 { 1 }\npub struct Widget { pub size: i32 }\n",
    );
    write_with_fresh_mtime(
        &root.join("src/lib.rs"),
        "pub mod util;\npub fn entry() -> i32 { 2 }\n",
    );
    write_with_fresh_mtime(&root.join("docs/readme.md"), "# fixture\n");
    dir
}

fn registry_for(cap: &CodeIndexCapability) -> BuiltinRegistry {
    let mut registry = BuiltinRegistry::new();
    cap.register_builtins(&mut registry);
    registry
}

fn rebuild(registry: &BuiltinRegistry, root: &Path) -> VmValue {
    call(
        registry,
        "hostlib_code_index_rebuild",
        dict(&[("root", str_value(&root.to_string_lossy()))]),
    )
}

fn rebuild_forced(registry: &BuiltinRegistry, root: &Path) -> VmValue {
    call(
        registry,
        "hostlib_code_index_rebuild",
        dict(&[
            ("root", str_value(&root.to_string_lossy())),
            ("force", VmValue::Bool(true)),
        ]),
    )
}

// === IndexState::refresh_from_root — the owning interface ===

#[test]
fn refresh_on_an_unchanged_tree_reads_no_files() {
    let dir = fixture();
    let (mut state, build) = IndexState::build_from_root(dir.path());
    assert_eq!(build.files_indexed, 4, "fixture should index four files");

    let refresh = state.refresh_from_root(None);

    assert_eq!(refresh.files_reindexed, 0, "no file changed, none re-read");
    assert_eq!(refresh.files_added, 0);
    assert_eq!(refresh.files_removed, 0);
    assert_eq!(refresh.files_touched_only, 0);
    assert_eq!(
        refresh.files_unchanged, 4,
        "every file must be recognised as unchanged, not merely not-reindexed"
    );
    assert_eq!(
        refresh.files_scanned, 4,
        "a refresh that scanned nothing would also report zero re-reads"
    );
    assert!(refresh.is_noop());
}

#[test]
fn refresh_after_one_edit_reads_exactly_that_file() {
    let dir = fixture();
    let (mut state, _) = IndexState::build_from_root(dir.path());

    write_with_fresh_mtime(
        &dir.path().join("src/util.rs"),
        "pub fn helper() -> i32 { 99 }\npub struct Widget { pub size: i32 }\npub fn added() {}\n",
    );
    let refresh = state.refresh_from_root(None);

    assert_eq!(
        refresh.files_reindexed, 1,
        "exactly one file changed; the other three must not be re-read"
    );
    assert_eq!(refresh.files_unchanged, 3);
    assert_eq!(refresh.files_added, 0);
    assert_eq!(refresh.files_removed, 0);

    // The change is not merely counted — it reached the index.
    let id = state
        .lookup_path("src/util.rs")
        .expect("util still indexed");
    assert!(
        state.files[&id].symbols.iter().any(|s| s.name == "added"),
        "the new symbol must be in the outline after the refresh"
    );
}

#[test]
fn refresh_notices_an_edit_that_preserves_the_file_size() {
    let dir = fixture();
    let (mut state, _) = IndexState::build_from_root(dir.path());
    let before = fs::read_to_string(dir.path().join("src/util.rs")).unwrap();

    // Same byte length, different bytes. A gate keyed only on size would
    // sail past this.
    let after = before.replace("helper", "helpee");
    assert_eq!(before.len(), after.len(), "the edit must preserve length");
    write_with_fresh_mtime(&dir.path().join("src/util.rs"), &after);

    let refresh = state.refresh_from_root(None);

    assert_eq!(refresh.files_reindexed, 1);
    let id = state.lookup_path("src/util.rs").unwrap();
    assert!(state.files[&id].symbols.iter().any(|s| s.name == "helpee"));
    assert!(!state.files[&id].symbols.iter().any(|s| s.name == "helper"));
}

#[test]
fn refresh_reports_a_touched_file_separately_from_a_changed_one() {
    let dir = fixture();
    let (mut state, _) = IndexState::build_from_root(dir.path());
    let same = fs::read_to_string(dir.path().join("src/util.rs")).unwrap();

    // New timestamp, identical bytes: the file is re-read, but nothing
    // downstream of the content hash needs redoing.
    write_with_fresh_mtime(&dir.path().join("src/util.rs"), &same);
    let refresh = state.refresh_from_root(None);

    assert_eq!(refresh.files_touched_only, 1);
    assert_eq!(
        refresh.files_reindexed, 0,
        "identical bytes must not trigger a re-parse"
    );
    assert!(refresh.is_noop());

    // And the recorded mtime must have moved, or the same file would be
    // re-read on every refresh from here on.
    let second = state.refresh_from_root(None);
    assert_eq!(
        second.files_touched_only, 0,
        "the refresh must record the new mtime so it stops re-reading the file"
    );
    assert_eq!(second.files_unchanged, 4);
}

#[test]
fn refresh_picks_up_an_added_file_and_resolves_imports_into_it() {
    let dir = fixture();
    let (mut state, _) = IndexState::build_from_root(dir.path());

    write_with_fresh_mtime(
        &dir.path().join("src/extra.rs"),
        "pub fn brand_new_symbol() -> i32 { 7 }\n",
    );
    let refresh = state.refresh_from_root(None);

    assert_eq!(refresh.files_added, 1);
    assert_eq!(refresh.files_reindexed, 1);
    assert_eq!(refresh.files_unchanged, 4);
    let id = state.lookup_path("src/extra.rs").expect("new file indexed");
    assert!(state.files[&id]
        .symbols
        .iter()
        .any(|s| s.name == "brand_new_symbol"));
    assert!(
        state
            .symbols
            .iter_nodes()
            .any(|n| n.name == "brand_new_symbol"),
        "a newly added file must reach the typed symbol graph, not just the file table"
    );
}

#[test]
fn refresh_drops_a_deleted_file_from_every_sub_index() {
    let dir = fixture();
    let (mut state, _) = IndexState::build_from_root(dir.path());
    // Match on the owning path, not the symbol name: `helper` is also a
    // call site in `src/main.rs`, and a name-only assertion would fail
    // for a reason that has nothing to do with deletion.
    assert!(state
        .symbols
        .iter_nodes()
        .any(|n| n.path == "src/util.rs" && n.name == "helper"));

    fs::remove_file(dir.path().join("src/util.rs")).unwrap();
    let refresh = state.refresh_from_root(None);

    assert_eq!(refresh.files_removed, 1);
    assert_eq!(refresh.files_unchanged, 3);
    assert!(state.lookup_path("src/util.rs").is_none());
    assert!(
        !state.symbols.iter_nodes().any(|n| n.path == "src/util.rs"),
        "every node owned by the deleted file must leave the typed graph"
    );
    assert!(
        state
            .files
            .values()
            .all(|f| f.relative_path != "src/util.rs"),
        "the deleted file must leave the file table"
    );
}

// === The rebuild builtin — the canonical caller ===

#[test]
fn second_rebuild_call_reindexes_nothing() {
    let dir = fixture();
    let cap = CodeIndexCapability::new();
    let registry = registry_for(&cap);

    let first = rebuild(&registry, dir.path());
    assert_eq!(field(&first, "files_indexed"), 4);
    assert_eq!(
        mode(&first),
        "built",
        "a cold call has no prior index to reconcile against"
    );
    assert_eq!(field(&first, "files_reindexed"), 4);

    let second = rebuild(&registry, dir.path());
    assert_eq!(
        mode(&second),
        "refreshed",
        "the second call must reconcile, not rebuild"
    );
    assert_eq!(
        field(&second, "files_reindexed"),
        0,
        "the second read of an unchanged tree must not re-read a single file"
    );
    assert_eq!(field(&second, "files_unchanged"), 4);
    assert_eq!(field(&second, "files_indexed"), 4);
}

#[test]
fn rebuild_after_an_edit_reindexes_only_the_edited_file() {
    let dir = fixture();
    let cap = CodeIndexCapability::new();
    let registry = registry_for(&cap);
    rebuild(&registry, dir.path());

    write_with_fresh_mtime(
        &dir.path().join("src/main.rs"),
        "mod util;\npub fn main() { util::helper(); }\npub fn second_entry() {}\n",
    );
    let after = rebuild(&registry, dir.path());

    assert_eq!(field(&after, "files_reindexed"), 1);
    assert_eq!(field(&after, "files_unchanged"), 3);

    let hits = call(
        &registry,
        "hostlib_code_index_query",
        dict(&[("needle", str_value("second_entry"))]),
    );
    let VmValue::Dict(d) = &hits else {
        panic!("expected dict")
    };
    let VmValue::List(results) = d.get("results").expect("results") else {
        panic!("expected list")
    };
    assert_eq!(
        results.len(),
        1,
        "the edit must be searchable through the refreshed index"
    );
}

#[test]
fn forced_rebuild_still_walks_and_reparses_everything() {
    let dir = fixture();
    let cap = CodeIndexCapability::new();
    let registry = registry_for(&cap);
    rebuild(&registry, dir.path());

    let forced = rebuild_forced(&registry, dir.path());

    assert_eq!(field(&forced, "files_indexed"), 4);
    assert_eq!(
        mode(&forced),
        "built",
        "`force` must take the full-build path, not the reconciliation path"
    );
    assert_eq!(field(&forced, "files_reindexed"), 4);
}

// === Cross-run reuse ===

/// A no-op refresh must not rewrite the snapshot. The snapshot is a
/// whole-index serialisation — on a large workspace it is hundreds of
/// megabytes — so persisting after a read that changed nothing would put
/// the cost straight back onto the path this change just made free.
#[test]
fn a_refresh_that_changed_nothing_does_not_rewrite_the_snapshot() {
    let dir = fixture();
    let cap = CodeIndexCapability::new();
    let registry = registry_for(&cap);
    rebuild(&registry, dir.path());
    cap.persist_to_disk().expect("snapshot saved");

    let snapshot = dir.path().join(".burin/index/snapshot.json");
    let before = fs::metadata(&snapshot)
        .expect("snapshot exists")
        .modified()
        .unwrap();
    // Stamp a fixed, unmistakably old mtime so a rewrite is visible even
    // inside one filesystem timestamp tick.
    filetime::set_file_mtime(
        &snapshot,
        filetime::FileTime::from_unix_time(1_600_000_000, 0),
    )
    .unwrap();
    let backdated = fs::metadata(&snapshot).unwrap().modified().unwrap();
    assert!(
        backdated < before,
        "backdating must actually move the mtime"
    );

    let refreshed = rebuild(&registry, dir.path());
    assert_eq!(field(&refreshed, "files_reindexed"), 0);
    assert_eq!(
        fs::metadata(&snapshot).unwrap().modified().unwrap(),
        backdated,
        "a refresh that changed nothing must leave the snapshot alone"
    );

    // Negative control: a real change must still be written back.
    write_with_fresh_mtime(
        &dir.path().join("src/util.rs"),
        "pub fn helper() -> i32 { 1 }\npub fn persisted() {}\n",
    );
    let changed = rebuild(&registry, dir.path());
    assert_eq!(field(&changed, "files_reindexed"), 1);
    assert!(
        fs::metadata(&snapshot).unwrap().modified().unwrap() > backdated,
        "a refresh that changed the index must write the snapshot"
    );
}

#[test]
fn restore_reconciles_a_file_that_changed_while_the_process_was_down() {
    let dir = fixture();
    let writer = CodeIndexCapability::new();
    let writer_registry = registry_for(&writer);
    rebuild(&writer_registry, dir.path());
    writer.persist_to_disk().expect("snapshot saved");

    // The workspace moves on without the index watching.
    write_with_fresh_mtime(
        &dir.path().join("src/util.rs"),
        "pub fn helper() -> i32 { 1 }\npub fn written_while_cold() {}\n",
    );
    fs::remove_file(dir.path().join("docs/readme.md")).unwrap();

    let reader = CodeIndexCapability::new();
    let reader_registry = registry_for(&reader);
    assert!(reader.restore_from_disk(dir.path()).expect("restorable"));

    let hits = call(
        &reader_registry,
        "hostlib_code_index_query",
        dict(&[("needle", str_value("written_while_cold"))]),
    );
    let VmValue::Dict(d) = &hits else {
        panic!("expected dict")
    };
    let VmValue::List(results) = d.get("results").expect("results") else {
        panic!("expected list")
    };
    assert_eq!(
        results.len(),
        1,
        "restore must reconcile against disk, never serve the workspace as it was"
    );

    let stats = call(&reader_registry, "hostlib_code_index_stats", dict(&[]));
    assert_eq!(
        field(&stats, "indexed_files"),
        3,
        "the file deleted while the process was down must be gone from the restored index"
    );
}
