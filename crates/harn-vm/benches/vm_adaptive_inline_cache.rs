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

fn bench_stable_integer_add_and_closure_call(c: &mut Criterion) {
    let chunk = compile_source(
        r"pipeline t(task) {
fn erase(x) {
  return x
}
var i = erase(0)
var total = erase(0)
while i < erase(200) {
  total = total + i
  i = i + erase(1)
}
return total
}",
    )
    .expect("compile stable adaptive cache fixture");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");

    c.bench_function("vm_adaptive_cache_stable_int_add_and_call", |b| {
        b.iter(|| black_box(run_chunk(&rt, &chunk)));
    });
}

fn bench_mixed_numeric_add_deopt(c: &mut Criterion) {
    let chunk = compile_source(
        r"pipeline t(task) {
fn erase(x) {
  return x
}
let values = [erase(1), erase(2), erase(3), erase(4.0), erase(5.0)]
var total = erase(0)
var i = 0
while i < 40 {
  for value in values {
    total = total + value
  }
  i = i + 1
}
return total
}",
    )
    .expect("compile mixed adaptive cache fixture");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");

    c.bench_function("vm_adaptive_cache_mixed_numeric_deopt", |b| {
        b.iter(|| black_box(run_chunk(&rt, &chunk)));
    });
}

criterion_group!(
    benches,
    bench_stable_integer_add_and_closure_call,
    bench_mixed_numeric_add_deopt
);
criterion_main!(benches);
