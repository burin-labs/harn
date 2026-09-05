#![recursion_limit = "256"]
//! Deterministic allocation-regression guard for the user-function call path.
//!
//! Entering a closure frame is the dominant cost of an orchestration-heavy
//! workload, and that cost is overwhelmingly heap allocation rather than CPU.
//! Unlike a timing benchmark, the *number* of allocations per call is
//! machine-independent and stable in CI, so we pin it here.
//!
//! A user-function call currently performs exactly **2** heap allocations:
//!   1. the callee's `Vec<LocalSlot>` (fresh mutable locals — fundamental),
//!   2. the callee's env scope-stack clone (`VmEnv::cloned_for_call`, which
//!      also reserves the slot the call's pushed scope reuses).
//!
//! The frame's `fn_name` used to be a third (a `String` clone per call); it
//! is now a shared `HarnStr` refcount bump off `CompiledFunction::name`.
//!
//! The caller-env snapshot is a move (`std::mem::replace`), not a clone, and the
//! cloned callee env reserves room for its pushed scope, so neither shows up
//! here. If this count regresses (e.g. a reintroduced env clone or a
//! clone-then-grow reallocation), this test fails. Improvements are allowed:
//! the assertion is an upper bound.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

thread_local! {
    /// Allocations made by *this* thread.
    ///
    /// Per thread, not per process. A global allocator sees every thread, and
    /// this test's process has more than one: the tokio driver `enable_all`
    /// starts, and any process-owned runtime the VM spins up, all allocate
    /// while the measurement is open. Counting them made the number mean
    /// "allocations anywhere during this wall-clock window" rather than
    /// "allocations the call path performed", and on Windows the 4000-iteration
    /// window came back smaller than the 2000-iteration one, so the marginal
    /// subtraction underflowed and the test aborted instead of reporting a
    /// regression (harn#8020, Windows job on head 84b51b3c).
    ///
    /// `const` initialization is load-bearing: a lazily initialized
    /// thread-local allocates on first touch, and allocating inside the
    /// allocator is unbounded recursion.
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
}

struct Counting;

/// Process-wide shadow count retained only as the negative control proving why
/// the attributed measurement must not use it.
static PROCESS_ALLOCS: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        PROCESS_ALLOCS.fetch_add(1, Ordering::Relaxed);
        // `try_with` rather than `with`: a thread allocating while its
        // thread-locals are being destroyed must not panic inside the
        // allocator. Losing a count during teardown cannot affect a
        // measurement, which is always bracketed inside one test body.
        let _ = ALLOCS.try_with(|allocs| allocs.set(allocs.get().wrapping_add(1)));
        System.alloc(layout)
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout);
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Allocations this thread has made so far.
fn allocs_on_this_thread() -> usize {
    ALLOCS.with(Cell::get)
}

/// Compile and run `source`, returning the number of heap allocations performed
/// during `vm.execute` only (compilation and VM setup are excluded).
fn allocs_during_execute(source: &str) -> usize {
    allocs_during_execute_after(source, || {})
}

/// Like [`allocs_during_execute`], with a hook that runs inside the measured
/// window after the opening allocation count is captured.
fn allocs_during_execute_after(source: &str, after_opening_count: impl FnOnce()) -> usize {
    harn_vm::reset_thread_local_state();
    let chunk = harn_vm::compile_source(source).expect("compile");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut vm = harn_vm::Vm::new();
                harn_vm::register_vm_stdlib(&mut vm);
                let before = allocs_on_this_thread();
                after_opening_count();
                vm.execute(&chunk).await.expect("execute");
                allocs_on_this_thread() - before
            })
            .await
    })
}

/// Calls a user `fn` `n` times in an allocation-free loop (lazy `to` range,
/// slot-indexed locals).
fn user_fn_loop(n: usize) -> String {
    format!(
        "pipeline t(task: unknown) {{\n\
         // Deliberate `any`: this benchmark isolates call-frame allocation
         // from the separately-covered runtime parameter contract path.
         fn f(x: any) {{ return x + 1 }}\n\
         let s = 0\n\
         for i in 0 to {n} {{ s = s + f(i) }}\n\
         return s\n\
         }}"
    )
}

/// The same loop without the call, to isolate per-call allocations from any
/// per-iteration loop overhead.
fn bare_loop(n: usize) -> String {
    format!(
        "pipeline t(task: unknown) {{\n\
         let s = 0\n\
         for i in 0 to {n} {{ s = s + (i + 1) }}\n\
         return s\n\
         }}"
    )
}

/// Marginal allocations attributable to one extra iteration of `make(n)`,
/// computed as the difference between `n2` and `n1` iterations so fixed setup
/// cancels out.
fn marginal_per_iter(make: impl Fn(usize) -> String) -> f64 {
    let (n1, n2) = (2000usize, 4000usize);
    let a1 = allocs_during_execute(&make(n1));
    let a2 = allocs_during_execute(&make(n2));
    // Report rather than underflow. More iterations doing fewer allocations is
    // a broken measurement, and a subtraction panic names the arithmetic
    // instead of the fact.
    let marginal = a2.checked_sub(a1).unwrap_or_else(|| {
        panic!(
            "{n2} iterations allocated {a2}, fewer than {a1} for {n1}; \
             the measurement is not attributable to the work under test"
        )
    });
    marginal as f64 / (n2 - n1) as f64
}

/// Execute while a sibling thread makes `allocation_count` observable heap
/// allocations inside the same measured window.
fn execute_with_sibling_allocations(
    source: &str,
    allocation_count: usize,
) -> (usize, usize, usize) {
    use std::sync::{Arc, Barrier};

    let start = Arc::new(Barrier::new(2));
    let done = Arc::new(Barrier::new(2));
    let sibling_allocations = Arc::new(AtomicUsize::new(0));
    let sibling_start = Arc::clone(&start);
    let sibling_done = Arc::clone(&done);
    let sibling_count = Arc::clone(&sibling_allocations);
    let sibling = std::thread::spawn(move || {
        sibling_start.wait();
        let before = allocs_on_this_thread();
        let mut sink: Vec<Box<[u8; 64]>> = Vec::with_capacity(allocation_count);
        for _ in 0..allocation_count {
            sink.push(Box::new([0u8; 64]));
        }
        std::hint::black_box(&sink);
        sibling_count.store(allocs_on_this_thread() - before, Ordering::Relaxed);
        sibling_done.wait();
    });

    let process_before = PROCESS_ALLOCS.load(Ordering::Relaxed);
    let measured = allocs_during_execute_after(source, || {
        start.wait();
        done.wait();
    });
    let process_measured = PROCESS_ALLOCS.load(Ordering::Relaxed) - process_before;
    sibling.join().expect("sibling allocation thread");
    let sibling_count = sibling_allocations.load(Ordering::Relaxed);
    (measured, sibling_count, process_measured)
}

#[test]
fn user_fn_call_allocates_at_most_twice() {
    let per_call = marginal_per_iter(user_fn_loop);
    let per_loop = marginal_per_iter(bare_loop);
    let attributable_to_call = per_call - per_loop;

    // The loop itself must stay allocation-free, otherwise the call delta is
    // meaningless.
    assert!(
        per_loop < 0.01,
        "the measurement loop regressed to {per_loop} allocs/iter; it must be allocation-free"
    );

    // Upper bound: a user-fn call must not exceed 2 heap allocations. Floats
    // because the value is a measured ratio; 2.0001 tolerates nothing extra
    // while staying robust to f64 division.
    assert!(
        attributable_to_call <= 2.0001,
        "user-fn call regressed to {attributable_to_call} allocs/call (was 2); \
         a redundant env clone, a reintroduced per-call name clone, or a \
         clone-then-grow reallocation is the usual cause"
    );
}

/// The property the process-wide counter could not offer: this measurement
/// belongs to the thread doing the work.
///
/// Without it the guard reads whatever else the process allocated during the
/// same window, which is how it came to report a negative marginal cost and
/// abort. The sibling thread reports its own allocation count, so a run where
/// the noise never happened fails here rather than passing as a quiet
/// agreement.
#[test]
fn a_concurrent_allocator_does_not_move_the_measurement() {
    let source = user_fn_loop(2000);
    // First execution pays one-time initialization; measure after it.
    let _warmup = allocs_during_execute(&source);
    let (quiet, quiet_sibling_allocations, quiet_process_allocations) =
        execute_with_sibling_allocations(&source, 0);
    let (noisy, noise_allocations, noisy_process_allocations) =
        execute_with_sibling_allocations(&source, 20_000);

    assert_eq!(
        quiet_sibling_allocations, 0,
        "the quiet control unexpectedly allocated on its sibling thread"
    );

    assert!(
        noise_allocations > 10_000,
        "the sibling only made {noise_allocations} allocations, so this case \
         would agree even with a process-wide counter"
    );
    assert!(
        noisy_process_allocations.saturating_sub(quiet_process_allocations) > 10_000,
        "negative control did not fire: a process-wide count moved only from \
         {quiet_process_allocations} to {noisy_process_allocations}"
    );
    assert_eq!(
        quiet, noisy,
        "a sibling thread's {noise_allocations} allocations moved the measured \
         count from {quiet} to {noisy}"
    );
}
