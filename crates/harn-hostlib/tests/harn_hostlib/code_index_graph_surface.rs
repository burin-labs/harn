//! Integration tests for the typed-symbol-graph builtins added by
//! issue #2434: `code_index.cypher`, `code_index.branch_overlay`, and
//! `code_index.freshness`. Drives the public registry surface so the
//! schemas and error shape are exercised together.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use harn_hostlib::{
    code_index::{CodeIndexCapability, ResolvedHarnReference},
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

fn extract_dict(value: &VmValue) -> Arc<harn_vm::value::DictMap> {
    match value {
        VmValue::Dict(d) => d.clone(),
        other => panic!("expected dict, got {other:?}"),
    }
}

fn string_field(dict: &harn_vm::value::DictMap, key: &str) -> String {
    match dict
        .get(key)
        .unwrap_or_else(|| panic!("missing field {key}"))
    {
        VmValue::String(s) => s.to_string(),
        other => panic!("expected string field {key}, got {other:?}"),
    }
}

fn bool_field(dict: &harn_vm::value::DictMap, key: &str) -> bool {
    match dict
        .get(key)
        .unwrap_or_else(|| panic!("missing field {key}"))
    {
        VmValue::Bool(value) => *value,
        other => panic!("expected bool field {key}, got {other:?}"),
    }
}

fn list_field(dict: &harn_vm::value::DictMap, key: &str) -> Arc<Vec<VmValue>> {
    match dict
        .get(key)
        .unwrap_or_else(|| panic!("missing field {key}"))
    {
        VmValue::List(value) => value.clone(),
        other => panic!("expected list field {key}, got {other:?}"),
    }
}

fn build_workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src/a.rs"),
        "pub fn alpha() {}\npub fn beta() { alpha(); }\n",
    )
    .unwrap();
    fs::write(
        root.join("src/b.rs"),
        "pub fn gamma() {}\npub fn driver() { gamma(); }\n",
    )
    .unwrap();
    dir
}

fn registry() -> (BuiltinRegistry, CodeIndexCapability) {
    let cap = CodeIndexCapability::new();
    let mut registry = BuiltinRegistry::new();
    cap.register_builtins(&mut registry);
    (registry, cap)
}

fn rebuild(registry: &BuiltinRegistry, root: &std::path::Path) {
    call(
        registry,
        "hostlib_code_index_rebuild",
        dict(&[(
            "root",
            VmValue::String(arcstr::ArcStr::from(root.to_string_lossy().as_ref())),
        )]),
    );
}

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.join(".git/empty-global-config"))
        .env("GIT_AUTHOR_NAME", "Index Probe")
        .env("GIT_AUTHOR_EMAIL", "index@example.invalid")
        .env("GIT_COMMITTER_NAME", "Index Probe")
        .env("GIT_COMMITTER_EMAIL", "index@example.invalid")
        .output()
        .expect("run isolated git");
    assert!(
        output.status.success(),
        "git {args:?}: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn function_count(registry: &BuiltinRegistry, name: &str) -> usize {
    let query = format!("MATCH (f:Function {{name: '{name}'}}) RETURN f.path AS path");
    let result = call(
        registry,
        "hostlib_code_index_cypher",
        dict(&[("query", VmValue::String(query.as_str().into()))]),
    );
    list_field(&extract_dict(&result), "rows").len()
}

fn indexed_content(registry: &BuiltinRegistry, path: &str) -> String {
    let result = call(
        registry,
        "hostlib_code_index_read_range",
        dict(&[("path", VmValue::String(path.into()))]),
    );
    string_field(&extract_dict(&result), "content")
}

#[test]
fn cypher_returns_function_by_name() {
    let dir = build_workspace();
    let (reg, _cap) = registry();
    rebuild(&reg, dir.path());

    let result = call(
        &reg,
        "hostlib_code_index_cypher",
        dict(&[(
            "query",
            VmValue::String(arcstr::ArcStr::from(
                "MATCH (f:Function {name: 'alpha'}) RETURN f.path AS path",
            )),
        )]),
    );
    let outer = extract_dict(&result);
    let rows = match outer.get("rows").unwrap() {
        VmValue::List(l) => l.clone(),
        other => panic!("expected list of rows, got {other:?}"),
    };
    assert_eq!(rows.len(), 1, "expected one match for fn alpha");
    let row = extract_dict(&rows[0]);
    let path = match row.get("path").unwrap() {
        VmValue::String(s) => s.to_string(),
        other => panic!("expected string path, got {other:?}"),
    };
    assert_eq!(path, "src/a.rs");
}

#[test]
fn restored_index_keeps_the_typed_symbol_graph() {
    let dir = build_workspace();
    let (writer_registry, writer) = registry();
    rebuild(&writer_registry, dir.path());
    let modules = |registry: &BuiltinRegistry| {
        let result = call(
            registry,
            "hostlib_code_index_cypher",
            dict(&[(
                "query",
                VmValue::String(arcstr::ArcStr::from(
                    "MATCH (m:Module) RETURN m.path AS path",
                )),
            )]),
        );
        list_field(&extract_dict(&result), "rows").len()
    };
    assert_eq!(
        modules(&writer_registry),
        2,
        "the fresh graph must be populated"
    );
    assert!(writer.persist_to_disk().expect("persist index"));

    let (reader_registry, reader) = registry();
    assert_eq!(
        reader.warm_session(dir.path()),
        harn_hostlib::code_index::SessionWarmOutcome::Restored
    );
    assert_eq!(
        modules(&reader_registry),
        2,
        "a successful restore must not turn a measured graph into an empty one"
    );
}

#[test]
fn restored_harn_references_use_the_current_host_resolver() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("importer.harn"), "fn helper() { run() }\n").unwrap();
    fs::write(dir.path().join("first.harn"), "pub fn run() { 1 }\n").unwrap();
    fs::write(dir.path().join("second.harn"), "pub fn run() { 2 }\n").unwrap();

    let query = |registry: &BuiltinRegistry| {
        let result = call(
            registry,
            "hostlib_code_index_cypher",
            dict(&[(
                "query",
                VmValue::String(
                    "MATCH (m:Module)-[:REFS]->(f:Function {name: 'run'}) RETURN f.path AS path"
                        .into(),
                ),
            )]),
        );
        list_field(&extract_dict(&result), "rows")
            .iter()
            .map(|row| string_field(&extract_dict(row), "path"))
            .collect::<Vec<_>>()
    };
    let capability = |target: &'static str| {
        CodeIndexCapability::new().with_harn_reference_resolver(Arc::new(move |_| {
            Ok(vec![ResolvedHarnReference {
                from_path: "importer.harn".into(),
                to_path: target.into(),
                to_name: "run".into(),
            }])
        }))
    };
    let writer = capability("first.harn");
    let mut writer_registry = BuiltinRegistry::new();
    writer.register_builtins(&mut writer_registry);
    rebuild(&writer_registry, dir.path());
    assert_eq!(query(&writer_registry), vec!["first.harn"]);
    assert!(writer.persist_to_disk().unwrap());

    let reader = capability("second.harn");
    let mut reader_registry = BuiltinRegistry::new();
    reader.register_builtins(&mut reader_registry);
    assert!(reader.restore_from_disk(dir.path()).unwrap());
    assert_eq!(query(&reader_registry), vec!["second.harn"]);

    let no_resolver = CodeIndexCapability::new();
    let mut no_resolver_registry = BuiltinRegistry::new();
    no_resolver.register_builtins(&mut no_resolver_registry);
    assert!(no_resolver.restore_from_disk(dir.path()).unwrap());
    assert!(query(&no_resolver_registry).is_empty());
}

#[test]
fn restored_graph_reconciles_a_commit_and_an_uncommitted_edit() {
    let dir = build_workspace();
    git(dir.path(), &["init", "-q"]);
    git(dir.path(), &["add", "src/a.rs", "src/b.rs"]);
    git(
        dir.path(),
        &[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            "initial",
        ],
    );
    let (baseline_registry, baseline) = registry();
    rebuild(&baseline_registry, dir.path());
    assert_eq!(function_count(&baseline_registry, "alpha"), 1);
    assert!(baseline.persist_to_disk().unwrap());

    // A commit may replace contents without changing a file's size or mtime.
    // A HEAD mismatch must verify bytes before retaining the old graph.
    let a = dir.path().join("src/a.rs");
    let old_mtime = filetime::FileTime::from_last_modification_time(&fs::metadata(&a).unwrap());
    let before = fs::read_to_string(&a).unwrap();
    let after = before.replace("alpha", "delta");
    assert_eq!(before.len(), after.len());
    fs::write(&a, after).unwrap();
    filetime::set_file_mtime(&a, old_mtime).unwrap();
    // Git's index trusts a matching size and mtime. Where it has no inode
    // change time to compare (Windows), `git add` would stage nothing and the
    // commit would find nothing to commit. Dropping the entry forces a re-hash.
    git(dir.path(), &["rm", "--cached", "-q", "src/a.rs"]);
    git(dir.path(), &["add", "src/a.rs"]);
    git(
        dir.path(),
        &["-c", "commit.gpgsign=false", "commit", "-q", "-m", "rename"],
    );

    let (committed_registry, committed) = registry();
    assert!(committed.restore_from_disk(dir.path()).unwrap());
    assert_eq!(function_count(&committed_registry, "alpha"), 0);
    assert_eq!(function_count(&committed_registry, "delta"), 1);
    assert!(indexed_content(&committed_registry, "src/a.rs").contains("fn delta"));

    // A second process also sees an ordinary uncommitted edit. Its saved
    // snapshot already carries the new HEAD, so this exercises file refresh.
    let b = dir.path().join("src/b.rs");
    let old_mtime = filetime::FileTime::from_last_modification_time(&fs::metadata(&b).unwrap());
    let before = fs::read_to_string(&b).unwrap();
    let after = before.replace("gamma", "theta");
    assert_eq!(before.len(), after.len());
    fs::write(&b, after).unwrap();
    filetime::set_file_mtime(
        &b,
        filetime::FileTime::from_unix_time(old_mtime.unix_seconds() + 2, 0),
    )
    .unwrap();
    let (edited_registry, edited) = registry();
    assert!(edited.restore_from_disk(dir.path()).unwrap());
    assert_eq!(function_count(&edited_registry, "gamma"), 0);
    assert_eq!(function_count(&edited_registry, "theta"), 1);
    assert!(indexed_content(&edited_registry, "src/b.rs").contains("fn theta"));
}

#[test]
fn harn_cypher_reindex_replaces_resolved_refs() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("exported.harn"), "pub fn run() { 1 }\n").unwrap();
    fs::write(dir.path().join("alternate.harn"), "pub fn run() { 2 }\n").unwrap();
    fs::write(
        dir.path().join("importer.harn"),
        "import { run } from \"./exported\"\nfn helper() { run() }\n",
    )
    .unwrap();

    let changed = Arc::new(AtomicBool::new(false));
    let resolver_changed = changed.clone();
    let cap = CodeIndexCapability::new().with_harn_reference_resolver(Arc::new(move |_| {
        Ok(vec![ResolvedHarnReference {
            from_path: "importer.harn".into(),
            to_path: if resolver_changed.load(Ordering::SeqCst) {
                "alternate.harn".into()
            } else {
                "exported.harn".into()
            },
            to_name: "run".into(),
        }])
    }));
    let mut reg = BuiltinRegistry::new();
    cap.register_builtins(&mut reg);
    rebuild(&reg, dir.path());

    let query = || {
        let result = call(
            &reg,
            "hostlib_code_index_cypher",
            dict(&[(
                "query",
                VmValue::String(arcstr::ArcStr::from(
                    "MATCH (m:Module)-[:REFS]->(f:Function {name: 'run'}) RETURN m.path AS source, f.path AS target",
                )),
            )]),
        );
        list_field(&extract_dict(&result), "rows")
            .iter()
            .map(|row| {
                let row = extract_dict(row);
                (string_field(&row, "source"), string_field(&row, "target"))
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        query(),
        vec![("importer.harn".into(), "exported.harn".into())]
    );

    changed.store(true, Ordering::SeqCst);
    call(
        &reg,
        "hostlib_code_index_reindex_file",
        dict(&[("path", VmValue::String("importer.harn".into()))]),
    );
    assert_eq!(
        query(),
        vec![("importer.harn".into(), "alternate.harn".into())]
    );
}

#[test]
fn repo_map_prioritizes_task_named_symbols() {
    let dir = build_workspace();
    let (reg, _cap) = registry();
    rebuild(&reg, dir.path());

    let result = call(
        &reg,
        "hostlib_code_index_repo_map",
        dict(&[
            ("task", VmValue::String(arcstr::ArcStr::from("Fix alpha"))),
            ("max_entries", VmValue::Int(4)),
            ("token_budget", VmValue::Int(200)),
        ]),
    );
    let outer = extract_dict(&result);
    let rendered = string_field(&outer, "rendered");
    assert!(rendered.contains("src/a.rs:"), "rendered map: {rendered}");
    assert!(rendered.contains("alpha"), "rendered map: {rendered}");

    let entries = list_field(&outer, "entries");
    assert!(!entries.is_empty(), "repo map should return ranked entries");
    let first = extract_dict(&entries[0]);
    assert_eq!(string_field(&first, "name"), "alpha");
    let reasons = list_field(&first, "reasons");
    assert!(
        reasons.iter().any(
            |value| matches!(value, VmValue::String(reason) if reason.as_str() == "task_symbol")
        ),
        "task-named symbol should carry task_symbol reason"
    );
}

#[test]
fn repo_map_boosts_context_files_and_respects_budget() {
    let dir = build_workspace();
    let (reg, _cap) = registry();
    rebuild(&reg, dir.path());

    let result = call(
        &reg,
        "hostlib_code_index_repo_map",
        dict(&[
            (
                "context_files",
                VmValue::List(Arc::new(vec![VmValue::String(arcstr::ArcStr::from(
                    "src/b.rs",
                ))])),
            ),
            ("max_entries", VmValue::Int(4)),
            ("token_budget", VmValue::Int(200)),
        ]),
    );
    let outer = extract_dict(&result);
    let entries = list_field(&outer, "entries");
    assert!(!entries.is_empty(), "repo map should return ranked entries");
    let first = extract_dict(&entries[0]);
    assert_eq!(string_field(&first, "path"), "src/b.rs");
    let reasons = list_field(&first, "reasons");
    assert!(
        reasons.iter().any(
            |value| matches!(value, VmValue::String(reason) if reason.as_str() == "context_file")
        ),
        "context-file symbol should carry context_file reason"
    );

    let tiny = call(
        &reg,
        "hostlib_code_index_repo_map",
        dict(&[
            ("max_entries", VmValue::Int(4)),
            ("token_budget", VmValue::Int(1)),
        ]),
    );
    let tiny_outer = extract_dict(&tiny);
    let rendered = string_field(&tiny_outer, "rendered");
    assert!(rendered.len() <= 4, "rendered map must honor char budget");
    assert!(
        bool_field(&tiny_outer, "truncated"),
        "tiny budget should truncate"
    );
}

#[test]
fn branch_overlay_create_then_query_reports_reuse() {
    let dir = build_workspace();
    let (reg, _cap) = registry();
    rebuild(&reg, dir.path());

    let result = call(
        &reg,
        "hostlib_code_index_branch_overlay",
        dict(&[
            ("action", VmValue::String(arcstr::ArcStr::from("create"))),
            (
                "branch",
                VmValue::String(arcstr::ArcStr::from("topic/test")),
            ),
        ]),
    );
    let d = extract_dict(&result);
    match d.get("active").unwrap() {
        VmValue::String(s) => assert_eq!(s.as_str(), "topic/test"),
        other => panic!("expected active branch string, got {other:?}"),
    }
    let reuse = match d.get("reuse_fraction").unwrap() {
        VmValue::Float(f) => *f,
        other => panic!("expected float, got {other:?}"),
    };
    // No deltas staged: full reuse.
    assert!(reuse >= 0.999, "expected ≥95% reuse, got {reuse}");

    // Deactivate brings us back to the base.
    let result = call(
        &reg,
        "hostlib_code_index_branch_overlay",
        dict(&[(
            "action",
            VmValue::String(arcstr::ArcStr::from("deactivate")),
        )]),
    );
    let d = extract_dict(&result);
    assert!(matches!(d.get("active").unwrap(), VmValue::Nil));
}

#[test]
fn freshness_detects_post_index_edits() {
    let dir = build_workspace();
    let (reg, _cap) = registry();
    rebuild(&reg, dir.path());

    // Pristine: not stale.
    let result = call(
        &reg,
        "hostlib_code_index_freshness",
        dict(&[("path", VmValue::String(arcstr::ArcStr::from("src/a.rs")))]),
    );
    let d = extract_dict(&result);
    assert!(matches!(d.get("known").unwrap(), VmValue::Bool(true)));
    assert!(matches!(d.get("stale").unwrap(), VmValue::Bool(false)));

    // Edit the file in place; the index hasn't been told yet.
    fs::write(
        dir.path().join("src/a.rs"),
        "pub fn alpha() {}\npub fn beta() {}\npub fn omega() {}\n",
    )
    .unwrap();
    let result = call(
        &reg,
        "hostlib_code_index_freshness",
        dict(&[("path", VmValue::String(arcstr::ArcStr::from("src/a.rs")))]),
    );
    let d = extract_dict(&result);
    assert!(matches!(d.get("stale").unwrap(), VmValue::Bool(true)));
}

#[test]
fn freshness_reports_unknown_for_unindexed_paths() {
    let dir = build_workspace();
    let (reg, _cap) = registry();
    rebuild(&reg, dir.path());

    let result = call(
        &reg,
        "hostlib_code_index_freshness",
        dict(&[(
            "path",
            VmValue::String(arcstr::ArcStr::from("src/does-not-exist.rs")),
        )]),
    );
    let d = extract_dict(&result);
    assert!(matches!(d.get("known").unwrap(), VmValue::Bool(false)));
    assert!(matches!(d.get("stale").unwrap(), VmValue::Bool(true)));
}
