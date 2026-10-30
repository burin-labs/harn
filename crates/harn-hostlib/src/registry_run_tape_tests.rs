#![cfg(unix)]

use super::HostlibRegistry;
use harn_vm::run_tape::{
    start_recording, EffectScan, RecordingLevel, TapeWorkspace, ToolEffectObserver, TAPE_FILE_NAME,
};
use harn_vm::testbench::tape::{EventTape, TapeRecordKind};
use std::path::Path;
use std::sync::Arc;

#[test]
fn owner_death_guardian_fixture() {
    if crate::process::owner_death::guardian_requested() {
        crate::process::owner_death::run_guardian_from_pipe().unwrap();
    }
}

fn registry() -> HostlibRegistry {
    let mut registry = HostlibRegistry::new().with(crate::tools::ToolsCapability);
    let command = registry
        .builtins
        .builtins
        .iter_mut()
        .find(|entry| entry.name == "hostlib_tools_run_command")
        .unwrap();
    let handler = Arc::clone(&command.handler);
    command.handler = Arc::new(move |args| {
        // Libtest owns main, so the managed child enters this fixture on re-exec.
        let _guardian = crate::process::owner_death::install_guardian_reexec_args([
            "--exact",
            "registry::run_tape_tests::owner_death_guardian_fixture",
            "--nocapture",
        ]);
        handler(args)
    });
    registry
}

fn run(source: &str, root: &Path) -> (harn_vm::VmValue, String) {
    let source = format!("fn main(harness: Harness) {{\n{source}\n}}");
    let chunk = harn_vm::compile_source(&source).unwrap();
    let rt = harn_clock::test_support::current_thread_runtime();
    rt.block_on(harn_clock::test_support::with_local_tasks(async {
        let mut vm = harn_vm::new_vm();
        harn_runtime::register_vm_stdlib(&mut vm);
        registry().register_into_vm(&mut vm);
        vm.set_harness(harn_vm::Harness::real());
        let policy = harn_vm::orchestration::CapabilityPolicy {
            workspace_roots: vec![root.display().to_string()],
            sandbox_profile: harn_vm::orchestration::SandboxProfile::WorkspacePaths,
            ..Default::default()
        };
        let result = harn_vm::orchestration::scope_execution_policy(policy, vm.execute(&chunk))
            .await
            .unwrap();
        (result, vm.output().to_string())
    }))
}

struct NoChanges;

impl ToolEffectObserver for NoChanges {
    fn begin(&self, _: &Path, _: &str) {}
    fn end(&self, _: &Path, _: &str) -> EffectScan {
        EffectScan::default()
    }
}

#[test]
fn a_tool_starting_a_background_command_is_unservable() {
    let dir = tempfile::tempdir().unwrap();
    let _session = harn_vm::agent_sessions::enter_current_session("tape-background-command");
    let guard = start_recording(
        dir.path(),
        RecordingLevel::Full,
        Some(TapeWorkspace {
            root: dir.path().to_path_buf(),
            observer: Box::new(NoChanges),
        }),
    )
    .unwrap()
    .unwrap();
    let root = serde_json::to_string(&dir.path().display().to_string()).unwrap();
    let source = format!(
        r#"
const tools = tool_define(tool_registry(), "launch", "Launch a command", {{
  parameters: {{}},
  handler: {{ _args ->
    const command = harness.tools.run_command({{
      argv: ["/usr/bin/printf", "recorded"], cwd: {root}, sandbox_profile: "workspace_paths",
      background: true, env_mode: "replace",
    }})
    assert(command.handle_id != nil)
    const settled = harness.tools.wait_command({{handle_id: command.handle_id, timeout_ms: 5000}})
    assert(settled.status == "completed")
    harness.stdio.log("spawned-and-settled")
    return "recorded"
  }},
}})
return harness.tools.dispatch_agent_call({{id: "launch-1", name: "launch", arguments: {{}}}}, tools, {{}})
"#
    );
    let (value, output) = run(&source, dir.path());
    assert!(
        output.contains("spawned-and-settled"),
        "{value:?}: {output}"
    );
    assert_eq!(guard.error(), None);
    drop(guard);
    let tape = EventTape::load(&dir.path().join(TAPE_FILE_NAME)).unwrap();
    let commands: Vec<_> = tape
        .records
        .iter()
        .filter_map(|record| match &record.kind {
            TapeRecordKind::ToolCall {
                tool, unservable, ..
            } if tool == "launch" => Some(unservable),
            _ => None,
        })
        .collect();
    assert_eq!(commands.len(), 1);
    assert!(commands[0]
        .as_ref()
        .is_some_and(|reason| reason.contains("background")));
}
