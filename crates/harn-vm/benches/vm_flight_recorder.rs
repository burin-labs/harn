use std::hint::black_box;

use criterion::{criterion_group, criterion_main, Criterion};
use harn_vm::{compile_source, register_vm_stdlib, Chunk, Vm};

const HOT_LOOP: &str = r"pipeline main(harness: Harness) {
  let total = 0
  let i = 0
  while i < 10000 {
    total = total + i
    i = i + 1
  }
  return total
}";

fn execute(rt: &tokio::runtime::Runtime, chunk: &Chunk, record: bool) {
    rt.block_on(async {
        tokio::task::LocalSet::new()
            .run_until(async {
                let mut vm = Vm::new();
                register_vm_stdlib(&mut vm);
                if record {
                    vm.enable_flight_recorder(harn_vm::flight_recorder::DEFAULT_MAX_EVENTS);
                }
                black_box(vm.execute(chunk).await.expect("benchmark executes"));
                if record {
                    black_box(vm.flight_recording().expect("recording").events.len());
                }
            })
            .await;
    });
}

fn bench_flight_recorder(c: &mut Criterion) {
    let chunk = compile_source(HOT_LOOP).expect("compile benchmark");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let mut group = c.benchmark_group("vm_flight_recorder");
    group.sample_size(10);
    group.bench_function("off", |b| b.iter(|| execute(&rt, &chunk, false)));
    group.bench_function("on", |b| b.iter(|| execute(&rt, &chunk, true)));
    group.finish();
}

criterion_group!(benches, bench_flight_recorder);
criterion_main!(benches);
