use std::fs;
use std::process::Command;

use tempfile::TempDir;

#[test]
fn check_reports_unknown_struct_type_in_stderr() {
    let temp = TempDir::new().unwrap();
    let script = temp.path().join("main.harn");
    fs::write(&script, "let p = Point { x: 3, y: 4 }\n").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_harn"))
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
