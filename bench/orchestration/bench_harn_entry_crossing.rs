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
//!   cache misses). A cold crossing used to replay module instantiation
//!   into the child VM's copy and drop it on return; an immutable stdlib
//!   module is now instantiated once per execution tree and reused from
//!   the tree's shared instance store (`vm/modules/shared_instances.rs`).
//!
//! The callee is `std/semver::parse("v1.2.3")` — pure and trivial — so
//! the numbers isolate crossing overhead, not callee work. `tool_parse_cold`
//! drives the production text-tool parse seam every tool-bearing LLM
//! response takes: a typed crossing into `std/llm/tool_parse`, an
//! 11-module import closure that no parent VM ever holds warm.

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

const TOOL_PARSE_TEXT: &str = "<assistant_prose>Reading the file now.</assistant_prose>";

fn crossing_tool_parse(runtime: &Runtime, ctx: &AsyncBuiltinCtx) -> String {
    runtime
        .block_on(harn_entry_crossing::parse_text_tool_calls(
            ctx,
            TOOL_PARSE_TEXT,
        ))
        .expect("text-tool parse crossing should succeed")
}

fn crossing_typed(runtime: &Runtime, ctx: &AsyncBuiltinCtx) -> ParsedVersion {
    runtime
        .block_on(harn_entry_crossing::call_export_typed(ctx, json!("v1.2.3")))
        .expect("typed boundary crossing should succeed")
}

#[derive(Clone, Copy)]
enum Crossing {
    ByName,
    Typed,
    ToolParse,
}

fn cross(runtime: &Runtime, ctx: &AsyncBuiltinCtx, args: &[VmValue], crossing: Crossing) {
    match crossing {
        Crossing::ByName => {
            black_box(crossing_by_name(runtime, ctx, args));
        }
        Crossing::Typed => {
            black_box(crossing_typed(runtime, ctx));
        }
        Crossing::ToolParse => {
            black_box(crossing_tool_parse(runtime, ctx));
        }
    }
}

/// One instrumented sample per configuration: verifies the crossing
/// returns the expected value and prints ns/call plus allocation counts
/// so allocation regressions on the seam are visible in bench logs.
fn measure_once(label: &str, runtime: &Runtime, ctx: &AsyncBuiltinCtx, crossing: Crossing) {
    let args = by_name_args();
    match crossing {
        Crossing::Typed => {
            let parsed = crossing_typed(runtime, ctx);
            assert_eq!(
                (parsed.major, parsed.minor, parsed.patch),
                (1, 2, 3),
                "typed crossing must round-trip std/semver::parse"
            );
        }
        Crossing::ByName => {
            let result = crossing_by_name(runtime, ctx, &args);
            assert!(
                result.as_dict().is_some(),
                "by-name crossing must return the parsed version dict"
            );
        }
        Crossing::ToolParse => {
            let prose = crossing_tool_parse(runtime, ctx);
            assert!(
                prose.contains("Reading the file now."),
                "tool-parse crossing must return the response prose, got {prose:?}"
            );
        }
    }

    const CALLS: usize = 32;
    let started = Instant::now();
    for _ in 0..CALLS {
        cross(runtime, ctx, &args, crossing);
    }
    let elapsed_ns = started.elapsed().as_nanos() as f64;

    ALLOCATION_COUNT.store(0, Ordering::Relaxed);
    ALLOCATED_BYTES.store(0, Ordering::Relaxed);
    TRACK_ALLOCATIONS.store(true, Ordering::Relaxed);
    for _ in 0..CALLS {
        cross(runtime, ctx, &args, crossing);
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
    let tool_parse = cold_ctx();
    let args = by_name_args();

    measure_once("by_name_warm", &runtime, &warm, Crossing::ByName);
    measure_once("by_name_cold", &runtime, &cold, Crossing::ByName);
    measure_once("typed_warm", &runtime, &warm, Crossing::Typed);
    measure_once("typed_cold", &runtime, &cold, Crossing::Typed);
    measure_once(
        "tool_parse_cold",
        &runtime,
        &tool_parse,
        Crossing::ToolParse,
    );

    let mut group = c.benchmark_group("harn_entry_crossing");
    for (name, ctx, crossing) in [
        ("by_name_warm", &warm, Crossing::ByName),
        ("by_name_cold", &cold, Crossing::ByName),
        ("typed_warm", &warm, Crossing::Typed),
        ("typed_cold", &cold, Crossing::Typed),
        ("tool_parse_cold", &tool_parse, Crossing::ToolParse),
    ] {
        group.bench_function(name, |b| {
            b.iter(|| cross(&runtime, ctx, black_box(&args), crossing));
        });
    }
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(60);
    targets = bench_harn_entry_crossing
}
criterion_main!(benches);
