use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use harn_vm::bench_internals::{MethodCacheReadFixture, METHOD_CACHE_READ_COUNTS};

/// Microbench for the method-cache inline-cache read path. The read
/// fires on every dispatch of every `Op::MethodCall`, `Op::MethodCallOpt`,
/// and `Op::MethodCallSpread` — the dominant opcode class for any
/// chained-collection pipeline (`xs.filter(...).map(...).count()`,
/// `s.contains(...)`, etc.), which most Harn user code exercises.
///
/// `method_cache_read/frame_index_peek` exercises the production frame-local
/// cache-set lookup and `peek_method_cache_by_index`. `method_cache_read/
/// hash_lookup_clone_control` exercises the old per-dispatch hash lookup plus
/// full `InlineCacheEntry` clone.
///
/// The N axis (8/32/128/512) approximates a small predicate, a loop
/// body, and a deep stdlib pipeline. Per-op savings compound across
/// the millions of method calls a typical pipeline issues.
fn bench_method_cache_read(c: &mut Criterion) {
    let fixtures = METHOD_CACHE_READ_COUNTS
        .into_iter()
        .map(MethodCacheReadFixture::new)
        .collect::<Vec<_>>();

    let mut optimized = c.benchmark_group("method_cache_read/frame_index_peek");
    for fixture in &fixtures {
        let benchmark = format!("ops_{:03}", fixture.op_count());
        optimized.bench_with_input(
            BenchmarkId::from_parameter(benchmark),
            fixture,
            |b, fixture| {
                b.iter(|| black_box(fixture.invoke_peek()));
            },
        );
    }
    optimized.finish();

    let mut baseline = c.benchmark_group("method_cache_read/hash_lookup_clone_control");
    for fixture in &fixtures {
        let benchmark = format!("ops_{:03}", fixture.op_count());
        baseline.bench_with_input(
            BenchmarkId::from_parameter(benchmark),
            fixture,
            |b, fixture| {
                b.iter(|| black_box(fixture.invoke_clone_control()));
            },
        );
    }
    baseline.finish();
}

criterion_group!(benches, bench_method_cache_read);
criterion_main!(benches);
