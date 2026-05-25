//! Criterion benchmark for `hostlib_ast_parse_file` warm parse latency.
//!
//! Replaces the wall-clock perf budget assertions that previously lived
//! in `crates/harn-hostlib/tests/ast_builtins.rs`. Wall-clock budgets in
//! unit/integration tests flake on shared CI runners under contention;
//! Criterion's statistical sampling tracks the same parse-latency signal
//! (issue #564's 20ms target) without that flake.
//!
//! Run with `cargo bench --bench ast_parse`.

use std::collections::BTreeMap;
use std::hint::black_box;
use std::path::PathBuf;
use std::rc::Rc;

use criterion::{criterion_group, criterion_main, Criterion};
use harn_hostlib::{ast::AstCapability, BuiltinRegistry, HostlibCapability};
use harn_vm::VmValue;

fn ast_registry() -> BuiltinRegistry {
    let mut registry = BuiltinRegistry::new();
    AstCapability.register_builtins(&mut registry);
    registry
}

fn dict(pairs: &[(&str, VmValue)]) -> VmValue {
    let mut map: BTreeMap<String, VmValue> = BTreeMap::new();
    for (k, v) in pairs {
        map.insert((*k).into(), v.clone());
    }
    VmValue::Dict(Rc::new(map))
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
        VmValue::String(Rc::from(path.to_string_lossy().as_ref())),
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

criterion_group!(benches, bench_parse_file_warm);
criterion_main!(benches);
