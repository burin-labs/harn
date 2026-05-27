use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use harn_vm::bench_internals::{PropertyCacheReadFixture, PROPERTY_CACHE_READ_COUNTS};

/// Microbench for the property-cache inline-cache read path. Fires on
/// every `Op::GetProperty` / `Op::GetPropertyOpt` dispatch — the
/// dominant opcode for any field-read-heavy code (`obj.field`,
/// `xs.count`, `pair.0`, etc.).
///
/// `property_cache_read/copy_peek` exercises `peek_property_cache`,
/// returning just the `Property` payload (`u16 + PropertyCacheTarget`).
/// `property_cache_read/clone_control` exercises the pre-optimization
/// `inline_cache_entry` path that cloned the wrapping `InlineCacheEntry`
/// enum — a 32-48B memcpy (padded to the largest variant, `DirectCall`)
/// that `try_cached_property`'s `let-else` destructures and throws away.
fn bench_property_cache_read(c: &mut Criterion) {
    let fixtures = PROPERTY_CACHE_READ_COUNTS
        .into_iter()
        .map(PropertyCacheReadFixture::new)
        .collect::<Vec<_>>();

    let mut optimized = c.benchmark_group("property_cache_read/copy_peek");
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

    let mut baseline = c.benchmark_group("property_cache_read/clone_control");
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

criterion_group!(benches, bench_property_cache_read);
criterion_main!(benches);
