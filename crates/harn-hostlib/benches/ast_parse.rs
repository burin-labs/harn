//! Benchmark for the `hostlib_ast_parse_file` builtin warm-path latency.
//!
//! Replaces the `parse_file_meets_perf_budget_on_a_known_input` integration
//! test (issue #564's 20ms target). Wall-clock budgets in unit tests flake on
//! shared CI runners under contention; Criterion's statistical sampling tracks
//! the same regression signal without that flake.
//!
//! Run with: `cargo bench --bench ast_parse`.

use std::collections::BTreeMap;
use std::hint::black_box;
use std::path::PathBuf;
use std::rc::Rc;
use std::slice;

use criterion::{criterion_group, criterion_main, Criterion};
use harn_hostlib::{ast::AstCapability, BuiltinRegistry, HostlibCapability};
use harn_vm::VmValue;

fn fixture_path(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/ast")
        .join(rel)
}

fn dict(pairs: &[(&str, VmValue)]) -> VmValue {
    let mut map: BTreeMap<String, VmValue> = BTreeMap::new();
    for (k, v) in pairs {
        map.insert((*k).into(), v.clone());
    }
    VmValue::Dict(Rc::new(map))
}

fn ast_registry() -> BuiltinRegistry {
    let mut registry = BuiltinRegistry::new();
    AstCapability.register_builtins(&mut registry);
    registry
}

fn parse_file_warm(c: &mut Criterion) {
    let registry = ast_registry();
    let path = fixture_path("rust/source.rs");
    let payload = dict(&[(
        "path",
        VmValue::String(Rc::from(path.to_string_lossy().as_ref())),
    )]);
    let entry = registry
        .find("hostlib_ast_parse_file")
        .expect("parse_file builtin registered");

    // Warm up: first call pays a one-time grammar load.
    let _ = (entry.handler)(slice::from_ref(&payload)).expect("warmup parse");

    c.bench_function("ast_parse_file_warm_rust_source", |b| {
        b.iter(|| {
            let result = (entry.handler)(black_box(slice::from_ref(&payload)))
                .expect("parse_file should succeed");
            black_box(result);
        });
    });
}

criterion_group!(benches, parse_file_warm);
criterion_main!(benches);
