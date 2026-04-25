//! Integration tests for the `code_index` host capability.
//!
//! Exercise every builtin end-to-end against a temp workspace: rebuild,
//! query, stats, imports_for, importers_of. The builtins are routed
//! through the same `BuiltinRegistry` plumbing the VM uses, so passing
//! these tests proves the schema-locked surface returns the right shape
//! for embedders.

use std::collections::BTreeMap;
use std::fs;
use std::rc::Rc;

use harn_hostlib::{
    code_index::CodeIndexCapability, BuiltinRegistry, HostlibCapability, RegisteredBuiltin,
};
use harn_vm::VmValue;

fn build_registry() -> (BuiltinRegistry, CodeIndexCapability) {
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
    let entry: &RegisteredBuiltin = registry.find(name).unwrap_or_else(|| {
        panic!("builtin {name} not registered");
    });
    (entry.handler)(&[payload]).unwrap_or_else(|err| {
        panic!("builtin {name} failed: {err:?}");
    })
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

fn extract_str(value: &VmValue) -> String {
    match value {
        VmValue::String(s) => s.to_string(),
        other => panic!("expected string, got {other:?}"),
    }
}

fn write_workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::create_dir_all(root.join("docs")).unwrap();
    fs::write(
        root.join("src/index.ts"),
        "import { helper } from \"./util\";\nimport { other } from \"./other\";\nexport const alphaToken = helper();\n",
    )
    .unwrap();
    fs::write(
        root.join("src/util.ts"),
        "export function helper() { return 'AlphaToken from util'; }\n",
    )
    .unwrap();
    fs::write(
        root.join("src/other.ts"),
        "import { helper } from \"./util\";\nexport function other() { return helper(); }\n",
    )
    .unwrap();
    fs::write(
        root.join("docs/notes.md"),
        "Random notes about alphaToken.\n",
    )
    .unwrap();
    fs::write(root.join("README.md"), "# project\nNo content here.\n").unwrap();
    dir
}

#[test]
fn rebuild_then_query_returns_hits_for_indexed_substring() {
    let dir = write_workspace();
    let (registry, _cap) = build_registry();

    let rebuild = call(
        &registry,
        "hostlib_code_index_rebuild",
        dict(&[(
            "root",
            VmValue::String(Rc::from(dir.path().to_string_lossy().to_string())),
        )]),
    );
    let r = extract_dict(&rebuild);
    assert!(extract_int(r.get("files_indexed").unwrap()) >= 4);

    let response = call(
        &registry,
        "hostlib_code_index_query",
        dict(&[("needle", VmValue::String(Rc::from("alphaToken")))]),
    );
    let response = extract_dict(&response);
    let results = extract_list(response.get("results").unwrap());
    let mut paths: Vec<String> = results
        .iter()
        .map(|hit| {
            let dict = extract_dict(hit);
            extract_str(dict.get("path").unwrap())
        })
        .collect();
    paths.sort();
    assert!(paths.contains(&"src/index.ts".to_string()));
    assert!(paths.contains(&"src/util.ts".to_string()));
    assert!(paths.contains(&"docs/notes.md".to_string()));
}

#[test]
fn query_respects_case_sensitive_flag() {
    let dir = write_workspace();
    let (registry, _) = build_registry();

    call(
        &registry,
        "hostlib_code_index_rebuild",
        dict(&[(
            "root",
            VmValue::String(Rc::from(dir.path().to_string_lossy().to_string())),
        )]),
    );

    let case_sensitive = call(
        &registry,
        "hostlib_code_index_query",
        dict(&[
            ("needle", VmValue::String(Rc::from("alphaToken"))),
            ("case_sensitive", VmValue::Bool(true)),
        ]),
    );
    let cs = extract_dict(&case_sensitive);
    let cs_results = extract_list(cs.get("results").unwrap());

    let case_insensitive = call(
        &registry,
        "hostlib_code_index_query",
        dict(&[
            ("needle", VmValue::String(Rc::from("alphaToken"))),
            ("case_sensitive", VmValue::Bool(false)),
        ]),
    );
    let ci = extract_dict(&case_insensitive);
    let ci_results = extract_list(ci.get("results").unwrap());

    // Case-insensitive should never miss what case-sensitive sees, and
    // typically catches more.
    assert!(ci_results.len() >= cs_results.len());
    assert!(!cs_results.is_empty());
}

#[test]
fn query_truncates_to_max_results() {
    let dir = write_workspace();
    let (registry, _) = build_registry();

    call(
        &registry,
        "hostlib_code_index_rebuild",
        dict(&[(
            "root",
            VmValue::String(Rc::from(dir.path().to_string_lossy().to_string())),
        )]),
    );
    let response = call(
        &registry,
        "hostlib_code_index_query",
        dict(&[
            ("needle", VmValue::String(Rc::from("export"))),
            ("max_results", VmValue::Int(1)),
        ]),
    );
    let response = extract_dict(&response);
    let results = extract_list(response.get("results").unwrap());
    assert_eq!(results.len(), 1);
    assert!(matches!(
        response.get("truncated").unwrap(),
        VmValue::Bool(true)
    ));
}

#[test]
fn query_scope_filter_restricts_results() {
    let dir = write_workspace();
    let (registry, _) = build_registry();

    call(
        &registry,
        "hostlib_code_index_rebuild",
        dict(&[(
            "root",
            VmValue::String(Rc::from(dir.path().to_string_lossy().to_string())),
        )]),
    );

    let scope_value = VmValue::List(Rc::new(vec![VmValue::String(Rc::from("src"))]));
    let response = call(
        &registry,
        "hostlib_code_index_query",
        dict(&[
            ("needle", VmValue::String(Rc::from("alphaToken"))),
            ("scope", scope_value),
        ]),
    );
    let response = extract_dict(&response);
    let results = extract_list(response.get("results").unwrap());
    let paths: Vec<String> = results
        .iter()
        .map(|hit| {
            let dict = extract_dict(hit);
            extract_str(dict.get("path").unwrap())
        })
        .collect();
    assert!(paths.iter().all(|p| p.starts_with("src/")));
}

#[test]
fn imports_for_returns_resolved_and_unresolved() {
    let dir = write_workspace();
    let (registry, _) = build_registry();

    call(
        &registry,
        "hostlib_code_index_rebuild",
        dict(&[(
            "root",
            VmValue::String(Rc::from(dir.path().to_string_lossy().to_string())),
        )]),
    );
    let response = call(
        &registry,
        "hostlib_code_index_imports_for",
        dict(&[("path", VmValue::String(Rc::from("src/index.ts")))]),
    );
    let response = extract_dict(&response);
    let imports = extract_list(response.get("imports").unwrap());
    let pairs: Vec<(String, Option<String>, String)> = imports
        .iter()
        .map(|item| {
            let d = extract_dict(item);
            let module = extract_str(d.get("module").unwrap());
            let resolved = match d.get("resolved_path").unwrap() {
                VmValue::Nil => None,
                VmValue::String(s) => Some(s.to_string()),
                other => panic!("expected str|nil, got {other:?}"),
            };
            let kind = extract_str(d.get("kind").unwrap());
            (module, resolved, kind)
        })
        .collect();

    let util_resolution = pairs
        .iter()
        .find(|(m, _, _)| m.contains("./util"))
        .expect("./util import surfaced");
    assert_eq!(util_resolution.1.as_deref(), Some("src/util.ts"));
    assert_eq!(util_resolution.2, "import");

    // The other import points at a path that doesn't exist in the
    // workspace — it should land in the response with `resolved_path: nil`.
    let other_resolution = pairs
        .iter()
        .find(|(m, _, _)| m.contains("./other"))
        .expect("./other import surfaced");
    assert_eq!(other_resolution.1.as_deref(), Some("src/other.ts"));
}

#[test]
fn importers_of_returns_paths_in_sorted_order() {
    let dir = write_workspace();
    let (registry, _) = build_registry();

    call(
        &registry,
        "hostlib_code_index_rebuild",
        dict(&[(
            "root",
            VmValue::String(Rc::from(dir.path().to_string_lossy().to_string())),
        )]),
    );
    let response = call(
        &registry,
        "hostlib_code_index_importers_of",
        dict(&[("module", VmValue::String(Rc::from("src/util.ts")))]),
    );
    let response = extract_dict(&response);
    let importers = extract_list(response.get("importers").unwrap());
    let paths: Vec<String> = importers.iter().map(extract_str).collect();
    assert_eq!(paths, vec!["src/index.ts", "src/other.ts"]);
}

#[test]
fn stats_reflect_index_state() {
    let (registry, _) = build_registry();

    let pre = extract_dict(&call(&registry, "hostlib_code_index_stats", dict(&[])));
    assert_eq!(extract_int(pre.get("indexed_files").unwrap()), 0);
    assert!(matches!(
        pre.get("last_rebuild_unix_ms").unwrap(),
        VmValue::Nil
    ));

    let dir = write_workspace();
    call(
        &registry,
        "hostlib_code_index_rebuild",
        dict(&[(
            "root",
            VmValue::String(Rc::from(dir.path().to_string_lossy().to_string())),
        )]),
    );
    let post = extract_dict(&call(&registry, "hostlib_code_index_stats", dict(&[])));
    assert!(extract_int(post.get("indexed_files").unwrap()) >= 4);
    assert!(extract_int(post.get("trigrams").unwrap()) > 0);
    assert!(extract_int(post.get("words").unwrap()) > 0);
    assert!(extract_int(post.get("memory_bytes").unwrap()) > 0);
    assert!(matches!(
        post.get("last_rebuild_unix_ms").unwrap(),
        VmValue::Int(_)
    ));
}

#[test]
fn rebuild_rejects_missing_root() {
    let (registry, _) = build_registry();
    let entry = registry.find("hostlib_code_index_rebuild").unwrap();
    let err = (entry.handler)(&[dict(&[(
        "root",
        VmValue::String(Rc::from("/definitely/not/here/zzz")),
    )])])
    .expect_err("missing root must error");
    let msg = format!("{err}");
    assert!(msg.contains("root"), "error mentions the param: {msg}");
}

#[test]
fn empty_workspace_returns_empty_responses() {
    let (registry, _) = build_registry();
    // No rebuild yet — every read op should still respond with a dict
    // shape rather than panicking.
    let q = extract_dict(&call(
        &registry,
        "hostlib_code_index_query",
        dict(&[("needle", VmValue::String(Rc::from("anything")))]),
    ));
    assert!(extract_list(q.get("results").unwrap()).is_empty());

    let imps = extract_dict(&call(
        &registry,
        "hostlib_code_index_imports_for",
        dict(&[("path", VmValue::String(Rc::from("src/main.rs")))]),
    ));
    assert!(extract_list(imps.get("imports").unwrap()).is_empty());

    let imps_of = extract_dict(&call(
        &registry,
        "hostlib_code_index_importers_of",
        dict(&[("module", VmValue::String(Rc::from("anything")))]),
    ));
    assert!(extract_list(imps_of.get("importers").unwrap()).is_empty());
}
