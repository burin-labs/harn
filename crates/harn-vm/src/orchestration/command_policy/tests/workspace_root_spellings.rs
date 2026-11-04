use super::*;

/// A root declared through a linked folder and a cwd reported in its resolved
/// spelling name the same directory. Tools such as `git rev-parse
/// --show-toplevel` and `pwd -P` report the resolved spelling, so a lexical
/// comparison would hold every command they feed for approval.
#[cfg(unix)]
#[test]
fn deterministic_scan_accepts_both_spellings_of_a_linked_workspace_root() {
    let temp = tempfile::tempdir().unwrap();
    let real = temp.path().join("real-project");
    std::fs::create_dir_all(real.join("src")).unwrap();
    let linked = temp.path().join("linked-project");
    std::os::unix::fs::symlink(&real, &linked).unwrap();
    let outside = temp.path().join("elsewhere");
    std::fs::create_dir_all(&outside).unwrap();
    let real_spelled = real.canonicalize().unwrap();

    let scan_labels = |root: &std::path::Path, cwd: &std::path::Path, operand: &std::path::Path| {
        labels(&command_risk_scan_json(
            &serde_json::json!({
                "request": {
                    "mode": "argv",
                    "argv": ["cat", operand.to_str().unwrap()],
                    "cwd": cwd.to_str().unwrap(),
                },
                "workspace_roots": [root.to_str().unwrap()],
            }),
            None,
        ))
    };
    let outside_label = "outside_workspace".to_string();

    for (root, cwd, operand) in [
        (&linked, &real_spelled, real_spelled.join("src/main.rs")),
        (&real_spelled, &linked, linked.join("src/main.rs")),
        (&linked, &linked, real_spelled.join("README.md")),
    ] {
        assert!(
            !scan_labels(root, cwd, &operand).contains(&outside_label),
            "root {root:?}, cwd {cwd:?}, operand {operand:?} name the same workspace"
        );
    }

    for (cwd, operand) in [
        (&outside, linked.join("README.md")),
        (&linked, outside.join("notes.txt")),
    ] {
        assert!(
            scan_labels(&linked, cwd, &operand).contains(&outside_label),
            "cwd {cwd:?}, operand {operand:?} leave the workspace"
        );
    }
}
