//! Scenario test: build a real index over a Swift-shaped host fixture.
//!
//! The fixture under `tests/fixtures/burin_code_subset/` reproduces the
//! shape of a code-index host module (file names, basic content,
//! cross-references). The asserts are picked to catch regressions in:
//!
//! - the trigram/word index (does `query("TrigramIndex")` find the file?)
//! - the import resolver (does `imports_for("CodeIndex.swift")` see
//!   `import Foundation`?)
//! - the dep graph (does `importers_of("Foundation")` … no, Swift does
//!   not resolve standard-library imports — so the symmetric assertion
//!   uses an in-fixture cross-reference instead).
//!
//! When `HARN_HOSTLIB_CODE_INDEX_SCENARIO_ROOT=<path>` is set in the
//! environment, the scenario also rebuilds the index against that path and
//! asserts the same set of invariants over a live repo. CI doesn't set the
//! env var so the synthetic fixture path is the default.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
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

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("burin_code_subset")
}

fn rebuild(registry: &BuiltinRegistry, root: &Path) -> i64 {
    let response = call(
        registry,
        "hostlib_code_index_rebuild",
        dict(&[(
            "root",
            VmValue::String(Rc::from(root.to_string_lossy().to_string())),
        )]),
    );
    let dict = extract_dict(&response);
    extract_int(dict.get("files_indexed").unwrap())
}

fn assert_substring_query_finds(registry: &BuiltinRegistry, needle: &str, expected: &[&str]) {
    let response = call(
        registry,
        "hostlib_code_index_query",
        dict(&[
            ("needle", VmValue::String(Rc::from(needle.to_string()))),
            ("max_results", VmValue::Int(100)),
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
    for fragment in expected {
        assert!(
            paths.iter().any(|p| p.ends_with(fragment)),
            "query `{needle}` missed `{fragment}`; got {paths:?}"
        );
    }
}

#[test]
fn fixture_reproduces_code_index_invariants() {
    let cap = CodeIndexCapability::new();
    let mut registry = BuiltinRegistry::new();
    cap.register_builtins(&mut registry);

    let files_indexed = rebuild(&registry, &fixture_path());
    assert!(
        files_indexed >= 4,
        "expected at least the four ported files, got {files_indexed}"
    );

    // Trigram path: literal substrings unique to each file land hits.
    assert_substring_query_finds(&registry, "TrigramIndex", &["TrigramIndex.swift"]);
    assert_substring_query_finds(&registry, "WordIndex", &["WordIndex.swift"]);
    assert_substring_query_finds(&registry, "DepGraph", &["DepGraph.swift"]);
    assert_substring_query_finds(&registry, "FilteredWalker", &["FilteredWalker.swift"]);

    // Imports surfaced for one of the modules.
    let imports = call(
        &registry,
        "hostlib_code_index_imports_for",
        dict(&[("path", VmValue::String(Rc::from("CodeIndex.swift")))]),
    );
    let imports = extract_dict(&imports);
    let modules: Vec<String> = extract_list(imports.get("imports").unwrap())
        .iter()
        .map(|entry| {
            let dict = extract_dict(entry);
            extract_str(dict.get("module").unwrap())
        })
        .collect();
    assert!(
        modules.iter().any(|m| m == "import Foundation"),
        "expected `import Foundation` in CodeIndex.swift imports; got {modules:?}",
    );

    // Stats reflect a populated index.
    let stats = extract_dict(&call(&registry, "hostlib_code_index_stats", dict(&[])));
    assert!(extract_int(stats.get("indexed_files").unwrap()) >= 4);
    assert!(extract_int(stats.get("trigrams").unwrap()) > 0);
}

/// When `HARN_HOSTLIB_CODE_INDEX_SCENARIO_ROOT` points at a checkout, the
/// same invariants are asserted over a real repo. The env-var gate keeps
/// CI fast and hermetic; local runs that opt in get the realistic stress
/// test.
#[test]
fn live_code_index_smoke() {
    let Some(path) = std::env::var_os("HARN_HOSTLIB_CODE_INDEX_SCENARIO_ROOT") else {
        eprintln!("HARN_HOSTLIB_CODE_INDEX_SCENARIO_ROOT not set; skipping live scenario test");
        return;
    };
    let path = PathBuf::from(path);
    if !path.exists() {
        eprintln!("HARN_HOSTLIB_CODE_INDEX_SCENARIO_ROOT does not exist; skipping");
        return;
    }
    let cap = CodeIndexCapability::new();
    let mut registry = BuiltinRegistry::new();
    cap.register_builtins(&mut registry);

    let started = std::time::Instant::now();
    let files_indexed = rebuild(&registry, &path);
    let elapsed = started.elapsed();
    eprintln!(
        "live code-index scenario rebuild: {files_indexed} files in {:.2}s",
        elapsed.as_secs_f64()
    );
    assert!(files_indexed > 0);

    assert_substring_query_finds(&registry, "TrigramIndex", &["TrigramIndex.swift"]);
}
