use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use harn_vm::bench_internals::{DirectCallStateReadFixture, DIRECT_CALL_STATE_READ_COUNTS};

/// Microbench for the direct-call inline-cache read path. Fires on
/// every `Op::Call` (closure-by-value callee) and the named-fn fast
/// path inside `Op::CallBuiltin` — i.e. every user-fn invocation in
/// a Harn program after warmup.
///
/// `direct_call_state_read/copy_peek` exercises `peek_direct_call_state`,
/// returning just the inner `DirectCallState` (still clones because it
/// holds the `Arc<VmClosure>` cached target, but skips the wrapping
/// `InlineCacheEntry` discriminant init and padding-to-largest-variant
/// memcpy). `direct_call_state_read/clone_control` exercises the
/// full `inline_cache_entry` path that clones the complete
/// `InlineCacheEntry` enum on every dispatch.
fn bench_direct_call_state_read(c: &mut Criterion) {
    let fixtures = DIRECT_CALL_STATE_READ_COUNTS
        .into_iter()
        .map(DirectCallStateReadFixture::new)
        .collect::<Vec<_>>();

    let mut optimized = c.benchmark_group("direct_call_state_read/copy_peek");
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

    let mut baseline = c.benchmark_group("direct_call_state_read/clone_control");
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

criterion_group!(benches, bench_direct_call_state_read);
criterion_main!(benches);
