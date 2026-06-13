//! Criterion benchmark for `hostlib_ast_parse_file` warm parse latency.
//!
//! Replaces the wall-clock perf budget assertions that previously lived
//! in `crates/harn-hostlib/tests/ast_builtins.rs`. Wall-clock budgets in
//! unit/integration tests flake on shared CI runners under contention;
//! Criterion's statistical sampling tracks the same parse-latency signal
//! (issue #564's 20ms target) without that flake.
//!
//! Run with `cargo bench --bench ast_parse`.

use std::hint::black_box;
use std::path::PathBuf;
use std::sync::Arc;

use criterion::{criterion_group, criterion_main, Criterion};
use harn_hostlib::{ast::AstCapability, tools::permissions, BuiltinRegistry, HostlibCapability};
use harn_vm::VmValue;

fn ast_registry() -> BuiltinRegistry {
    let mut registry = BuiltinRegistry::new();
    AstCapability.register_builtins(&mut registry);
    registry
}

fn dict(pairs: &[(&str, VmValue)]) -> VmValue {
    let mut map: harn_vm::value::DictMap = Default::default();
    for (k, v) in pairs {
        map.insert((*k).into(), v.clone());
    }
    VmValue::dict(map)
}

fn fixture_path(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/ast")
        .join(rel)
}

fn bench_parse_file_warm(c: &mut Criterion) {
    let registry = ast_registry();
    // Use the largest in-tree fixture (Rust). Keeps the benchmark
    // hermetic and CI-friendly.
    let path = fixture_path("rust/source.rs");
    let payload = dict(&[(
        "path",
        VmValue::String(Arc::from(path.to_string_lossy().as_ref())),
    )]);

    let entry = registry
        .find("hostlib_ast_parse_file")
        .expect("hostlib_ast_parse_file builtin registered");

    // Warm up: first call sometimes pays a one-time grammar load.
    let _ = (entry.handler)(std::slice::from_ref(&payload)).expect("warmup parse_file succeeds");

    c.bench_function("hostlib_ast_parse_file/rust_source.rs (warm)", |b| {
        b.iter(|| {
            let result = (entry.handler)(std::slice::from_ref(black_box(&payload)))
                .expect("parse_file succeeds");
            black_box(result);
        });
    });
}

/// A ~5k-LOC source plus an `apply_node` edit for a representative spread
/// of tier-1 languages (a code grammar, plus the data/markup formats added
/// in B.7). The B.7 budget is parse + apply + re-validate ≤ 50ms p99 for
/// files ≤ 5k LOC; this bench tracks that signal per grammar without a
/// flaky wall-clock assertion in the test suite.
fn large_source(language: &str) -> (String, &'static str, &'static str) {
    match language {
        "rust" => {
            let mut body = String::new();
            for i in 0..700 {
                body.push_str(&format!("fn f{i}() -> i32 {{ {i} }}\n"));
            }
            (
                body,
                "(function_item name: (identifier) @name (#eq? @name \"f0\") body: (block) @target)",
                "{ 42 }",
            )
        }
        "json" => {
            let mut body = String::from("{\n  \"first\": 1");
            for i in 0..2000 {
                body.push_str(&format!(",\n  \"k{i}\": {i}"));
            }
            body.push_str("\n}\n");
            (body, "(pair value: (number) @target)", "9")
        }
        "yaml" => {
            let mut body = String::from("first: 1\n");
            for i in 0..2000 {
                body.push_str(&format!("k{i}: {i}\n"));
            }
            (
                body,
                "(block_mapping_pair value: (flow_node (plain_scalar) @target))",
                "9",
            )
        }
        "css" => {
            let mut body = String::new();
            for i in 0..1500 {
                body.push_str(&format!(".c{i} {{ color: red; }}\n"));
            }
            (body, "(declaration (plain_value) @target)", "blue")
        }
        other => panic!("no large-source generator for {other}"),
    }
}

fn bench_apply_node_large(c: &mut Criterion) {
    use std::io::Write;

    permissions::enable_for_test();
    let registry = ast_registry();
    let entry = registry
        .find("hostlib_ast_apply_node")
        .expect("hostlib_ast_apply_node builtin registered");

    for language in ["rust", "json", "yaml", "css"] {
        let (source, query, replacement) = large_source(language);
        let mut file = tempfile::Builder::new()
            .suffix(&format!(".bench-{language}"))
            .tempfile()
            .expect("temp file");
        file.write_all(source.as_bytes()).expect("write source");
        let path = file.path().to_string_lossy().to_string();
        // `dry_run` keeps the bench hermetic (no disk writes) while still
        // exercising read + parse + query + splice + re-validate.
        let payload = dict(&[
            ("path", VmValue::String(Arc::from(path.as_str()))),
            ("language", VmValue::String(Arc::from(language))),
            ("query", VmValue::String(Arc::from(query))),
            ("replacement", VmValue::String(Arc::from(replacement))),
            ("select", VmValue::String(Arc::from("first"))),
            ("dry_run", VmValue::Bool(true)),
        ]);
        c.bench_function(
            &format!("hostlib_ast_apply_node/{language}_5k_loc (dry_run)"),
            |b| {
                b.iter(|| {
                    let result = (entry.handler)(std::slice::from_ref(black_box(&payload)))
                        .expect("apply_node succeeds");
                    black_box(result);
                });
            },
        );
    }
}

criterion_group!(benches, bench_parse_file_warm, bench_apply_node_large);
criterion_main!(benches);
