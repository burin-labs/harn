//! Integration tests for the typed-symbol-graph builtins added by
//! issue #2434: `code_index.cypher`, `code_index.branch_overlay`, and
//! `code_index.freshness`. Drives the public registry surface so the
//! schemas and error shape are exercised together.

use std::collections::BTreeMap;
use std::fs;
use std::rc::Rc;

use harn_hostlib::{
    code_index::CodeIndexCapability, BuiltinRegistry, HostlibCapability, RegisteredBuiltin,
};
use harn_vm::VmValue;

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

fn extract_dict(value: &VmValue) -> Rc<BTreeMap<String, VmValue>> {
    match value {
        VmValue::Dict(d) => d.clone(),
        other => panic!("expected dict, got {other:?}"),
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
            VmValue::String(Rc::from(root.to_string_lossy().as_ref())),
        )]),
    );
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
            VmValue::String(Rc::from(
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
fn branch_overlay_create_then_query_reports_reuse() {
    let dir = build_workspace();
    let (reg, _cap) = registry();
    rebuild(&reg, dir.path());

    let result = call(
        &reg,
        "hostlib_code_index_branch_overlay",
        dict(&[
            ("action", VmValue::String(Rc::from("create"))),
            ("branch", VmValue::String(Rc::from("topic/test"))),
        ]),
    );
    let d = extract_dict(&result);
    match d.get("active").unwrap() {
        VmValue::String(s) => assert_eq!(s.as_ref(), "topic/test"),
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
        dict(&[("action", VmValue::String(Rc::from("deactivate")))]),
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
        dict(&[("path", VmValue::String(Rc::from("src/a.rs")))]),
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
        dict(&[("path", VmValue::String(Rc::from("src/a.rs")))]),
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
        dict(&[("path", VmValue::String(Rc::from("src/does-not-exist.rs")))]),
    );
    let d = extract_dict(&result);
    assert!(matches!(d.get("known").unwrap(), VmValue::Bool(false)));
    assert!(matches!(d.get("stale").unwrap(), VmValue::Bool(true)));
}
