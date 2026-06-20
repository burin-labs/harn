//! Criterion benchmark for `hostlib_code_index_cypher` p95 latency.
//!
//! Issue #2434's verification calls for p95 < 50 ms on a 100k-LOC fixture.
//! We synthesize that corpus in a tempdir on `criterion_group` setup
//! (cheap relative to the parse work the rebuild does), then sample the
//! Cypher executor against the same workspace across a representative
//! query mix:
//!
//! - direct-property lookup by name (the fast path);
//! - reverse traversal with a small variable-length hop;
//! - label-only scan + WHERE filter (the slow path).
//!
//! Criterion reports the median + outlier-filtered p95 of every benchmark.
//! Run with `cargo bench --bench code_index_cypher`.

use std::fs;
use std::hint::black_box;
use std::path::PathBuf;

use criterion::{criterion_group, criterion_main, Criterion};
use harn_hostlib::{
    code_index::CodeIndexCapability, BuiltinRegistry, HostlibCapability, RegisteredBuiltin,
};
use harn_vm::VmValue;

/// Approx. 100k LOC = 200 files × 500 lines each. Cheap enough to
/// rebuild once per benchmark group (~seconds), small enough to stay in
/// memory comfortably.
const FILE_COUNT: usize = 200;
const LINES_PER_FILE: usize = 500;

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

fn build_corpus() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("create tempdir");
    let root = dir.path().to_path_buf();
    fs::create_dir_all(root.join("src")).expect("mkdir src");
    for i in 0..FILE_COUNT {
        let mut src = String::with_capacity(LINES_PER_FILE * 32);
        // One module-level function + a body that calls the previous file's
        // function. This wires CALLS edges across the corpus so the
        // traversal benchmarks have non-trivial fan-out.
        src.push_str(&format!("pub fn fn_{i}() {{\n"));
        if i > 0 {
            src.push_str(&format!("    fn_{prev}();\n", prev = i - 1));
        }
        for j in 0..LINES_PER_FILE {
            src.push_str(&format!("    let v_{j} = {j};\n"));
        }
        src.push_str("}\n");
        // Pad with extra siblings so the per-file LOC matches the budget.
        for j in 0..4 {
            src.push_str(&format!("pub fn helper_{i}_{j}() {{}}\n"));
        }
        fs::write(root.join(format!("src/m{i}.rs")), src).expect("write file");
    }
    (dir, root)
}

fn cypher_query(reg: &BuiltinRegistry, query: &str) {
    let payload = dict(&[("query", VmValue::String(arcstr::ArcStr::from(query)))]);
    let response = call(reg, "hostlib_code_index_cypher", payload);
    black_box(response);
}

fn bench_cypher(c: &mut Criterion) {
    let (_dir, root) = build_corpus();
    let cap = CodeIndexCapability::new();
    let mut reg = BuiltinRegistry::new();
    cap.register_builtins(&mut reg);
    call(
        &reg,
        "hostlib_code_index_rebuild",
        dict(&[(
            "root",
            VmValue::String(arcstr::ArcStr::from(root.to_string_lossy().as_ref())),
        )]),
    );

    let mut group = c.benchmark_group("code_index_cypher");
    group.sample_size(20);

    group.bench_function("direct_by_name", |b| {
        b.iter(|| {
            cypher_query(
                &reg,
                "MATCH (f:Function {name: 'fn_42'}) RETURN f.path AS path",
            );
        });
    });

    group.bench_function("var_length_called_by", |b| {
        b.iter(|| {
            cypher_query(
                &reg,
                "MATCH (f:Function {name: 'fn_10'})<-[:CALLS*1..2]-(c:CallSite) RETURN c.path AS path",
            );
        });
    });

    group.bench_function("label_scan_with_where", |b| {
        b.iter(|| {
            cypher_query(
                &reg,
                "MATCH (f:Function) WHERE f.name = 'fn_99' RETURN f.path AS path",
            );
        });
    });

    group.finish();
}

criterion_group!(benches, bench_cypher);
criterion_main!(benches);
