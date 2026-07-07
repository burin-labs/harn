//! Benchmarks isolating variable/builtin *name resolution* on the VM hot
//! path: function-local slot reads (the fast O(1) path) versus module-level
//! ("global") reads and builtin references/calls, which fall through
//! `execute_get_var` / `resolve_named_closure` (local-slot scan -> env walk
//! -> `module_state` mutex -> globals -> builtin registry).
//!
//! Each fixture reads its target name ten times per iteration over a long
//! loop so the per-read resolution cost dominates the one-time VM
//! construction. The `*_many_locals` variant widens the enclosing function's
//! local-slot table so the linear, string-comparing scan that every
//! non-local access pays in `active_local_slot_index` is visible.

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, Criterion};
use harn_vm::{compile_source, register_vm_stdlib, Chunk, Vm};

fn run_chunk(rt: &tokio::runtime::Runtime, chunk: &Chunk) -> String {
    rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut vm = Vm::new();
                register_vm_stdlib(&mut vm);
                let result = vm.execute(chunk).await.expect("bench script runs");
                black_box(result);
                vm.output().to_string()
            })
            .await
    })
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
}

/// `work` reads a *function-local* `a` ten times per iteration: the compiler
/// resolves `a` to a `GetLocalSlot`, the O(1) fast path.
const LOCAL_READ: &str = r"pipeline t(task) {
  fn work() {
    const a = 1
    let total = 0
    let i = 0
    while i < 50000 {
      total = total + a + a + a + a + a + a + a + a + a + a
      i = i + 1
    }
    return total
  }
  return work()
}";

/// `work` reads a *module-level* `a` ten times per iteration: the compiler
/// emits `GetVar`, so each read runs the dead local-slot scan + env walk +
/// `module_state` mutex before hitting the binding.
const GLOBAL_READ: &str = r"pipeline t(task) {
  const a = 1
  fn work() {
    let total = 0
    let i = 0
    while i < 50000 {
      total = total + a + a + a + a + a + a + a + a + a + a
      i = i + 1
    }
    return total
  }
  return work()
}";

/// Same global read, but `work` carries many local slots so the linear
/// `active_local_slot_index` scan each `GetVar` pays is wide.
const GLOBAL_READ_MANY_LOCALS: &str = r"pipeline t(task) {
  const a = 1
  fn work() {
    const l0 = 0
    const l1 = 1
    const l2 = 2
    const l3 = 3
    const l4 = 4
    const l5 = 5
    const l6 = 6
    const l7 = 7
    const l8 = 8
    const l9 = 9
    const l10 = 10
    const l11 = 11
    const l12 = 12
    const l13 = 13
    const l14 = 14
    const l15 = 15
    const l16 = 16
    const l17 = 17
    const l18 = 18
    const l19 = 19
    let total = l0 + l19
    let i = 0
    while i < 50000 {
      total = total + a + a + a + a + a + a + a + a + a + a
      i = i + 1
    }
    return total
  }
  return work()
}";

fn bench(c: &mut Criterion, name: &str, src: &str) {
    let chunk = compile_source(src).expect("compile name-resolution fixture");
    let rt = rt();
    c.bench_function(name, |b| b.iter(|| black_box(run_chunk(&rt, &chunk))));
}

/// Hot loop calling a builtin (`abs`) by bare name ten times per iteration.
/// The sync call path runs `resolve_named_closure` — local-slot scan, env
/// walk, and the `module_functions` + `module_state` mutexes (both missing) —
/// before the builtin finally dispatches by id.
const BUILTIN_CALL: &str = r"pipeline t(task) {
  fn work() {
    let total = 0
    let i = 0
    while i < 50000 {
      total = abs(i) + abs(i) + abs(i) + abs(i) + abs(i) + abs(i) + abs(i) + abs(i) + abs(i) + abs(i)
      i = i + 1
    }
    return total
  }
  return work()
}";

/// Hot loop calling a sibling module-level function ten times per iteration:
/// `resolve_named_closure` re-resolves through the module-function/module-state
/// mutexes every call even though the DirectCall cache is "warm".
const USER_FN_CALL: &str = r"pipeline t(task) {
  fn dbl(x) { return x + x }
  fn work() {
    let total = 0
    let i = 0
    while i < 50000 {
      total = dbl(i) + dbl(i) + dbl(i) + dbl(i) + dbl(i) + dbl(i) + dbl(i) + dbl(i) + dbl(i) + dbl(i)
      i = i + 1
    }
    return total
  }
  return work()
}";

fn benches_group(c: &mut Criterion) {
    bench(c, "name_res_local_read", LOCAL_READ);
    bench(c, "name_res_global_read", GLOBAL_READ);
    bench(
        c,
        "name_res_global_read_many_locals",
        GLOBAL_READ_MANY_LOCALS,
    );
    bench(c, "name_res_builtin_call", BUILTIN_CALL);
    bench(c, "name_res_user_fn_call", USER_FN_CALL);
}

criterion_group!(benches, benches_group);
criterion_main!(benches);
