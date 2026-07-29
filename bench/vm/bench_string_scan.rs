//! Source-scanning benchmarks (harn#2790).
//!
//! Repo-hygiene and codegen scripts ported to Harn need to scan multi-kilobyte
//! source files with a cursor-style loop. Random char access into a `string`
//! (`substring`, `s[i]`, `s[a:b]`, `s.count`) is O(n) per call because the
//! backing storage is UTF-8, so a naive per-character cursor loop is O(n^2) and
//! stalls on real files. The supported idiom is to materialize the string into
//! a list of single-character values once (`chars(...)`), then scan the list
//! with O(1) indexing.
//!
//! These fixtures pin both shapes at two input sizes so the linear `chars`
//! path stays linear and the quadratic `substring` cursor regression is
//! visible if anyone makes the materialization path allocate per character
//! again.

use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use harn_vm::{Chunk, Vm};
use tokio::runtime::{Builder, Runtime};

/// One synthetic "source line" of mostly-ASCII text with brace/quote tokens a
/// real scanner would track. Repeated to build the scanned corpus.
const SOURCE_LINE: &str = "    let handler = { \"op\": \"host.read\", body: foo(bar) } // note\n";

struct ScanFixture {
    name: String,
    source: String,
    path: PathBuf,
    chunk: Chunk,
}

impl ScanFixture {
    fn new(name: impl Into<String>, source: String, runtime: &Runtime) -> Self {
        let name = name.into();
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("synthetic")
            .join(format!("{name}.harn"));
        let chunk = harn_vm::compile_source(&source).expect("scan fixture should compile");
        let fixture = Self {
            name,
            source,
            path,
            chunk,
        };
        fixture.execute(runtime, fixture.setup_vm());
        fixture
    }

    fn setup_vm(&self) -> Vm {
        harn_vm::reset_thread_local_state();
        let mut vm = Vm::new();
        harn_vm::register_vm_stdlib(&mut vm);
        vm.set_source_info(&self.path.to_string_lossy(), &self.source);
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
                    black_box(result.expect("scan fixture should execute"));
                })
                .await;
        });
    }
}

/// Builds a corpus of roughly `lines` repeated source lines as a Harn string
/// literal, embedded in a script that scans it `scan_kind` ways.
fn scan_source(scan_kind: &str, lines: usize) -> String {
    let corpus = SOURCE_LINE.repeat(lines);
    // Escape the literal for embedding: backslash, quote, and newline.
    let escaped = corpus
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n");
    let scan_body = match scan_kind {
        // Linear path: materialize once, then index the list.
        "chars_list" => {
            r#"
  let cs = chars(src)
  let n = cs.count
  var i = 0
  var braces = 0
  while i < n {
    if cs[i] == "{" { braces = braces + 1 }
    i = i + 1
  }
"#
        }
        // Quadratic path: re-scan the string on every cursor step.
        "substring_cursor" => {
            r#"
  let n = src.count
  var i = 0
  var braces = 0
  while i < n {
    if substring(src, i, i + 1) == "{" { braces = braces + 1 }
    i = i + 1
  }
"#
        }
        other => panic!("unknown scan kind {other}"),
    };
    format!("pipeline default(task) {{\n  let src = \"{escaped}\"\n{scan_body}\n  if braces < 0 {{ log(\"unreachable\") }}\n}}\n")
}

fn runtime() -> Runtime {
    Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("benchmark runtime")
}

fn fixtures(runtime: &Runtime) -> Vec<ScanFixture> {
    let mut fixtures = Vec::new();
    for lines in [400usize, 800] {
        for kind in ["chars_list", "substring_cursor"] {
            fixtures.push(ScanFixture::new(
                format!("{kind}_{lines}"),
                scan_source(kind, lines),
                runtime,
            ));
        }
    }
    fixtures
}

fn timed_runs(runtime: &Runtime, fixture: &ScanFixture, iterations: u64) -> Duration {
    let mut total = Duration::ZERO;
    for _ in 0..iterations {
        let vm = fixture.setup_vm();
        let started = Instant::now();
        fixture.execute(runtime, vm);
        total += started.elapsed();
    }
    total
}

fn bench_string_scan(c: &mut Criterion) {
    let runtime = runtime();
    let fixtures = fixtures(&runtime);
    let mut group = c.benchmark_group("string_scan");
    for fixture in &fixtures {
        group.bench_with_input(
            BenchmarkId::from_parameter(&fixture.name),
            fixture,
            |b, fixture| {
                b.iter_custom(|iterations| timed_runs(&runtime, black_box(fixture), iterations));
            },
        );
    }
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(10);
    targets = bench_string_scan
}
criterion_main!(benches);
