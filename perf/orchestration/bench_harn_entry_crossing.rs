//! Rust↔`.harn` boundary-crossing microbenchmark.
//!
//! Regression gate for the `harn_entry` seam
//! (`crates/harn-vm/src/stdlib/harn_entry.rs`) ahead of the stage-loop
//! inversion re-architecture, which multiplies the number of crossings
//! (per tool call, per turn, per stage attempt). Two axes:
//!
//! - **entry point**: `call_harn_export_by_name` (`&[VmValue]`-direct)
//!   vs `call_harn_export_typed` (JSON in, JSON out, serde deserialize —
//!   the double-marshal path).
//! - **parent module cache**: *warm* (parent VM already instantiated the
//!   target module, children inherit it via Arc COW) vs *cold* (parent
//!   cache misses, so every crossing replays module instantiation into
//!   the child VM's copy and drops it on return — the
//!   `modules.rs` `instantiate_module` re-instantiation tax).
//!
//! The callee is `std/semver::parse("v1.2.3")` — pure and trivial — so
//! the numbers isolate crossing overhead, not callee work.

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use criterion::{criterion_group, criterion_main, Criterion};
use harn_vm::bench_internals::harn_entry_crossing::{self, ParsedVersion};
use harn_vm::{register_vm_stdlib, reset_thread_local_state, AsyncBuiltinCtx, Vm, VmValue};
use serde_json::json;
use tokio::runtime::{Builder, Runtime};

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

fn runtime() -> Runtime {
    Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("benchmark runtime")
}

fn vm_with_stdlib() -> Vm {
    reset_thread_local_state();
    let mut vm = Vm::new();
    register_vm_stdlib(&mut vm);
    vm
}

/// Parent VM whose module cache already carries the instantiated target
/// module: every child crossing takes the warm lookup path.
fn warm_ctx(runtime: &Runtime) -> AsyncBuiltinCtx {
    let mut vm = vm_with_stdlib();
    runtime
        .block_on(harn_entry_crossing::warm_parent_module_cache(
            &mut vm,
            harn_entry_crossing::IMPORT_PATH,
        ))
        .expect("warm the parent module cache");
    assert!(
        harn_entry_crossing::stdlib_module_is_cached(&vm, harn_entry_crossing::STDLIB_MODULE),
        "warm fixture: parent module cache must hold the target module"
    );
    AsyncBuiltinCtx::from_vm(vm)
}

/// Parent VM whose module cache misses the target module: every child
/// crossing replays module instantiation into its COW cache copy, which
/// is dropped on return.
fn cold_ctx() -> AsyncBuiltinCtx {
    let vm = vm_with_stdlib();
    assert!(
        !harn_entry_crossing::stdlib_module_is_cached(&vm, harn_entry_crossing::STDLIB_MODULE),
        "cold fixture: parent module cache must miss the target module"
    );
    AsyncBuiltinCtx::from_vm(vm)
}

fn by_name_args() -> Vec<VmValue> {
    vec![VmValue::string("v1.2.3")]
}

fn crossing_by_name(runtime: &Runtime, ctx: &AsyncBuiltinCtx, args: &[VmValue]) -> VmValue {
    runtime
        .block_on(harn_entry_crossing::call_export_by_name(ctx, args))
        .expect("by-name boundary crossing should succeed")
}

fn crossing_typed(runtime: &Runtime, ctx: &AsyncBuiltinCtx) -> ParsedVersion {
    runtime
        .block_on(harn_entry_crossing::call_export_typed(ctx, json!("v1.2.3")))
        .expect("typed boundary crossing should succeed")
}

/// One instrumented sample per configuration: verifies the crossing
/// returns the expected value and prints ns/call plus allocation counts
/// so allocation regressions on the seam are visible in bench logs.
fn measure_once(label: &str, runtime: &Runtime, ctx: &AsyncBuiltinCtx, typed: bool) {
    let args = by_name_args();
    if typed {
        let parsed = crossing_typed(runtime, ctx);
        assert_eq!(
            (parsed.major, parsed.minor, parsed.patch),
            (1, 2, 3),
            "typed crossing must round-trip std/semver::parse"
        );
    } else {
        let result = crossing_by_name(runtime, ctx, &args);
        assert!(
            result.as_dict().is_some(),
            "by-name crossing must return the parsed version dict"
        );
    }

    const CALLS: usize = 32;
    let started = Instant::now();
    for _ in 0..CALLS {
        if typed {
            black_box(crossing_typed(runtime, ctx));
        } else {
            black_box(crossing_by_name(runtime, ctx, &args));
        }
    }
    let elapsed_ns = started.elapsed().as_nanos() as f64;

    ALLOCATION_COUNT.store(0, Ordering::Relaxed);
    ALLOCATED_BYTES.store(0, Ordering::Relaxed);
    TRACK_ALLOCATIONS.store(true, Ordering::Relaxed);
    for _ in 0..CALLS {
        if typed {
            black_box(crossing_typed(runtime, ctx));
        } else {
            black_box(crossing_by_name(runtime, ctx, &args));
        }
    }
    TRACK_ALLOCATIONS.store(false, Ordering::Relaxed);

    let calls = CALLS as f64;
    eprintln!(
        "harn_entry_crossing/{label} sample: {:.1} ns/call, {:.1} allocations/call, {:.1} allocated bytes/call",
        elapsed_ns / calls,
        ALLOCATION_COUNT.load(Ordering::Relaxed) as f64 / calls,
        ALLOCATED_BYTES.load(Ordering::Relaxed) as f64 / calls,
    );
}

fn bench_harn_entry_crossing(c: &mut Criterion) {
    let runtime = runtime();
    let warm = warm_ctx(&runtime);
    let cold = cold_ctx();
    let args = by_name_args();

    measure_once("by_name_warm", &runtime, &warm, false);
    measure_once("by_name_cold", &runtime, &cold, false);
    measure_once("typed_warm", &runtime, &warm, true);
    measure_once("typed_cold", &runtime, &cold, true);

    let mut group = c.benchmark_group("harn_entry_crossing");
    group.bench_function("by_name_warm", |b| {
        b.iter(|| crossing_by_name(&runtime, &warm, black_box(&args)));
    });
    group.bench_function("by_name_cold", |b| {
        b.iter(|| crossing_by_name(&runtime, &cold, black_box(&args)));
    });
    group.bench_function("typed_warm", |b| {
        b.iter(|| crossing_typed(&runtime, &warm));
    });
    group.bench_function("typed_cold", |b| {
        b.iter(|| crossing_typed(&runtime, &cold));
    });
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(60);
    targets = bench_harn_entry_crossing
}
criterion_main!(benches);
