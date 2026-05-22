use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion};
use harn_vm::bytecode_cache;
use harn_vm::{Chunk, Vm};
use harn_vm_perf::{
    allocation_stats_per_iteration, allocation_stats_per_iteration_batched, emit_allocation_jsonl,
};
use tokio::runtime::{Builder, Runtime};

const ALLOCATION_SAMPLES: u64 = 25;

struct VmHotPathFixture {
    name: &'static str,
    source: &'static str,
    path: PathBuf,
    chunk: Chunk,
}

impl VmHotPathFixture {
    fn new(name: &'static str, source: &'static str, runtime: &Runtime) -> Self {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("synthetic")
            .join(format!("{name}.harn"));
        let chunk = harn_vm::compile_source(source).expect("hot-path fixture should compile");
        let fixture = Self {
            name,
            source,
            path,
            chunk,
        };
        fixture.warm_inline_caches(runtime);
        fixture
    }

    fn setup_vm(&self) -> Vm {
        harn_vm::reset_thread_local_state();
        let mut vm = Vm::new();
        harn_vm::register_vm_stdlib(&mut vm);
        vm.set_source_info(&self.path.to_string_lossy(), self.source);
        if let Some(parent) = self.path.parent() {
            vm.set_source_dir(parent);
        }
        vm
    }

    fn execute(&self, runtime: &Runtime, mut vm: Vm) {
        runtime.block_on(async {
            let local = tokio::task::LocalSet::new();
            local
                .run_until(async {
                    let result = vm.execute(&self.chunk).await;
                    black_box(result.expect("hot-path fixture should execute"));
                })
                .await;
        });
    }

    fn warm_inline_caches(&self, runtime: &Runtime) {
        self.execute(runtime, self.setup_vm());
    }
}

struct BytecodeCacheFixture {
    source: String,
    source_path: PathBuf,
    chunk: Chunk,
    key: bytecode_cache::CacheKey,
}

impl BytecodeCacheFixture {
    fn new() -> Self {
        let source = r#"
pipeline default(task) {
  struct Point {
    x: int
    y: int
  }
  let point = Point {x: 1, y: 2}
  var i = 0
  var total = 0
  while i < 500 {
    total = total + point.x + point.y
    i = i + 1
  }
  if total < 0 {
    log("unreachable")
  }
}
"#
        .to_string();
        let root = std::env::temp_dir().join(format!(
            "harn-vm-bytecode-cache-bench-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("create bytecode cache bench dir");
        let source_path = root.join("entry.harn");
        std::fs::write(&source_path, &source).expect("write bytecode cache bench source");
        let chunk = harn_vm::compile_source(&source).expect("cache fixture should compile");
        let key = bytecode_cache::CacheKey::from_source(&source_path, &source);
        let artifact_path = bytecode_cache::adjacent_cache_path(&source_path)
            .expect("source path should have adjacent artifact path");
        bytecode_cache::store_at(&artifact_path, &key, &chunk)
            .expect("write adjacent bytecode cache artifact");
        Self {
            source,
            source_path,
            chunk,
            key,
        }
    }

    fn freeze(&self) {
        black_box(
            bytecode_cache::serialize_chunk_artifact(&self.key, &self.chunk)
                .expect("serialize bytecode cache artifact"),
        );
    }

    fn load_adjacent(&self) {
        let outcome = bytecode_cache::load(&self.source_path, &self.source);
        black_box(outcome.chunk.expect("adjacent bytecode cache should load"));
    }
}

fn runtime() -> Runtime {
    Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("benchmark runtime")
}

fn criterion_filters() -> Vec<String> {
    std::env::args()
        .skip(1)
        .filter(|arg| !arg.starts_with('-'))
        .collect()
}

fn matches_criterion_filter(filters: &[String], group: &str, benchmark: &str) -> bool {
    if filters.is_empty() {
        return true;
    }
    let full_name = format!("{group}/{benchmark}");
    filters.iter().any(|filter| full_name.contains(filter))
}

fn fixtures(runtime: &Runtime) -> Vec<VmHotPathFixture> {
    vec![
        VmHotPathFixture::new(
            "closure_call_setup",
            r#"
pipeline default(task) {
  fn make_step(offset) {
    return { value -> value + offset }
  }
  let step = make_step(3)
  var i = 0
  var total = 0
  while i < 1500 {
    total = step(total)
    i = i + 1
  }
  if total < 0 {
    log("unreachable")
  }
}
"#,
            runtime,
        ),
        VmHotPathFixture::new(
            "builtin_native_call_setup",
            r#"
pipeline default(task) {
  let text = "abcdefghij"
  var i = 0
  var total = 0
  while i < 2500 {
    total = total + len(text) + len([1, 2, 3, 4])
    i = i + 1
  }
  if total < 0 {
    log("unreachable")
  }
}
"#,
            runtime,
        ),
        VmHotPathFixture::new(
            "runtime_parameter_validation",
            r#"
pipeline default(task) {
  fn typed_step(value: int, label: string) -> int {
    return value + len(label)
  }
  var i = 0
  var total = 0
  while i < 1500 {
    total = typed_step(total, "abc")
    i = i + 1
  }
  if total < 0 {
    log("unreachable")
  }
}
"#,
            runtime,
        ),
        VmHotPathFixture::new(
            "property_inline_cache_hits",
            r#"
pipeline default(task) {
  struct Point {
    x: int
    y: int
  }
  let record = {hot: 7}
  let point = Point {x: 2, y: 3}
  let list = [1, 2, 3, 4]
  let text = ""
  var i = 0
  var total = 0
  while i < 2500 {
    total = total + record.hot + point.y + list.count
    if text.empty {
      total = total + 1
    }
    i = i + 1
  }
  if total < 0 {
    log("unreachable")
  }
}
"#,
            runtime,
        ),
        VmHotPathFixture::new(
            "method_inline_cache_hits",
            r#"
pipeline default(task) {
  let list = [1, 2, 3, 4]
  let text = "abcdef"
  let dict = {a: 1, b: 2}
  let values = set(1, 3, 5)
  var i = 0
  var total = 0
  while i < 2500 {
    total = total + list.count() + text.count() + dict.count() + values.count()
    if list.contains(3) { total = total + 1 }
    if text.contains("cd") { total = total + 1 }
    if dict.has("a") { total = total + 1 }
    if values.contains(5) { total = total + 1 }
    i = i + 1
  }
  if total < 0 {
    log("unreachable")
  }
}
"#,
            runtime,
        ),
        VmHotPathFixture::new(
            "list_callback_dispatch",
            r#"
pipeline default(task) {
  let input = range(0, 128).to_list()
  var i = 0
  var total = 0
  while i < 100 {
    let evens = input.filter({ value -> value % 2 == 0 })
    let doubled = evens.map({ value -> value * 2 })
    total = total + len(doubled)
    i = i + 1
  }
  if total < 0 {
    log("unreachable")
  }
}
"#,
            runtime,
        ),
        VmHotPathFixture::new(
            "dict_helper_builtins",
            r#"
import { filter_nil, pick_keys } from "std/collections"

pipeline default(task) {
  let raw = {
    repo: "burin-labs/harn",
    branch: "main",
    title: "perf",
    body: "bench fixture",
    draft: nil,
    labels: nil,
    timeout_ms: 30000,
  }
  let pickable = ["repo", "branch", "title", "body", "labels"]
  var i = 0
  var total = 0
  while i < 1000 {
    let cleaned = filter_nil(raw)
    let picked = pick_keys(cleaned, pickable)
    total = total + len(cleaned.keys()) + len(picked.keys())
    i = i + 1
  }
  if total < 0 {
    log("unreachable")
  }
}
"#,
            runtime,
        ),
    ]
}

fn timed_vm_runs(runtime: &Runtime, fixture: &VmHotPathFixture, iterations: u64) -> Duration {
    let mut total = Duration::ZERO;
    for _ in 0..iterations {
        let vm = fixture.setup_vm();
        let started = Instant::now();
        fixture.execute(runtime, vm);
        total += started.elapsed();
    }
    total
}

fn bench_vm_hot_paths(c: &mut Criterion) {
    let runtime = runtime();
    let filters = criterion_filters();
    let fixtures = fixtures(&runtime);
    let mut group = c.benchmark_group("vm_hot_paths");
    for fixture in &fixtures {
        if matches_criterion_filter(&filters, "vm_hot_paths", fixture.name) {
            let stats = allocation_stats_per_iteration_batched(
                ALLOCATION_SAMPLES,
                || fixture.setup_vm(),
                |vm| fixture.execute(&runtime, vm),
            );
            emit_allocation_jsonl("vm_hot_paths", fixture.name, ALLOCATION_SAMPLES, stats);
        }
        group.bench_with_input(
            BenchmarkId::from_parameter(fixture.name),
            fixture,
            |b, fixture| {
                b.iter_custom(|iterations| timed_vm_runs(&runtime, black_box(fixture), iterations));
            },
        );
    }
    group.finish();
}

fn bench_bytecode_cache(c: &mut Criterion) {
    let filters = criterion_filters();
    let fixture = BytecodeCacheFixture::new();
    let mut group = c.benchmark_group("vm_bytecode_cache");

    if matches_criterion_filter(&filters, "vm_bytecode_cache", "serialize_chunk_artifact") {
        let freeze_stats = allocation_stats_per_iteration(ALLOCATION_SAMPLES, || fixture.freeze());
        emit_allocation_jsonl(
            "vm_bytecode_cache",
            "serialize_chunk_artifact",
            ALLOCATION_SAMPLES,
            freeze_stats,
        );
    }
    group.bench_function("serialize_chunk_artifact", |b| {
        b.iter(|| fixture.freeze());
    });

    if matches_criterion_filter(&filters, "vm_bytecode_cache", "load_adjacent_chunk") {
        let load_stats =
            allocation_stats_per_iteration(ALLOCATION_SAMPLES, || fixture.load_adjacent());
        emit_allocation_jsonl(
            "vm_bytecode_cache",
            "load_adjacent_chunk",
            ALLOCATION_SAMPLES,
            load_stats,
        );
    }
    group.bench_function("load_adjacent_chunk", |b| {
        b.iter_batched(|| (), |()| fixture.load_adjacent(), BatchSize::SmallInput);
    });

    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(10);
    targets = bench_vm_hot_paths, bench_bytecode_cache
}
criterion_main!(benches);
