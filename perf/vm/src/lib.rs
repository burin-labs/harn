use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use serde_json::json;

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

#[derive(Clone, Copy, Debug)]
pub struct AllocationStats {
    pub calls: u64,
    pub bytes: u64,
}

impl AllocationStats {
    pub fn per_iteration(self, iterations: u64) -> AllocationStatsPerIteration {
        AllocationStatsPerIteration {
            calls: self.calls as f64 / iterations as f64,
            bytes: self.bytes as f64 / iterations as f64,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct AllocationStatsPerIteration {
    pub calls: f64,
    pub bytes: f64,
}

pub fn measure_allocations(mut f: impl FnMut()) -> AllocationStats {
    ALLOCATION_COUNT.store(0, Ordering::Relaxed);
    ALLOCATED_BYTES.store(0, Ordering::Relaxed);
    TRACK_ALLOCATIONS.store(true, Ordering::Relaxed);
    f();
    TRACK_ALLOCATIONS.store(false, Ordering::Relaxed);
    AllocationStats {
        calls: ALLOCATION_COUNT.load(Ordering::Relaxed),
        bytes: ALLOCATED_BYTES.load(Ordering::Relaxed),
    }
}

pub fn allocation_stats_per_iteration(
    iterations: u64,
    mut f: impl FnMut(),
) -> AllocationStatsPerIteration {
    measure_allocations(|| {
        for _ in 0..iterations {
            f();
        }
    })
    .per_iteration(iterations)
}

pub fn allocation_stats_per_iteration_batched<T>(
    iterations: u64,
    mut setup: impl FnMut() -> T,
    mut f: impl FnMut(T),
) -> AllocationStatsPerIteration {
    ALLOCATION_COUNT.store(0, Ordering::Relaxed);
    ALLOCATED_BYTES.store(0, Ordering::Relaxed);
    for _ in 0..iterations {
        let input = setup();
        TRACK_ALLOCATIONS.store(true, Ordering::Relaxed);
        f(input);
        TRACK_ALLOCATIONS.store(false, Ordering::Relaxed);
    }
    AllocationStats {
        calls: ALLOCATION_COUNT.load(Ordering::Relaxed),
        bytes: ALLOCATED_BYTES.load(Ordering::Relaxed),
    }
    .per_iteration(iterations)
}

pub fn emit_allocation_jsonl(
    suite: &str,
    benchmark: &str,
    iterations: u64,
    stats: AllocationStatsPerIteration,
) {
    eprintln!(
        "{}",
        json!({
            "suite": suite,
            "benchmark": benchmark,
            "iterations": iterations,
            "allocations_per_iteration": stats.calls,
            "allocated_bytes_per_iteration": stats.bytes,
        })
    );
}
