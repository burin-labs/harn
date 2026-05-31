use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use harn_vm::{compile_source, register_vm_stdlib, Chunk, Vm, VmValue};
use tokio::runtime::{Builder, Runtime};

const TASKS: usize = 16;
const INNER_ITERS: usize = 120_000;

fn pool_source(max_concurrent: usize) -> String {
    format!(
        r#"
import {{ pool_create, pool_wait }} from "std/lifecycle/pool"

fn crunch(seed) {{
  var i = 0
  var total = seed + 1
  while i < {INNER_ITERS} {{
    total = ((total * 1664525) + i + 1013904223) % 2147483647
    i = i + 1
  }}
  return total
}}

pipeline default() {{
  let pool = pool_create({{name: "pool-parallel-bench", max_concurrent: {max_concurrent}}})
  var handles = []
  for i in 0 to {TASKS} exclusive {{
    let seed = i
    handles = handles.push(pool.submit({{ -> crunch(seed) }}))
  }}
  let results = pool_wait(handles)
  return len(results)
}}
"#
    )
}

fn compile_pool_fixture(max_concurrent: usize) -> Chunk {
    compile_source(&pool_source(max_concurrent)).expect("pool parallel bench fixture compiles")
}

fn runtime() -> Runtime {
    Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("pool parallel bench runtime")
}

fn execute(rt: &Runtime, chunk: &Chunk) -> VmValue {
    harn_vm::reset_thread_local_state();
    rt.block_on(async {
        let mut vm = Vm::new();
        register_vm_stdlib(&mut vm);
        vm.execute(chunk).await.expect("pool fixture executes")
    })
}

fn bench_pool_parallel(c: &mut Criterion) {
    let mut group = c.benchmark_group("pool_parallel_cpu_fanout");
    for workers in [1_usize, 2, 4] {
        let chunk = compile_pool_fixture(workers);
        let rt = runtime();
        group.bench_with_input(BenchmarkId::from_parameter(workers), &workers, |b, _| {
            b.iter(|| black_box(execute(&rt, &chunk)));
        });
    }
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(10);
    targets = bench_pool_parallel
}
criterion_main!(benches);
