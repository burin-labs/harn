use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use futures::future::join_all;
use harn_vm::orchestration::{
    clear_runtime_hooks, matching_vm_lifecycle_hooks, register_vm_hook, run_lifecycle_hooks,
    HookEvent,
};
use harn_vm::{register_vm_stdlib, reset_thread_local_state, with_async_builtin_ctx_sync, Vm};
use serde_json::{json, Value as JsonValue};
use tokio::runtime::{Builder, Runtime};

const FANOUTS: [usize; 4] = [1, 8, 32, 128];
const HOOK_EVENT: HookEvent = HookEvent::PreAgentTurn;

static TRACK_ALLOCATIONS: AtomicBool = AtomicBool::new(false);
static ALLOCATION_COUNT: AtomicU64 = AtomicU64::new(0);
static ALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);

struct CountingAllocator;

#[global_allocator]
static GLOBAL_ALLOCATOR: CountingAllocator = CountingAllocator;

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: This allocator only observes calls before delegating to the system allocator.
        let ptr = unsafe { System.alloc(layout) };
        record_allocation(ptr, layout.size());
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: This allocator only observes calls before delegating to the system allocator.
        let ptr = unsafe { System.alloc_zeroed(layout) };
        record_allocation(ptr, layout.size());
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: The pointer and layout are passed through unchanged from the allocator caller.
        unsafe { System.dealloc(ptr, layout) };
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: The pointer, layout, and new size are passed through unchanged.
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

#[derive(Clone)]
struct HookDispatchFixture {
    fanout: usize,
    payloads: Vec<JsonValue>,
}

fn runtime() -> Runtime {
    Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("benchmark runtime")
}

fn install_noop_hook(runtime: &Runtime) -> Vm {
    reset_thread_local_state();
    clear_runtime_hooks();

    let mut vm = Vm::new();
    register_vm_stdlib(&mut vm);
    let exports = runtime
        .block_on(vm.load_module_exports_from_source(
            "perf/orchestration/noop_hook.harn",
            "pub fn noop(event) {\n  return nil\n}\n",
        ))
        .expect("compile noop hook");
    let closure = exports.get("noop").expect("noop export").clone();
    register_vm_hook(HOOK_EVENT, "*", "bench::noop", closure);
    vm
}

fn payload(index: usize) -> JsonValue {
    let even = index.is_multiple_of(2);
    json!({
        "event": "trigger.dispatch",
        "target": format!("trigger.script_{index:03}"),
        "trigger": {
            "provider": if even { "cron" } else { "webhook" },
            "kind": if even { "schedule.tick" } else { "github.issue" },
            "dedupe_key": format!("bench-delivery-{index:03}"),
        },
        "script": {
            "path": format!("scripts/bench_{index:03}.harn"),
        },
    })
}

fn fixture(fanout: usize) -> HookDispatchFixture {
    let payloads: Vec<JsonValue> = (0..fanout).map(payload).collect();
    for payload in &payloads {
        assert_eq!(
            matching_vm_lifecycle_hooks(HOOK_EVENT, payload).len(),
            1,
            "benchmark fixture should dispatch each trigger payload into one hook"
        );
    }
    HookDispatchFixture { fanout, payloads }
}

async fn dispatch_fanout(payloads: &[JsonValue]) {
    let results = join_all(
        payloads
            .iter()
            .map(|payload| run_lifecycle_hooks(HOOK_EVENT, payload)),
    )
    .await;

    for result in results {
        result.expect("noop hook dispatch should succeed");
    }
}

fn measure_once(runtime: &Runtime, fixture: &HookDispatchFixture) {
    runtime.block_on(dispatch_fanout(&fixture.payloads));

    let started = Instant::now();
    runtime.block_on(dispatch_fanout(&fixture.payloads));
    let elapsed = started.elapsed();

    ALLOCATION_COUNT.store(0, Ordering::Relaxed);
    ALLOCATED_BYTES.store(0, Ordering::Relaxed);
    TRACK_ALLOCATIONS.store(true, Ordering::Relaxed);
    runtime.block_on(dispatch_fanout(&fixture.payloads));
    TRACK_ALLOCATIONS.store(false, Ordering::Relaxed);

    let hook_count = fixture.fanout as f64;
    let elapsed_ns = elapsed.as_nanos() as f64;
    let allocations = ALLOCATION_COUNT.load(Ordering::Relaxed) as f64;
    let allocated_bytes = ALLOCATED_BYTES.load(Ordering::Relaxed) as f64;
    eprintln!(
        "hook_dispatch/fanout_{:03} sample: {:.1} ns/event, {:.1} ns/hook, {:.2} allocations/event, {:.1} allocated bytes/event",
        fixture.fanout,
        elapsed_ns / hook_count,
        elapsed_ns / hook_count,
        allocations / hook_count,
        allocated_bytes / hook_count
    );
}

fn bench_hook_dispatch(c: &mut Criterion) {
    let runtime = runtime();
    let vm = install_noop_hook(&runtime);
    // Bind the VM as the async-builtin context for the whole bench: hook
    // dispatch resolves a `clone_async_builtin_child_vm` root, and `block_on`
    // polls inline on this thread so the sync scope is visible throughout.
    // (Replaces the old `install_async_builtin_child_vm` RAII guard.) harn#2667.
    with_async_builtin_ctx_sync(vm, || {
        let fixtures: Vec<HookDispatchFixture> = FANOUTS.into_iter().map(fixture).collect();
        for fixture in &fixtures {
            measure_once(&runtime, fixture);
        }

        let mut group = c.benchmark_group("hook_dispatch/noop_lifecycle_hook");
        for fixture in &fixtures {
            group.throughput(Throughput::Elements(fixture.fanout as u64));
            group.bench_with_input(
                BenchmarkId::from_parameter(format!("fanout_{:03}", fixture.fanout)),
                fixture,
                |b, fixture| {
                    b.iter(|| {
                        runtime.block_on(dispatch_fanout(black_box(&fixture.payloads)));
                    });
                },
            );
        }
        group.finish();
    });
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(20);
    targets = bench_hook_dispatch
}
criterion_main!(benches);
