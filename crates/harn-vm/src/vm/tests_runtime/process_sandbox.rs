//! Per-OS process sandboxing.
//!
//! macOS surfaces a denial as a typed result, Linux blocks a battery of process
//! escapes, and Windows permits in-workspace exec (including argv0) while
//! rejecting writes outside it.

#[cfg(target_os = "windows")]
use crate::VmError;

use super::harness::*;
#[cfg(target_os = "macos")]
#[test]
fn test_macos_process_sandbox_surfaces_denial_as_typed_result() {
    if !std::path::Path::new("/usr/bin/sandbox-exec").exists() {
        return;
    }
    let cwd = std::env::current_dir().unwrap();
    let allowed = tempfile::tempdir_in(&cwd).unwrap();
    let outside_base = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_dir())
        .unwrap_or_else(|| cwd.parent().unwrap_or(cwd.as_path()).to_path_buf());
    if outside_base.starts_with("/tmp") || outside_base.starts_with("/private/tmp") {
        return;
    }
    let outside = tempfile::tempdir_in(outside_base).unwrap();
    let outside_file = outside.path().join("blocked.txt");
    let sandbox_env = crate::stdlib::sandbox::handler_sandbox_test_guard();
    sandbox_env.set("enforce");

    let policy = crate::orchestration::CapabilityPolicy {
        capabilities: std::collections::BTreeMap::from([(
            "process".to_string(),
            vec!["run".to_string()],
        )]),
        workspace_roots: vec![allowed.path().display().to_string()],
        side_effect_level: Some("process_exec".to_string()),
        ..Default::default()
    };
    let source = format!(
        r#"pipeline t(harness: Harness, task: unknown) {{ harness.process.shell("printf denied > '{}'") }}"#,
        outside_file.display()
    );
    let (_, result) = run_harn_with_policy(&source, policy)
        .expect("the process capability returns a typed nonzero-exit receipt");
    let result = result
        .as_dict()
        .expect("HarnessProcess.shell must return a process receipt");
    assert_eq!(
        result
            .get("success")
            .map(crate::VmValue::display)
            .as_deref(),
        Some("false")
    );
    let stderr = result
        .get("stderr")
        .map(crate::VmValue::display)
        .expect("the denied process receipt must include stderr");
    assert!(
        stderr
            .to_ascii_lowercase()
            .contains("operation not permitted"),
        "sandbox denial must be observable in the process receipt, got stderr {stderr:?}"
    );
    assert!(!outside_file.exists());
}

#[cfg(target_os = "linux")]
#[test]
fn test_linux_process_sandbox_catches_ten_process_escapes() {
    let allowed = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let outside_file = outside.path().join("secret.txt");
    let outside_new = outside.path().join("new.txt");
    let outside_copy = outside.path().join("copy.txt");
    let outside_dir = outside.path().join("new_dir");
    let allowed_file = allowed.path().join("allowed.txt");
    std::fs::write(&outside_file, "secret").unwrap();
    std::fs::write(&allowed_file, "allowed").unwrap();

    let sandbox_env = crate::stdlib::sandbox::handler_sandbox_test_guard();
    sandbox_env.set("enforce");

    let policy = crate::orchestration::CapabilityPolicy {
        capabilities: std::collections::BTreeMap::from([
            ("process".to_string(), vec!["run".to_string()]),
            (
                "workspace".to_string(),
                vec![
                    "read_text".to_string(),
                    "list".to_string(),
                    "exists".to_string(),
                    "write_text".to_string(),
                    "delete".to_string(),
                ],
            ),
        ]),
        workspace_roots: vec![allowed.path().display().to_string()],
        side_effect_level: Some("process_exec".to_string()),
        ..Default::default()
    };

    let escapes = [
        format!("cat {}", shell_quote(&outside_file)),
        format!("printf x > {}", shell_quote(&outside_new)),
        format!("printf x >> {}", shell_quote(&outside_file)),
        format!("mkdir {}", shell_quote(&outside_dir)),
        format!("rm {}", shell_quote(&outside_file)),
        format!(
            "cp {} {}",
            shell_quote(&outside_file),
            shell_quote(&allowed.path().join("copy.txt"))
        ),
        format!(
            "cp {} {}",
            shell_quote(&allowed_file),
            shell_quote(&outside_copy)
        ),
        format!(
            "mv {} {}",
            shell_quote(&allowed_file),
            shell_quote(&outside.path().join("moved.txt"))
        ),
        format!(
            "ln -s {} {} && cat {}",
            shell_quote(&outside_file),
            shell_quote(&allowed.path().join("link.txt")),
            shell_quote(&allowed.path().join("link.txt"))
        ),
        format!("touch {}", shell_quote(&outside.path().join("touched.txt"))),
    ];
    assert_eq!(escapes.len(), 10);

    for command in escapes {
        let source = format!(
            r#"pipeline t(harness: Harness, task: unknown) {{ harness.process.shell("{}") }}"#,
            harn_string_escape(&command)
        );
        let (_, result) = run_harn_with_policy(&source, policy.clone())
            .expect("a sandboxed process denial returns a typed process receipt");
        let receipt = result
            .as_dict()
            .expect("HarnessProcess.shell must return a process receipt");
        assert!(
            receipt
                .get("success")
                .is_some_and(|value| matches!(value, crate::VmValue::Bool(false))),
            "expected an unsuccessful process receipt for command {command}, got {receipt:?}"
        );
        assert!(
            receipt
                .get("exit_code")
                .is_some_and(|value| matches!(value, crate::VmValue::Int(code) if *code != 0)),
            "expected a nonzero exit code for command {command}, got {receipt:?}"
        );
    }

    assert!(outside_file.exists());
    assert!(!outside_new.exists());
    assert!(!outside_copy.exists());
    assert!(!outside_dir.exists());
}

#[cfg(target_os = "windows")]
#[test]
fn test_windows_process_sandbox_allows_process_exec_in_workspace() {
    let allowed = tempfile::tempdir().unwrap();
    let allowed_file = allowed.path().join("allowed.txt");
    let sandbox_env = crate::stdlib::sandbox::handler_sandbox_test_guard();
    sandbox_env.set("enforce");

    let policy = crate::orchestration::CapabilityPolicy {
        capabilities: std::collections::BTreeMap::from([
            ("process".to_string(), vec!["run".to_string()]),
            ("workspace".to_string(), vec!["write_text".to_string()]),
        ]),
        workspace_roots: vec![allowed.path().display().to_string()],
        side_effect_level: Some("process_exec".to_string()),
        ..Default::default()
    };
    let command = format!("echo allowed>{}", allowed_file.display());
    let source = format!(
        r#"pipeline t(harness: Harness, task: unknown) {{ harness.process.shell("{}") }}"#,
        harn_string_escape(&command)
    );
    let result = run_harn_with_policy(&source, policy);

    result.unwrap();
    assert!(allowed_file.exists());
}

#[cfg(target_os = "windows")]
#[test]
fn test_windows_process_sandbox_allows_exec_argv0() {
    let allowed = tempfile::tempdir().unwrap();
    let sandbox_env = crate::stdlib::sandbox::handler_sandbox_test_guard();
    sandbox_env.set("enforce");

    let policy = crate::orchestration::CapabilityPolicy {
        capabilities: std::collections::BTreeMap::from([(
            "process".to_string(),
            vec!["run".to_string()],
        )]),
        workspace_roots: vec![allowed.path().display().to_string()],
        side_effect_level: Some("process_exec".to_string()),
        ..Default::default()
    };
    let result = run_harn_with_policy(
        r#"pipeline t(harness: Harness, task: unknown) { harness.process.exec("cmd", "/C", "exit 0") }"#,
        policy,
    );

    result.unwrap();
}

#[cfg(target_os = "windows")]
#[test]
fn test_windows_process_sandbox_denies_write_outside_workspace() {
    let allowed = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let outside_file = outside.path().join("blocked.txt");
    let sandbox_env = crate::stdlib::sandbox::handler_sandbox_test_guard();
    sandbox_env.set("enforce");

    let policy = crate::orchestration::CapabilityPolicy {
        capabilities: std::collections::BTreeMap::from([
            ("process".to_string(), vec!["run".to_string()]),
            ("workspace".to_string(), vec!["write_text".to_string()]),
        ]),
        workspace_roots: vec![allowed.path().display().to_string()],
        side_effect_level: Some("process_exec".to_string()),
        ..Default::default()
    };
    let command = format!("echo denied>{}", outside_file.display());
    let source = format!(
        r#"pipeline t(harness: Harness, task: unknown) {{ harness.process.shell("{}") }}"#,
        harn_string_escape(&command)
    );
    let err = run_harn_with_policy(&source, policy).unwrap_err();

    assert!(matches!(
        err,
        VmError::CategorizedError {
            category: crate::value::ErrorCategory::ToolRejected,
            ..
        }
    ));
    assert!(
        err.to_string().contains("sandbox violation")
            || err.to_string().contains("process sandbox failed"),
        "expected sandbox denial, got {err}"
    );
    assert!(!outside_file.exists());
}

#[cfg(target_os = "linux")]
fn shell_quote(path: &std::path::Path) -> String {
    shell_words::quote(&path.to_string_lossy()).into_owned()
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
fn harn_string_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}
