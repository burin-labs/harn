use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use harn_vm::bench_internals::{NonModuleClosureCallFixture, VMENV_CAPTURE_COUNTS};
use harn_vm_perf::{allocation_stats_per_iteration, emit_allocation_jsonl};

fn bench_vmenv_clone(c: &mut Criterion) {
    let fixtures = VMENV_CAPTURE_COUNTS
        .into_iter()
        .map(NonModuleClosureCallFixture::new)
        .collect::<Vec<_>>();

    let mut group = c.benchmark_group("vmenv_non_module_closure_call");
    for fixture in &fixtures {
        let benchmark = format!("captures_{:03}", fixture.capture_count());
        let allocation_stats = allocation_stats_per_iteration(10_000, || {
            black_box(fixture.invoke());
        });
        emit_allocation_jsonl(
            "vmenv_non_module_closure_call",
            &benchmark,
            10_000,
            allocation_stats,
        );

        group.bench_with_input(
            BenchmarkId::from_parameter(benchmark),
            fixture,
            |b, fixture| {
                b.iter(|| black_box(fixture.invoke()));
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_vmenv_clone);
criterion_main!(benches);
