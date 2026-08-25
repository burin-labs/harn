use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::super::{execute_run, CliLlmMockMode, RunProfileOptions};

/// Write an entry graph whose imported helper names `host_call`, with
/// `[check].trusted_host_dispatch` set to `declared`.
fn write_imported_host_dispatch_project(root: &Path, declared: bool) -> PathBuf {
    std::fs::create_dir_all(root.join(".git")).expect("project boundary");
    std::fs::write(
        root.join("harn.toml"),
        format!("[check]\ntrusted_host_dispatch = {declared}\n"),
    )
    .expect("write manifest");
    std::fs::write(
        root.join("host_adapter.harn"),
        r#"
pub fn unreachable_host_read() -> any {
  return host_call("runtime.pipeline_input", {})
}
"#,
    )
    .expect("write host adapter");
    let script = root.join("main.harn");
    std::fs::write(
        &script,
        r#"
import { unreachable_host_read } from "./host_adapter"

pipeline main(harness: Harness) {
  const _ = unreachable_host_read()
  harness.stdio.println("target-ran")
}
"#,
    )
    .expect("write entry script");
    script
}

async fn run_host_dispatch_fixture(declared: bool) -> crate::commands::run::RunOutcome {
    harn_vm::reset_thread_local_state();
    let project = tempfile::tempdir().expect("temp project");
    let script = write_imported_host_dispatch_project(project.path(), declared);
    assert_eq!(
        crate::compiler_context::trusted_host_dispatch_for_source(&script),
        declared,
        "fixture manifest authority"
    );
    let _bridge = harn_vm::install_host_call_bridge(std::sync::Arc::new(DispatchBoundaryBridge));
    let outcome = execute_run(
        &script.to_string_lossy(),
        false,
        HashSet::new(),
        Vec::new(),
        Vec::new(),
        CliLlmMockMode::Off,
        None,
        RunProfileOptions::default(),
    )
    .await;
    harn_vm::reset_thread_local_state();
    outcome
}

struct DispatchBoundaryBridge;

impl harn_vm::HostCallBridge for DispatchBoundaryBridge {
    fn dispatch<'a>(
        &'a self,
        capability: &'a str,
        operation: &'a str,
        _params: &'a harn_vm::value::DictMap,
    ) -> harn_vm::HostCallDispatchFuture<'a> {
        assert_eq!((capability, operation), ("runtime", "pipeline_input"));
        harn_vm::host_call_ready(Ok(Some(harn_vm::VmValue::String("bridge-reached".into()))))
    }
}

#[tokio::test]
async fn execute_run_honors_manifest_trusted_host_dispatch() {
    let outcome = run_host_dispatch_fixture(true).await;
    assert_eq!(
        outcome.exit_code, 0,
        "stderr:\n{}\nstdout:\n{}",
        outcome.stderr, outcome.stdout
    );
    assert_eq!(outcome.stdout.trim(), "target-ran");
}

#[tokio::test]
async fn execute_run_rejects_imported_host_dispatch_without_manifest_authority() {
    let outcome = run_host_dispatch_fixture(false).await;
    assert_ne!(
        outcome.exit_code, 0,
        "ordinary module graph unexpectedly ran:\nstdout:\n{}\nstderr:\n{}",
        outcome.stdout, outcome.stderr
    );
    assert!(outcome.stderr.contains("host_call"), "{}", outcome.stderr);
    assert!(
        outcome.stderr.contains("not callable source API"),
        "{}",
        outcome.stderr
    );
    assert!(
        !outcome.stdout.contains("target-ran"),
        "denied graph reached its entry pipeline: {}",
        outcome.stdout
    );
}
