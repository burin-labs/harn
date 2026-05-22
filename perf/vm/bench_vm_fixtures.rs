use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use harn_vm_perf::{allocation_stats_per_iteration_batched, emit_allocation_jsonl};
use tokio::runtime::{Builder, Runtime};

const FIXTURES: &[&str] = &[
    "agent_tool_dispatch.harn",
    "arithmetic_loop.harn",
    "comparison_loop.harn",
    "dict_merge_loop.harn",
    "dict_property_read.harn",
    "dict_subscript_assign.harn",
    "filter_nil_loop.harn",
    "function_call_loop.harn",
    "list_iteration.harn",
    "list_map_filter.harn",
    "local_variable_lookup.harn",
    "method_call_dispatch.harn",
    "recursive_countdown.harn",
    "string_interpolation_loop.harn",
    "struct_field_read.harn",
];

struct Fixture {
    name: String,
    path: PathBuf,
    source: String,
    chunk: harn_vm::Chunk,
}

fn runtime() -> Runtime {
    Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("benchmark runtime")
}

fn load_fixture(name: &str) -> Fixture {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(name);
    let source = std::fs::read_to_string(&path).expect("benchmark fixture should be readable");
    let chunk = harn_vm::compile_source(&source).expect("benchmark fixture should compile");
    Fixture {
        name: name.trim_end_matches(".harn").to_string(),
        path,
        source,
        chunk,
    }
}

fn setup_vm(fixture: &Fixture) -> harn_vm::Vm {
    harn_vm::reset_thread_local_state();
    let mut vm = harn_vm::Vm::new();
    harn_vm::register_vm_stdlib(&mut vm);
    vm.set_source_info(&fixture.path.to_string_lossy(), &fixture.source);
    if let Some(parent) = fixture.path.parent() {
        vm.set_source_dir(parent);
    }
    vm
}

fn execute_fixture(runtime: &Runtime, fixture: &Fixture, mut vm: harn_vm::Vm) {
    runtime.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let result = vm.execute(&fixture.chunk).await;
                black_box(result.expect("benchmark fixture should execute"));
            })
            .await;
    });
}

fn allocation_stats_per_run(runtime: &Runtime, fixture: &Fixture, samples: u64) {
    let stats = allocation_stats_per_iteration_batched(
        samples,
        || setup_vm(fixture),
        |vm| execute_fixture(runtime, fixture, vm),
    );
    emit_allocation_jsonl("vm_fixtures", &fixture.name, samples, stats);
}

fn timed_runs(runtime: &Runtime, fixture: &Fixture, iterations: u64) -> Duration {
    let mut total = Duration::ZERO;
    for _ in 0..iterations {
        let vm = setup_vm(fixture);
        let started = Instant::now();
        execute_fixture(runtime, fixture, vm);
        total += started.elapsed();
    }
    total
}

fn bench_vm_fixtures(c: &mut Criterion) {
    let runtime = runtime();
    let fixtures = FIXTURES
        .iter()
        .map(|name| load_fixture(name))
        .collect::<Vec<_>>();

    let mut group = c.benchmark_group("vm_fixtures");
    for fixture in &fixtures {
        allocation_stats_per_run(&runtime, fixture, 10);

        group.bench_with_input(
            BenchmarkId::from_parameter(&fixture.name),
            fixture,
            |b, fixture| {
                b.iter_custom(|iterations| timed_runs(&runtime, black_box(fixture), iterations));
            },
        );
    }
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(10);
    targets = bench_vm_fixtures
}
criterion_main!(benches);
