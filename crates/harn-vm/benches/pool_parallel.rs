use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use harn_vm::{compile_source, register_vm_stdlib, Chunk, Vm, VmValue};
use tokio::runtime::{Builder, Runtime};

const TASKS: usize = 16;
const INNER_ITERS: usize = 90_000;

fn crunch_function() -> String {
    format!(
        r#"
fn crunch(seed) {{
  var i = 0
  var total = seed + 1
  while i < {INNER_ITERS} {{
    total = ((total * 1664525) + i + 1013904223) % 2147483647
    i = i + 1
  }}
  return {{
    seed: seed,
    total: total,
    payload: [seed, total, "pool-parallel"]
  }}
}}
"#
    )
}

fn serial_source() -> String {
    format!(
        r"
{}

pipeline default() {{
  var results = []
  for i in 0 to {TASKS} exclusive {{
    results = results.push(crunch(i))
  }}
  return len(results)
}}
",
        crunch_function()
    )
}

fn pool_source(max_concurrent: usize) -> String {
    format!(
        r#"
import {{ pool_create, pool_wait }} from "std/lifecycle/pool"

{}

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
"#,
        crunch_function()
    )
}

fn compile_pool_fixture(max_concurrent: usize) -> Chunk {
    compile_source(&pool_source(max_concurrent)).expect("pool parallel bench fixture compiles")
}

fn compile_serial_fixture() -> Chunk {
    compile_source(&serial_source()).expect("serial pool baseline bench fixture compiles")
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
    group.throughput(Throughput::Elements(TASKS as u64));
    let serial_chunk = compile_serial_fixture();
    let serial_rt = runtime();
    group.bench_function(BenchmarkId::new("serial", 1), |b| {
        b.iter(|| black_box(execute(&serial_rt, &serial_chunk)));
    });
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
