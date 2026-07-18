//! Measures the VM's async-opcode dispatch overhead with a deliberately tiny
//! async builtin. Real host operations usually dominate this boundary with
//! I/O; this fixture makes the dispatch future's fixed overhead visible.

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use harn_vm::{compile_source, Chunk, Vm, VmValue};

const ASYNC_CALLS: u64 = 20_000;

fn fixture() -> Chunk {
    compile_source(&format!(
        r"pipeline default() {{
  let i = 0
  while i < {ASYNC_CALLS} {{
    async_noop()
    i = i + 1
  }}
  return i
}}"
    ))
    .expect("compile async-dispatch fixture")
}

fn execute(runtime: &tokio::runtime::Runtime, chunk: &Chunk) -> VmValue {
    harn_vm::reset_thread_local_state();
    runtime.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut vm = Vm::new();
                vm.register_async_builtin("async_noop", |_ctx, _args| async { Ok(VmValue::Nil) });
                vm.execute(chunk).await.expect("async fixture executes")
            })
            .await
    })
}

fn bench_async_dispatch(c: &mut Criterion) {
    let chunk = fixture();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("async-dispatch runtime");
    let mut group = c.benchmark_group("vm_async_dispatch");
    group.throughput(Throughput::Elements(ASYNC_CALLS));
    group.bench_function("noop_builtin", |b| {
        b.iter(|| black_box(execute(&runtime, &chunk)));
    });
    group.finish();
}

criterion_group!(benches, bench_async_dispatch);
criterion_main!(benches);
