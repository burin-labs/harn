//! Workspace and read-only roots as a filesystem boundary.
//!
//! Escapes are caught whether they are attempted through the file builtins,
//! through template rendering, or by moving the process cwd; a read-only root
//! serves reads and rejects writes.

use crate::{VmError, VmValue};

use super::harness::*;
#[test]
fn test_policy_workspace_roots_catch_filesystem_escapes() {
    let allowed = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let outside_file = outside.path().join("secret.txt");
    std::fs::write(&outside_file, "secret").unwrap();
    let outside_copy = outside.path().join("copy.txt");
    let outside_new = outside.path().join("new.txt");
    let outside_dir = outside.path().join("new_dir");

    let policy = crate::orchestration::CapabilityPolicy {
        capabilities: std::collections::BTreeMap::from([(
            "workspace".to_string(),
            vec![
                "read_text".to_string(),
                "list".to_string(),
                "exists".to_string(),
                "write_text".to_string(),
                "delete".to_string(),
            ],
        )]),
        workspace_roots: vec![allowed.path().display().to_string()],
        side_effect_level: Some("workspace_write".to_string()),
        ..Default::default()
    };

    let escapes = [
        format!(
            r#"pipeline t(task) {{ read_file("{}") }}"#,
            outside_file.display()
        ),
        format!(
            r#"pipeline t(task) {{ read_file_bytes("{}") }}"#,
            outside_file.display()
        ),
        format!(
            r#"pipeline t(task) {{ write_file("{}", "x") }}"#,
            outside_new.display()
        ),
        format!(
            r#"pipeline t(task) {{ append_file("{}", "x") }}"#,
            outside_file.display()
        ),
        format!(
            r#"pipeline t(task) {{ copy_file("{}", "{}") }}"#,
            outside_file.display(),
            allowed.path().join("copy.txt").display()
        ),
        format!(
            r#"pipeline t(task) {{ copy_file("{}", "{}") }}"#,
            allowed.path().join("missing.txt").display(),
            outside_copy.display()
        ),
        format!(
            r#"pipeline t(task) {{ list_dir("{}") }}"#,
            outside.path().display()
        ),
        format!(
            r#"pipeline t(task) {{ mkdir("{}") }}"#,
            outside_dir.display()
        ),
        format!(
            r#"pipeline t(task) {{ stat("{}") }}"#,
            outside_file.display()
        ),
        format!(
            r#"pipeline t(task) {{ delete_file("{}") }}"#,
            outside_file.display()
        ),
    ];

    for source in escapes {
        let err = run_harn_with_policy(&source, policy.clone()).unwrap_err();
        assert!(
            matches!(
                err,
                VmError::CategorizedError {
                    category: crate::value::ErrorCategory::ToolRejected,
                    ..
                }
            ),
            "expected tool_rejected for source {source}, got {err:?}"
        );
        assert!(
            err.to_string().contains("sandbox violation"),
            "expected sandbox violation message, got {err}"
        );
    }

    // `file_exists`/`exists` is the one read-scope probe that soft-falses
    // instead of throwing on an out-of-root path (v0.8.55): an absent path and
    // an out-of-sandbox path are indistinguishable to a caller, so the safe,
    // non-leaky answer is `false` rather than a sandbox violation. Content
    // reads (`read_file`, asserted above) still error — only presence probing
    // is softened.
    let (_, exists_outside) = run_harn_with_policy(
        &format!(
            r#"pipeline t(task) {{ file_exists("{}") }}"#,
            outside_file.display()
        ),
        policy,
    )
    .expect("file_exists outside sandbox should soft-false, not error");
    assert!(
        matches!(exists_outside, VmValue::Bool(false)),
        "file_exists on an out-of-root path must read as absent, got {exists_outside:?}"
    );
}

#[test]
fn test_policy_read_only_root_allows_reads_but_rejects_writes() {
    let writable = tempfile::tempdir().unwrap();
    let read_only = tempfile::tempdir().unwrap();
    let read_only_file = read_only.path().join("memory.txt");
    std::fs::write(&read_only_file, "secret").unwrap();

    let policy = crate::orchestration::CapabilityPolicy {
        capabilities: std::collections::BTreeMap::from([(
            "workspace".to_string(),
            vec![
                "read_text".to_string(),
                "list".to_string(),
                "exists".to_string(),
                "write_text".to_string(),
                "delete".to_string(),
            ],
        )]),
        workspace_roots: vec![writable.path().display().to_string()],
        read_only_roots: vec![read_only.path().display().to_string()],
        side_effect_level: Some("workspace_write".to_string()),
        ..Default::default()
    };

    // Reading from a read-only root succeeds.
    let read_source = format!(
        r#"pipeline t(task) {{ return read_file("{}") }}"#,
        read_only_file.display()
    );
    let (_out, value) = run_harn_with_policy(&read_source, policy.clone()).unwrap();
    assert_eq!(value.display(), "secret");

    // Mutating an existing file under a read-only root is rejected with
    // the read-only-specific message even though the workspace grants
    // write_text/delete. These target the existing file so the path
    // canonicalizes identically on every platform (Windows resolves a
    // non-existent target to a `\\?\` verbatim path that the generic
    // out-of-scope branch reports instead — still rejected, just a
    // coarser message; the path-scope logic itself is covered for the
    // non-existent case by the `sandbox_hardened` integration test).
    let existing_mutations = [
        format!(
            r#"pipeline t(task) {{ write_file("{}", "x") }}"#,
            read_only_file.display()
        ),
        format!(
            r#"pipeline t(task) {{ append_file("{}", "x") }}"#,
            read_only_file.display()
        ),
        format!(
            r#"pipeline t(task) {{ delete_file("{}") }}"#,
            read_only_file.display()
        ),
    ];
    for source in existing_mutations {
        let err = run_harn_with_policy(&source, policy.clone()).unwrap_err();
        assert!(
            matches!(
                err,
                VmError::CategorizedError {
                    category: crate::value::ErrorCategory::ToolRejected,
                    ..
                }
            ),
            "expected tool_rejected for source {source}, got {err:?}"
        );
        assert!(
            err.to_string().contains("read-only workspace root"),
            "expected read-only rejection message, got {err}"
        );
    }

    // Creating a new file under a read-only root is likewise rejected.
    let create = format!(
        r#"pipeline t(task) {{ write_file("{}", "x") }}"#,
        read_only.path().join("new.txt").display()
    );
    let err = run_harn_with_policy(&create, policy).unwrap_err();
    assert!(
        matches!(
            err,
            VmError::CategorizedError {
                category: crate::value::ErrorCategory::ToolRejected,
                ..
            }
        ),
        "creating a new file under a read-only root must be rejected, got {err:?}"
    );
    assert!(
        err.to_string().contains("sandbox violation"),
        "expected sandbox violation, got {err}"
    );
    assert!(
        !read_only.path().join("new.txt").exists(),
        "rejected write must not touch disk"
    );
}

#[test]
fn test_policy_workspace_roots_catch_template_render_escapes() {
    let allowed = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let outside_template = outside.path().join("secret.harn.prompt");
    std::fs::write(&outside_template, "TOP_SECRET_RENDER_BYPASS").unwrap();

    let policy = crate::orchestration::CapabilityPolicy {
        capabilities: std::collections::BTreeMap::from([
            ("workspace".to_string(), vec!["read_text".to_string()]),
            ("template".to_string(), vec!["render".to_string()]),
        ]),
        workspace_roots: vec![allowed.path().display().to_string()],
        side_effect_level: Some("read_only".to_string()),
        ..Default::default()
    };

    let escaped_path = outside_template.display();
    let escapes = [
        format!(
            r#"pipeline t(harness: Harness, task) {{ harness.fs.render_prompt("{escaped_path}") }}"#
        ),
        format!(
            r#"pipeline t(harness: Harness, task) {{ harness.fs.render_prompt_with_provenance("{escaped_path}") }}"#
        ),
    ];

    for source in escapes {
        let err = run_harn_with_policy(&source, policy.clone()).unwrap_err();
        assert!(
            err.to_string().contains("sandbox violation"),
            "expected sandbox violation for source {source}, got {err}"
        );
    }
}

#[test]
fn test_policy_workspace_roots_reject_process_cwd_escape() {
    let allowed = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let policy = crate::orchestration::CapabilityPolicy {
        capabilities: std::collections::BTreeMap::from([(
            "process".to_string(),
            vec!["exec".to_string()],
        )]),
        workspace_roots: vec![allowed.path().display().to_string()],
        side_effect_level: Some("process_exec".to_string()),
        ..Default::default()
    };

    let source = format!(
        r#"pipeline t(task) {{ exec_at("{}", "sh", "-c", "true") }}"#,
        outside.path().display()
    );
    let err = run_harn_with_policy(&source, policy).unwrap_err();
    assert!(matches!(
        err,
        VmError::CategorizedError {
            category: crate::value::ErrorCategory::ToolRejected,
            ..
        }
    ));
    assert!(err.to_string().contains("process cwd"));
}
