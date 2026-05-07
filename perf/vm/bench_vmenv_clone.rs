use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use harn_vm::bench_internals::{NonModuleClosureCallFixture, VMENV_CAPTURE_COUNTS};

struct CountingAllocator;

static COUNT_ALLOCATIONS: AtomicBool = AtomicBool::new(false);
static ALLOCATION_CALLS: AtomicU64 = AtomicU64::new(0);

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNT_ALLOCATIONS.load(Ordering::Relaxed) {
            ALLOCATION_CALLS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if COUNT_ALLOCATIONS.load(Ordering::Relaxed) {
            ALLOCATION_CALLS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if COUNT_ALLOCATIONS.load(Ordering::Relaxed) {
            ALLOCATION_CALLS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

fn allocations_per_call(fixture: &NonModuleClosureCallFixture, iterations: u64) -> f64 {
    ALLOCATION_CALLS.store(0, Ordering::Relaxed);
    COUNT_ALLOCATIONS.store(true, Ordering::Relaxed);
    for _ in 0..iterations {
        black_box(fixture.invoke());
    }
    COUNT_ALLOCATIONS.store(false, Ordering::Relaxed);
    ALLOCATION_CALLS.load(Ordering::Relaxed) as f64 / iterations as f64
}

fn bench_vmenv_clone(c: &mut Criterion) {
    let fixtures = VMENV_CAPTURE_COUNTS
        .into_iter()
        .map(NonModuleClosureCallFixture::new)
        .collect::<Vec<_>>();

    let mut group = c.benchmark_group("vmenv_non_module_closure_call");
    for fixture in &fixtures {
        let allocs_per_call = allocations_per_call(fixture, 10_000);
        eprintln!(
            "vmenv_non_module_closure_call/captures_{:03}: {:.2} allocations/call",
            fixture.capture_count(),
            allocs_per_call
        );

        group.bench_with_input(
            BenchmarkId::from_parameter(format!("captures_{:03}", fixture.capture_count())),
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
