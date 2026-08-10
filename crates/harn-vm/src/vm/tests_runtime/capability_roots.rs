//! Workspace and read-only roots as a filesystem boundary.
//!
//! Escapes are caught whether they are attempted through the file builtins,
//! through template rendering, or by moving the process cwd; a read-only root
//! serves reads and rejects writes.

use crate::{VmError, VmValue};

use super::harness::*;

#[test]
fn legacy_call_derives_the_declared_leading_sub_capability() {
    let strict_source = r#"
fn describe(env: HarnessEnv, fs: HarnessFs, value: string) -> string {
  return type_of(env) + ":" + type_of(fs) + ":" + value
}

pipeline main(harness: Harness) {
  return describe("ok")
}
"#;
    let legacy_source = r#"
fn describe(env: HarnessEnv, fs: HarnessFs, value: string) -> string {
  return type_of(env) + ":" + type_of(fs) + ":" + value
}

pipeline main(harness: Harness) {
  const span = trace_start("legacy-call")
  trace_end(span)
  const alias_result = regex_replace_all("x", "y", "x")
  return describe("ok") + ":" + alias_result
}
"#;

    let strict =
        run_harn_result(strict_source).expect_err("strict runtime must reject omitted authority");
    assert!(
        strict
            .to_string()
            .contains("expects at least 3 arguments, got 1"),
        "unexpected strict error: {strict}"
    );

    let (_, value) = run_harn_with_legacy_ambient_capabilities(legacy_source);
    assert_eq!(value.display(), "HarnessEnv:HarnessFs:ok:y");
}

#[test]
fn legacy_ambient_projects_declared_capability_global_names() {
    // Pre-cutover ambient globals are spelled `runtime_context_set` while the
    // typed contract is published as `__cap_runtime_context_set`. The ambient
    // bridge must accept the historical spelling and dispatch on the parent VM.
    let source = r#"
pipeline main(harness: Harness) {
  runtime_context_set("ambient-bridge", 7)
  return runtime_context_get("ambient-bridge", 0)
}
"#;
    let (_, value) = run_harn_with_legacy_ambient_capabilities(source);
    assert_eq!(value.display(), "7");
}

#[test]
fn harness_fs_source_dir_tracks_the_owning_imported_module() {
    let project = tempfile::tempdir().unwrap();
    let library_dir = project.path().join("lib");
    std::fs::create_dir_all(&library_dir).unwrap();
    std::fs::write(
        library_dir.join("paths.harn"),
        "pub fn local_source_dir(fs: HarnessFs) { return fs.source_dir() }\n",
    )
    .unwrap();
    let entry = project.path().join("entry.harn");
    let source = r#"
import { local_source_dir } from "./lib/paths"

pipeline main(harness: Harness) {
  return local_source_dir(harness.fs)
}
"#;

    let (_, value) = run_harn_at(&entry, source).expect("entry executes");
    let VmValue::String(path) = &value else {
        panic!("source_dir must return a string, got {value:?}");
    };
    assert_eq!(
        std::path::Path::new(path.as_str()).canonicalize().unwrap(),
        library_dir.canonicalize().unwrap(),
        "source_dir must retain lexical module ownership after attenuation"
    );
}

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
                "apply_edit".to_string(),
            ],
        )]),
        workspace_roots: vec![allowed.path().display().to_string()],
        side_effect_level: Some("workspace_write".to_string()),
        ..Default::default()
    };

    let escapes = [
        format!(
            r#"pipeline t(harness: Harness, task) {{ harness.fs.read_text("{}") }}"#,
            outside_file.display()
        ),
        format!(
            r#"pipeline t(harness: Harness, task) {{ harness.fs.read_bytes("{}") }}"#,
            outside_file.display()
        ),
        format!(
            r#"pipeline t(harness: Harness, task) {{ harness.fs.write_text("{}", "x") }}"#,
            outside_new.display()
        ),
        format!(
            r#"pipeline t(harness: Harness, task) {{ harness.fs.append("{}", "x") }}"#,
            outside_file.display()
        ),
        format!(
            r#"pipeline t(harness: Harness, task) {{ harness.fs.copy("{}", "{}") }}"#,
            outside_file.display(),
            allowed.path().join("copy.txt").display()
        ),
        format!(
            r#"pipeline t(harness: Harness, task) {{ harness.fs.copy("{}", "{}") }}"#,
            allowed.path().join("missing.txt").display(),
            outside_copy.display()
        ),
        format!(
            r#"pipeline t(harness: Harness, task) {{ harness.fs.list_dir("{}") }}"#,
            outside.path().display()
        ),
        format!(
            r#"pipeline t(harness: Harness, task) {{ harness.fs.mkdir("{}") }}"#,
            outside_dir.display()
        ),
        format!(
            r#"pipeline t(harness: Harness, task) {{ harness.fs.delete("{}") }}"#,
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

    // Presence and status probes do not throw for an out-of-root path. `exists`
    // deliberately makes absence and denial indistinguishable, while `status`
    // returns the typed denial envelope needed by host diagnostics.
    let (_, exists_outside) = run_harn_with_policy(
        &format!(
            r#"pipeline t(harness: Harness, task) {{ harness.fs.exists("{}") }}"#,
            outside_file.display()
        ),
        policy.clone(),
    )
    .expect("file_exists outside sandbox should soft-false, not error");
    assert!(
        matches!(exists_outside, VmValue::Bool(false)),
        "file_exists on an out-of-root path must read as absent, got {exists_outside:?}"
    );
    let (_, status_outside) = run_harn_with_policy(
        &format!(
            r#"pipeline t(harness: Harness, task) {{ harness.fs.status("{}") }}"#,
            outside_file.display()
        ),
        policy,
    )
    .expect("path status outside sandbox should return a typed denial");
    let status = status_outside
        .as_dict()
        .expect("HarnessFs.status must return a record");
    assert_eq!(
        status.get("status").map(VmValue::display).as_deref(),
        Some("scope_denied")
    );
}

#[test]
fn recursive_mkdir_treats_existing_sandbox_roots_as_read_only_noops() {
    let writable = tempfile::tempdir().unwrap();
    let read_only = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let existing_file = writable.path().join("not-a-directory");
    std::fs::write(&existing_file, "file").unwrap();

    let policy = crate::orchestration::CapabilityPolicy {
        workspace_roots: vec![writable.path().display().to_string()],
        read_only_roots: vec![read_only.path().display().to_string()],
        side_effect_level: Some("workspace_write".to_string()),
        ..Default::default()
    };

    for existing_root in [writable.path(), read_only.path()] {
        run_harn_with_policy(
            &format!(
                r#"pipeline t(harness: Harness, task) {{ harness.fs.mkdir("{}", true) }}"#,
                existing_root.display()
            ),
            policy.clone(),
        )
        .expect("recursive mkdir of an existing sandbox root must be a no-op");
        assert!(existing_root.is_dir());
    }

    run_harn_with_policy(
        &format!(
            r#"pipeline t(harness: Harness, task) {{ harness.fs.mkdir("{}", true) }}"#,
            existing_file.display()
        ),
        policy.clone(),
    )
    .expect_err("recursive mkdir must not accept an existing file");
    assert_eq!(std::fs::read_to_string(&existing_file).unwrap(), "file");

    let missing_read_only = read_only.path().join("missing");
    let read_only_error = run_harn_with_policy(
        &format!(
            r#"pipeline t(harness: Harness, task) {{ harness.fs.mkdir("{}", true) }}"#,
            missing_read_only.display()
        ),
        policy.clone(),
    )
    .expect_err("creating under a read-only root must still be rejected");
    assert!(read_only_error
        .to_string()
        .contains("read-only workspace root"));
    assert!(!missing_read_only.exists());

    let outside_error = run_harn_with_policy(
        &format!(
            r#"pipeline t(harness: Harness, task) {{ harness.fs.mkdir("{}", true) }}"#,
            outside.path().display()
        ),
        policy,
    )
    .expect_err("an existing out-of-root directory must remain unreadable");
    assert!(outside_error.to_string().contains("sandbox violation"));
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
                "apply_edit".to_string(),
            ],
        )]),
        workspace_roots: vec![writable.path().display().to_string()],
        read_only_roots: vec![read_only.path().display().to_string()],
        side_effect_level: Some("workspace_write".to_string()),
        ..Default::default()
    };

    // Reading from a read-only root succeeds.
    let read_source = format!(
        r#"pipeline t(harness: Harness, task) {{ return harness.fs.read_text("{}") }}"#,
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
            r#"pipeline t(harness: Harness, task) {{ harness.fs.write_text("{}", "x") }}"#,
            read_only_file.display()
        ),
        format!(
            r#"pipeline t(harness: Harness, task) {{ harness.fs.append("{}", "x") }}"#,
            read_only_file.display()
        ),
        format!(
            r#"pipeline t(harness: Harness, task) {{ harness.fs.delete("{}") }}"#,
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
        r#"pipeline t(harness: Harness, task) {{ harness.fs.write_text("{}", "x") }}"#,
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
        capabilities: std::collections::BTreeMap::from([
            ("process".to_string(), vec!["run".to_string()]),
            ("workspace".to_string(), vec!["read_text".to_string()]),
        ]),
        workspace_roots: vec![allowed.path().display().to_string()],
        side_effect_level: Some("process_exec".to_string()),
        ..Default::default()
    };

    let source = format!(
        r#"pipeline t(harness: Harness, task) {{ harness.process.exec_at("{}", "sh", "-c", "true") }}"#,
        outside.path().display()
    );
    let err = run_harn_with_policy(&source, policy).unwrap_err();
    assert!(
        matches!(
            err,
            VmError::CategorizedError {
                category: crate::value::ErrorCategory::ToolRejected,
                ..
            }
        ),
        "expected typed process-cwd rejection, got {err:?}"
    );
    assert!(
        err.to_string().contains("process cwd"),
        "expected process-cwd sandbox denial, got {err}"
    );
}
