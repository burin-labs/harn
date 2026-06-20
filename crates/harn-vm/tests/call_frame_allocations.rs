#![recursion_limit = "256"]
//! Deterministic allocation-regression guard for the user-function call path.
//!
//! Entering a closure frame is the dominant cost of an orchestration-heavy
//! workload, and that cost is overwhelmingly heap allocation rather than CPU.
//! Unlike a timing benchmark, the *number* of allocations per call is
//! machine-independent and stable in CI, so we pin it here.
//!
//! A user-function call currently performs exactly **3** heap allocations:
//!   1. the callee's `Vec<LocalSlot>` (fresh mutable locals — fundamental),
//!   2. the callee's env scope-stack clone (`VmEnv::cloned_for_call`, which
//!      also reserves the slot the call's pushed scope reuses), and
//!   3. the frame's `fn_name` string clone.
//!
//! The caller-env snapshot is a move (`std::mem::replace`), not a clone, and the
//! cloned callee env reserves room for its pushed scope, so neither shows up
//! here. If this count regresses (e.g. a reintroduced env clone or a
//! clone-then-grow reallocation), this test fails. Improvements are allowed:
//! the assertion is an upper bound.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Counting;
static ALLOCS: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        System.alloc(layout)
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout);
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Compile and run `source`, returning the number of heap allocations performed
/// during `vm.execute` only (compilation and VM setup are excluded).
fn allocs_during_execute(source: &str) -> usize {
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
                let before = ALLOCS.load(Ordering::Relaxed);
                vm.execute(&chunk).await.expect("execute");
                ALLOCS.load(Ordering::Relaxed) - before
            })
            .await
    })
}

/// Calls a user `fn` `n` times in an allocation-free loop (lazy `to` range,
/// slot-indexed locals).
fn user_fn_loop(n: usize) -> String {
    format!(
        "pipeline t(task) {{\n\
         fn f(x) {{ return x + 1 }}\n\
         var s = 0\n\
         for i in 0 to {n} {{ s = s + f(i) }}\n\
         return s\n\
         }}"
    )
}

/// The same loop without the call, to isolate per-call allocations from any
/// per-iteration loop overhead.
fn bare_loop(n: usize) -> String {
    format!(
        "pipeline t(task) {{\n\
         var s = 0\n\
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
    (a2 - a1) as f64 / (n2 - n1) as f64
}

#[test]
fn user_fn_call_allocates_at_most_three_times() {
    let per_call = marginal_per_iter(user_fn_loop);
    let per_loop = marginal_per_iter(bare_loop);
    let attributable_to_call = per_call - per_loop;

    // The loop itself must stay allocation-free, otherwise the call delta is
    // meaningless.
    assert!(
        per_loop < 0.01,
        "the measurement loop regressed to {per_loop} allocs/iter; it must be allocation-free"
    );

    // Upper bound: a user-fn call must not exceed 3 heap allocations. Floats
    // because the value is a measured ratio; 3.0001 tolerates nothing extra
    // while staying robust to f64 division.
    assert!(
        attributable_to_call <= 3.0001,
        "user-fn call regressed to {attributable_to_call} allocs/call (was 3); \
         a redundant env clone or clone-then-grow reallocation was likely reintroduced"
    );
}
