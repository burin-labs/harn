use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use harn_vm::bench_internals::{InlineCacheSlotLookupFixture, INLINE_CACHE_LOOKUP_COUNTS};

/// Microbench for `Chunk::inline_cache_slot`. The lookup fires once per
/// dispatch of every adaptive binary op (Add/Sub/Mul/Div/Mod/Eq/Neq/
/// Less/Greater/LessEq/GreaterEq), every `Op::Call`, every
/// `Op::MethodCall(Opt)`, and every `Op::GetProperty(Opt)`. The fixture
/// sweeps a chunk of N adjacent `Op::Add` slots and accumulates the
/// resolved IDs — directly mirroring the dispatcher's
/// `op_offset = ip - 1; chunk.inline_cache_slot(op_offset)` shape.
///
/// The N axis (8/32/128/512) approximates a small predicate, a loop
/// body, and a deep stdlib fn body. The pre-optimization
/// `BTreeMap::get` had to walk one or more tree nodes per lookup, so
/// per-op cost grew with chunk size; the flat-index `Vec<u32>` index
/// stays roughly constant per lookup, scaling only with N (which is the
/// sweep iteration count, not the data-structure cost).
fn bench_inline_cache_slot(c: &mut Criterion) {
    let fixtures = INLINE_CACHE_LOOKUP_COUNTS
        .into_iter()
        .map(InlineCacheSlotLookupFixture::new)
        .collect::<Vec<_>>();

    let mut optimized = c.benchmark_group("inline_cache_slot/flat_vec");
    for fixture in &fixtures {
        let benchmark = format!("ops_{:03}", fixture.op_count());
        optimized.bench_with_input(
            BenchmarkId::from_parameter(benchmark),
            fixture,
            |b, fixture| {
                b.iter(|| black_box(fixture.invoke()));
            },
        );
    }
    optimized.finish();

    let mut baseline = c.benchmark_group("inline_cache_slot/btreemap_control");
    for fixture in &fixtures {
        let benchmark = format!("ops_{:03}", fixture.op_count());
        baseline.bench_with_input(
            BenchmarkId::from_parameter(benchmark),
            fixture,
            |b, fixture| {
                b.iter(|| black_box(fixture.invoke_btreemap_control()));
            },
        );
    }
    baseline.finish();
}

criterion_group!(benches, bench_inline_cache_slot);
criterion_main!(benches);
