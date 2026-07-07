use std::hint::black_box;

use criterion::{criterion_group, criterion_main, Criterion};
use harn_vm::{compile_source, register_vm_stdlib, Chunk, Vm, VmValue};

fn find_text_hits_chunk(root: &str, pattern: &str) -> Chunk {
    let root = serde_json::to_string(root).expect("root string literal");
    let pattern = serde_json::to_string(pattern).expect("pattern string literal");
    compile_source(&format!(
        r#"
pipeline default(task) {{
  const hits = find_text({root}, {pattern}, {{
    include: ["**/*.harn"],
    max_matches: 100,
  }})
  return len(hits)
}}
"#
    ))
    .expect("bench source compiles")
}

fn find_text_exists_chunk(root: &str, pattern: &str) -> Chunk {
    let root = serde_json::to_string(root).expect("root string literal");
    let pattern = serde_json::to_string(pattern).expect("pattern string literal");
    compile_source(&format!(
        r#"
pipeline default(task) {{
  return find_text({root}, {pattern}, {{
    include: ["**/*.harn"],
    mode: "exists",
    preset: "source",
    parallel: true,
  }})
}}
"#
    ))
    .expect("bench source compiles")
}

fn execute(rt: &tokio::runtime::Runtime, chunk: &Chunk) -> VmValue {
    rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut vm = Vm::new();
                register_vm_stdlib(&mut vm);
                vm.execute(chunk).await.expect("bench source executes")
            })
            .await
    })
}

fn bench_find_text_repo_scale(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    for shard in 0..50 {
        let subdir = dir.path().join(format!("pkg-{shard}"));
        std::fs::create_dir_all(&subdir).unwrap();
        for file in 0..40 {
            let marker = if shard == 42 && file == 17 {
                "needle"
            } else {
                "ordinary"
            };
            std::fs::write(
                subdir.join(format!("file-{file}.harn")),
                format!("fn main() {{\n  const value = \"{marker}\"\n}}\n"),
            )
            .unwrap();
        }
    }
    let root = dir.path().to_string_lossy().into_owned();
    std::fs::create_dir_all(dir.path().join("node_modules/pkg")).unwrap();
    std::fs::write(
        dir.path().join("node_modules/pkg/ignored.harn"),
        "fn main() {\n  const value = \"needle\"\n}\n",
    )
    .unwrap();

    let hits_chunk = find_text_hits_chunk(&root, "needle");
    let exists_chunk = find_text_exists_chunk(&root, "needle");
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("tokio runtime");

    c.bench_function("fs_find_text_repo_scale_hits", |b| {
        b.iter(|| black_box(execute(&rt, &hits_chunk)));
    });
    c.bench_function("fs_find_text_repo_scale_exists_parallel", |b| {
        b.iter(|| black_box(execute(&rt, &exists_chunk)));
    });
}

criterion_group!(benches, bench_find_text_repo_scale);
criterion_main!(benches);
