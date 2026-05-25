//! Ground-truth recall test for the typed symbol graph + Cypher executor.
//!
//! Loads the 30-question fixture in `tests/fixtures/code_index_queries/`,
//! rebuilds the sibling corpus through the public hostlib surface, runs
//! every Cypher query, and asserts that aggregate recall is ≥ 80% — the
//! threshold defined in issue #2434's acceptance criteria.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::PathBuf;
use std::rc::Rc;

use harn_hostlib::{
    code_index::CodeIndexCapability, BuiltinRegistry, HostlibCapability, RegisteredBuiltin,
};
use harn_vm::VmValue;

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/code_index_queries")
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

#[test]
fn ground_truth_recall_at_least_80_percent() {
    let cap = CodeIndexCapability::new();
    let mut registry = BuiltinRegistry::new();
    cap.register_builtins(&mut registry);

    let corpus = fixture_root().join("corpus");
    let root_str = corpus.to_string_lossy().into_owned();
    let _ = call(
        &registry,
        "hostlib_code_index_rebuild",
        dict(&[("root", VmValue::String(Rc::from(root_str.as_str())))]),
    );

    let qa_text =
        std::fs::read_to_string(fixture_root().join("queries.json")).expect("read queries.json");
    let qa: serde_json::Value = serde_json::from_str(&qa_text).expect("parse queries.json");
    let questions = qa["questions"].as_array().expect("questions array");
    assert_eq!(
        questions.len(),
        30,
        "fixture must hold exactly 30 questions per the issue"
    );

    let mut total_expected: usize = 0;
    let mut total_found: usize = 0;
    let mut failures: Vec<String> = Vec::new();
    for q in questions {
        let id = q["id"].as_str().unwrap();
        let cypher = q["cypher"].as_str().unwrap();
        let match_key = q["match_key"].as_str().unwrap();
        let expected: BTreeSet<String> = q["expected"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();

        let payload = dict(&[("query", VmValue::String(Rc::from(cypher)))]);
        let result = call(&registry, "hostlib_code_index_cypher", payload);
        let dict_view = extract_dict(&result);
        let rows = extract_list(dict_view.get("rows").expect("rows in cypher response"));

        let found: HashSet<String> = rows
            .iter()
            .filter_map(|row| {
                let m = match row {
                    VmValue::Dict(d) => d,
                    _ => return None,
                };
                m.get(match_key).and_then(|v| match v {
                    VmValue::String(s) => Some(s.to_string()),
                    VmValue::Int(n) => Some(n.to_string()),
                    _ => None,
                })
            })
            .collect();

        let hits = expected.iter().filter(|e| found.contains(*e)).count();
        total_expected += expected.len();
        total_found += hits;

        if hits < expected.len() {
            failures.push(format!(
                "[{id}] missing {missing}/{exp} for `{cypher}` (got {found:?})",
                missing = expected.len() - hits,
                exp = expected.len(),
                found = found
            ));
        }
    }

    let recall = total_found as f64 / total_expected as f64;
    if recall < 0.80 {
        for f in &failures {
            eprintln!("{f}");
        }
        panic!(
            "aggregate recall = {recall:.3} (found {total_found}/{total_expected}); \
             expected ≥ 0.80. failures above."
        );
    }
}
