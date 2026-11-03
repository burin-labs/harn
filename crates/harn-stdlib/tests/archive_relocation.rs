use std::path::Path;
use std::process::Command;

include!(concat!(env!("OUT_DIR"), "/stdlib_archive.rs"));

fn compile_and_run(checkout: &Path, shared_target: &Path) {
    let source = checkout.join("probe.rs");
    std::fs::write(
        &source,
        r#"include!(concat!(env!("OUT_DIR"), "/stdlib_archive.rs"));
fn main() {
    assert!(!ARCHIVE.is_empty());
    assert_eq!(LENGTHS.iter().sum::<usize>(), ARCHIVE_TEXT_LEN);
    println!("archive_bytes={}", ARCHIVE.len());
}
"#,
    )
    .unwrap();
    let executable = shared_target.join(format!("probe{}", std::env::consts::EXE_SUFFIX));
    let compiler = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let compiled = Command::new(compiler)
        .args(["--edition=2021", "--crate-name", "archive_relocation_probe"])
        .arg(&source)
        .arg("-o")
        .arg(&executable)
        .env("OUT_DIR", checkout.join("generated"))
        .output()
        .expect("run rustc on the cached archive bindings");
    assert!(
        compiled.status.success(),
        "cached archive bindings failed in {}: {}",
        checkout.display(),
        String::from_utf8_lossy(&compiled.stderr)
    );
    let executed = Command::new(&executable).output().unwrap();
    assert!(executed.status.success(), "{executed:?}");
    assert_eq!(
        String::from_utf8(executed.stdout).unwrap(),
        format!("archive_bytes={}\n", ARCHIVE.len())
    );
}

#[test]
fn cached_archive_bindings_survive_a_moved_checkout() {
    assert!(!ARCHIVE.is_empty());
    assert_eq!(LENGTHS.iter().sum::<usize>(), ARCHIVE_TEXT_LEN);
    let root = tempfile::tempdir().unwrap();
    let first = root.path().join("checkout-a");
    let second = root.path().join("checkout-b");
    let shared_target = root.path().join("shared-target");
    std::fs::create_dir_all(first.join("generated")).unwrap();
    std::fs::create_dir_all(second.join("generated")).unwrap();
    std::fs::create_dir(&shared_target).unwrap();

    // Replay the actual emitted bindings at the first checkout's output path.
    let generated = include_str!(concat!(env!("OUT_DIR"), "/stdlib_archive.rs")).replace(
        &format!(
            "{:?}",
            Path::new(env!("OUT_DIR"))
                .join("stdlib.zst")
                .display()
                .to_string()
        ),
        &format!(
            "{:?}",
            first.join("generated/stdlib.zst").display().to_string()
        ),
    );
    std::fs::write(first.join("generated/stdlib_archive.rs"), generated).unwrap();
    std::fs::write(first.join("generated/stdlib.zst"), ARCHIVE).unwrap();
    compile_and_run(&first, &shared_target);

    for file in ["stdlib_archive.rs", "stdlib.zst"] {
        std::fs::copy(
            first.join("generated").join(file),
            second.join("generated").join(file),
        )
        .unwrap();
    }
    std::fs::remove_dir_all(&first).unwrap();
    assert!(!first.exists());
    compile_and_run(&second, &shared_target);
}
