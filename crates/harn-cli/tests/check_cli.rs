mod test_util;

use std::fs;
use std::path::Path;

use tempfile::TempDir;
use test_util::process::harn_command;

#[test]
fn check_reports_unknown_struct_type_in_stderr() {
    let temp = TempDir::new().unwrap();
    let script = temp.path().join("main.harn");
    fs::write(&script, "let p = Point { x: 3, y: 4 }\n").unwrap();

    let output = harn_command()
        .current_dir(temp.path())
        .args(["check", script.to_str().unwrap()])
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "expected nonzero exit, stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unknown struct type `Point`"),
        "missing unknown-struct diagnostic: {stderr}"
    );
    assert!(
        stderr.contains(&format!("{}:1:9", script.display())),
        "missing precise location in stderr: {stderr}"
    );
}

/// CLI-level regression for the Linux hang on `harn lint <dir>` and
/// `harn check --workspace` against pipeline trees with cyclic
/// cross-sibling-directory relative imports (#748). The underlying fix
/// (`harn_modules::build()` canonicalizing before seen-set dedupe,
/// #93) already has a unit test in `harn-modules`. This test guards
/// the same regression at the CLI surface so a future walker change in
/// `lint` / `check --workspace` that re-introduces the explosion is
/// caught here rather than in downstream pipeline trees like
/// burin-code's. Four sibling directories × six files importing every
/// other directory by relative path — the exact pattern that produced
/// fresh path spellings on every round-trip and OOM-killed Linux CI
/// runners around 48 s pre-fix.
#[test]
fn lint_and_check_complete_on_large_cross_directory_cycle_workspace() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    let pipelines = root.join("Sources/pipelines");
    write_cross_directory_cycle_workspace(&pipelines);

    fs::write(
        root.join("harn.toml"),
        "[package]\n\
         name = \"hang-regression\"\n\
         version = \"0.0.0\"\n\
         \n\
         [workspace]\n\
         pipelines = [\"Sources/pipelines\"]\n",
    )
    .unwrap();

    let lint_output = harn_command()
        .current_dir(root)
        .args(["lint", pipelines.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        lint_output.status.success(),
        "lint failed: status={:?} stdout={} stderr={}",
        lint_output.status,
        String::from_utf8_lossy(&lint_output.stdout),
        String::from_utf8_lossy(&lint_output.stderr)
    );

    let check_output = harn_command()
        .current_dir(root)
        .args(["check", "--workspace"])
        .output()
        .unwrap();
    assert!(
        check_output.status.success(),
        "check --workspace failed: status={:?} stdout={} stderr={}",
        check_output.status,
        String::from_utf8_lossy(&check_output.stdout),
        String::from_utf8_lossy(&check_output.stderr)
    );
}

fn write_cross_directory_cycle_workspace(pipelines: &Path) {
    let dirs = ["context", "runtime", "host", "tools"];
    let files_per_dir = 6;
    for dir in dirs {
        fs::create_dir_all(pipelines.join(dir)).unwrap();
    }
    for (dir_idx, dir) in dirs.iter().enumerate() {
        for file_idx in 0..files_per_dir {
            let mut source = String::new();
            for (other_idx, other) in dirs.iter().enumerate() {
                if other_idx == dir_idx {
                    continue;
                }
                let target = format!("m{}", (file_idx + other_idx) % files_per_dir);
                source.push_str(&format!("import \"../{other}/{target}\"\n"));
            }
            source.push_str(&format!("pub fn {dir}_m{file_idx}() {{ {file_idx} }}\n"));
            fs::write(
                pipelines.join(dir).join(format!("m{file_idx}.harn")),
                source,
            )
            .unwrap();
        }
    }
}
