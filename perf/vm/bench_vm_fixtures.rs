use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use tokio::runtime::{Builder, Runtime};

const FIXTURES: &[&str] = &[
    "agent_tool_dispatch.harn",
    "arithmetic_loop.harn",
    "comparison_loop.harn",
    "dict_property_read.harn",
    "function_call_loop.harn",
    "list_iteration.harn",
    "list_map_filter.harn",
    "local_variable_lookup.harn",
    "method_call_dispatch.harn",
    "recursive_countdown.harn",
    "string_interpolation_loop.harn",
    "struct_field_read.harn",
];

static TRACK_ALLOCATIONS: AtomicBool = AtomicBool::new(false);
static ALLOCATION_COUNT: AtomicU64 = AtomicU64::new(0);
static ALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);

struct CountingAllocator;

#[global_allocator]
static GLOBAL_ALLOCATOR: CountingAllocator = CountingAllocator;

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        record_allocation(ptr, layout.size());
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc_zeroed(layout) };
        record_allocation(ptr, layout.size());
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let ptr = unsafe { System.realloc(ptr, layout, new_size) };
        record_allocation(ptr, new_size);
        ptr
    }
}

fn record_allocation(ptr: *mut u8, bytes: usize) {
    if !ptr.is_null() && TRACK_ALLOCATIONS.load(Ordering::Relaxed) {
        ALLOCATION_COUNT.fetch_add(1, Ordering::Relaxed);
        ALLOCATED_BYTES.fetch_add(bytes as u64, Ordering::Relaxed);
    }
}

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

fn allocation_stats_per_run(runtime: &Runtime, fixture: &Fixture, samples: u64) -> (f64, f64) {
    ALLOCATION_COUNT.store(0, Ordering::Relaxed);
    ALLOCATED_BYTES.store(0, Ordering::Relaxed);
    for _ in 0..samples {
        let vm = setup_vm(fixture);
        TRACK_ALLOCATIONS.store(true, Ordering::Relaxed);
        execute_fixture(runtime, fixture, vm);
        TRACK_ALLOCATIONS.store(false, Ordering::Relaxed);
    }
    (
        ALLOCATION_COUNT.load(Ordering::Relaxed) as f64 / samples as f64,
        ALLOCATED_BYTES.load(Ordering::Relaxed) as f64 / samples as f64,
    )
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
        let (allocations, allocated_bytes) = allocation_stats_per_run(&runtime, fixture, 10);
        eprintln!(
            "vm_fixtures/{}: {:.2} allocations/run, {:.1} allocated bytes/run",
            fixture.name, allocations, allocated_bytes
        );

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
