#![recursion_limit = "256"]
//! End-to-end recall test for `std/code_librarian` against the same
//! 30-question fixture the #2434 ground-truth recall test uses
//! (`tests/fixtures/code_index_queries/queries.json` plus the sibling
//! `corpus/` workspace).
//!
//! The library's `code_librarian_query` is meant to be a thin typed
//! pass-through over `hostlib_code_index_cypher`. This test pins that
//! contract: the librarian must deliver at least the same aggregate
//! recall the raw Cypher executor does (≥ 80% per #2434).

use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;

use harn_lexer::Lexer;
use harn_parser::Parser;
use harn_vm::{register_vm_stdlib, Compiler, Vm, VmValue};

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/code_index_queries")
}

fn run_harn(source: &str) -> VmValue {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut lexer = Lexer::new(source);
                let tokens = lexer.tokenize().expect("tokenize");
                let mut parser = Parser::new(tokens);
                let program = parser.parse().expect("parse");
                let chunk = Compiler::new().compile(&program).expect("compile");
                let mut vm = Vm::new();
                register_vm_stdlib(&mut vm);
                let _ = harn_hostlib::install_default(&mut vm);
                vm.execute(&chunk).await.expect("execute")
            })
            .await
    })
}

fn extract_list(value: &VmValue) -> std::sync::Arc<Vec<VmValue>> {
    match value {
        VmValue::List(l) => l.clone(),
        other => panic!("expected list, got {other:?}"),
    }
}

fn extract_dict(value: &VmValue) -> std::sync::Arc<harn_vm::value::DictMap> {
    match value {
        VmValue::Dict(d) => d.clone(),
        other => panic!("expected dict, got {other:?}"),
    }
}

#[test]
fn librarian_query_meets_ground_truth_recall() {
    let corpus = fixture_root().join("corpus");
    let root_str = corpus.to_string_lossy().replace('\\', "/");

    let qa_text =
        std::fs::read_to_string(fixture_root().join("queries.json")).expect("read queries.json");
    let qa: serde_json::Value = serde_json::from_str(&qa_text).expect("parse queries.json");
    let questions = qa["questions"].as_array().expect("questions array");
    assert_eq!(
        questions.len(),
        30,
        "fixture must hold exactly 30 questions per the issue"
    );

    // Render the queries into a Harn source that drives them all through
    // `code_librarian_query` and returns a list-of-lists of match keys.
    let mut query_lines = String::new();
    for q in questions {
        let cypher = q["cypher"].as_str().unwrap();
        let match_key = q["match_key"].as_str().unwrap();
        // Inline-escape; the queries.json values use only single quotes
        // and no backslashes, so a verbatim drop is safe. Defensive check
        // catches any future fixture additions that violate this.
        assert!(
            !cypher.contains('"') && !cypher.contains('\\'),
            "cypher contains chars that need escaping: {cypher}"
        );
        assert!(
            !match_key.contains('"') && !match_key.contains('\\'),
            "match_key contains chars that need escaping: {match_key}"
        );
        query_lines.push_str(&format!(
            "rows = rows + [project(code_librarian_query(\"{cypher}\").rows, \"{match_key}\")]\n"
        ));
    }
    let source = format!(
        r#"
import "std/code_librarian"

fn project(rows, key) {{
  let out = []
  for row in rows {{
    let v = row?[key]
    if v != nil {{
      out = out + [to_string(v)]
    }}
  }}
  return out
}}

let _ = hostlib_code_index_rebuild({{ root: "{root_str}" }})
let rows = []
{query_lines}
return rows
"#
    );

    let result = run_harn(&source);
    let rows = extract_list(&result);
    assert_eq!(rows.len(), questions.len(), "row-set count must match");

    let mut total_expected: usize = 0;
    let mut total_found: usize = 0;
    let mut failures: Vec<String> = Vec::new();
    for (q, row_value) in questions.iter().zip(rows.iter()) {
        let id = q["id"].as_str().unwrap();
        let expected: BTreeSet<String> = q["expected"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();

        let found: HashSet<String> = extract_list(row_value)
            .iter()
            .filter_map(|v| match v {
                VmValue::String(s) => Some(s.to_string()),
                _ => None,
            })
            .collect();

        let hits = expected.iter().filter(|e| found.contains(*e)).count();
        total_expected += expected.len();
        total_found += hits;
        if hits < expected.len() {
            failures.push(format!(
                "[{id}] missing {missing}/{exp} (got {found:?}, want {expected:?})",
                missing = expected.len() - hits,
                exp = expected.len(),
            ));
        }
    }

    let recall = total_found as f64 / total_expected as f64;
    if recall < 0.80 {
        for f in &failures {
            eprintln!("{f}");
        }
        panic!(
            "code_librarian_query recall = {recall:.3} (found {total_found}/{total_expected}); \
             expected >= 0.80. failures above."
        );
    }
}

#[test]
fn librarian_who_calls_returns_call_sites() {
    // Reuse the same fixture: `fetchUser` is called from
    // `src/auth.ts` and `src/router.ts` per Q19 in queries.json.
    let corpus = fixture_root().join("corpus");
    let root_str = corpus.to_string_lossy().replace('\\', "/");
    let source = format!(
        r#"
import "std/code_librarian"
let _ = hostlib_code_index_rebuild({{ root: "{root_str}" }})
let callers = code_librarian_who_calls("fetchUser")
let paths = []
for c in callers {{
  paths = paths + [c.path]
}}
let symbols = []
for c in callers {{
  symbols = symbols + [c.symbol]
}}
return {{
  count: len(callers),
  paths: paths,
  symbols: symbols,
}}
"#
    );
    let result = run_harn(&source);
    let d = extract_dict(&result);
    let count = match d.get("count").unwrap() {
        VmValue::Int(i) => *i,
        other => panic!("count: {other:?}"),
    };
    assert!(
        count >= 2,
        "expected at least two callers for fetchUser, got {count}"
    );
    let paths: BTreeSet<String> = extract_list(d.get("paths").unwrap())
        .iter()
        .filter_map(|v| match v {
            VmValue::String(s) => Some(s.to_string()),
            _ => None,
        })
        .collect();
    assert!(
        paths.contains("src/auth.ts") && paths.contains("src/router.ts"),
        "expected paths to cover auth.ts + router.ts, got {paths:?}"
    );
    let symbols: BTreeSet<String> = extract_list(d.get("symbols").unwrap())
        .iter()
        .filter_map(|v| match v {
            VmValue::String(s) => Some(s.to_string()),
            _ => None,
        })
        .collect();
    assert_eq!(
        symbols,
        BTreeSet::from(["fetchUser".to_string()]),
        "every call site should report the target symbol"
    );
}

#[test]
fn librarian_freshness_and_overlay_roundtrip() {
    let corpus = fixture_root().join("corpus");
    let root_str = corpus.to_string_lossy().replace('\\', "/");
    let source = format!(
        r#"
import "std/code_librarian"
let _ = hostlib_code_index_rebuild({{ root: "{root_str}" }})
let fresh = code_librarian_freshness("src/util.ts")
let unknown = code_librarian_freshness("src/no-such-file.ts")
let on = code_librarian_branch_overlay("topic/test")
let off = code_librarian_branch_overlay(nil)
return {{
  fresh_known: fresh.known,
  fresh_stale: fresh.stale,
  unknown_known: unknown.known,
  on_active: on.active,
  off_active: off.active,
}}
"#
    );
    let result = run_harn(&source);
    let d = extract_dict(&result);
    let bool_at = |k: &str| match d.get(k).unwrap() {
        VmValue::Bool(b) => *b,
        other => panic!("{k}: {other:?}"),
    };
    assert!(bool_at("fresh_known"));
    assert!(!bool_at("fresh_stale"));
    assert!(!bool_at("unknown_known"));
    assert!(matches!(
        d.get("on_active").unwrap(),
        VmValue::String(s) if s.as_str() == "topic/test"
    ));
    assert!(matches!(d.get("off_active").unwrap(), VmValue::Nil));
}
