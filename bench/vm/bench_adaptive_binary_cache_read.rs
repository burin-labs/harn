use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use harn_vm::bench_internals::{AdaptiveBinaryCacheReadFixture, ADAPTIVE_BINARY_CACHE_READ_COUNTS};

/// Microbench for the adaptive-binary inline-cache read path. The read
/// fires on every dispatch of every adaptive binary op (Add / Sub /
/// Mul / Div / Mod / Eq / Neq / Less / Greater / LessEq / GreaterEq) —
/// the hottest opcode class in the VM dispatch loop.
///
/// `adaptive_binary_cache_read/frame_index_peek` exercises the production
/// frame-local cache-set lookup and `peek_adaptive_binary_cache_by_index`.
/// `adaptive_binary_cache_read/hash_lookup_clone_control` exercises the old
/// per-dispatch hash lookup plus full `InlineCacheEntry` clone.
///
/// The N axis (8/32/128/512) approximates a small predicate, a loop
/// body, and a deep stdlib fn body. Per-op savings compound across
/// the millions of dispatches a typical loop body issues.
fn bench_adaptive_binary_cache_read(c: &mut Criterion) {
    let fixtures = ADAPTIVE_BINARY_CACHE_READ_COUNTS
        .into_iter()
        .map(AdaptiveBinaryCacheReadFixture::new)
        .collect::<Vec<_>>();

    let mut optimized = c.benchmark_group("adaptive_binary_cache_read/frame_index_peek");
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

    let mut baseline = c.benchmark_group("adaptive_binary_cache_read/hash_lookup_clone_control");
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

criterion_group!(benches, bench_adaptive_binary_cache_read);
criterion_main!(benches);
