//! Real-child environment checks for the hostlib process-sandbox projection.

#![cfg(unix)]

use std::sync::Arc;

use harn_hostlib::tools::ToolsCapability;
use harn_hostlib::{BuiltinRegistry, HostlibCapability, HostlibError};
use harn_vm::VmValue;

fn call(request: harn_vm::value::DictMap) -> Result<VmValue, HostlibError> {
    let mut registry = BuiltinRegistry::new();
    ToolsCapability.register_builtins(&mut registry);
    let entry = registry
        .find("hostlib_tools_run_command")
        .expect("run_command builtin must be registered");
    (entry.handler)(&[VmValue::dict(request)])
}

fn value(value: &str) -> VmValue {
    VmValue::String(arcstr::ArcStr::from(value))
}

fn command_request(cwd: &str) -> harn_vm::value::DictMap {
    let mut request = harn_vm::value::DictMap::new();
    request.insert(
        "argv".into(),
        VmValue::List(Arc::new(
            [
                "sh",
                "-c",
                "printf '<%s>|<%s>|<%s>|<%s>' \"$RUSTC_WRAPPER\" \"$CARGO_BUILD_RUSTC_WRAPPER\" \"$RUSTC_WORKSPACE_WRAPPER\" \"$CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER\"",
            ]
            .into_iter()
            .map(value)
            .collect(),
        )),
    );
    request.insert("cwd".into(), value(cwd));
    request
}

fn response_string(response: &harn_vm::value::DictMap, key: &str) -> String {
    match response.get(key) {
        Some(VmValue::String(value)) => value.to_string(),
        other => panic!("expected string at {key}, got {other:?}"),
    }
}

#[test]
fn real_run_command_neutralizes_rustc_wrappers_inside_sandbox() {
    use harn_vm::orchestration::{
        pop_execution_policy, push_execution_policy, CapabilityPolicy, SandboxProfile,
    };

    let workspace = tempfile::tempdir().expect("workspace");

    // `warn` keeps the Worktree process policy active while allowing hosts
    // without an OS confinement backend to exercise the environment contract.
    // SAFETY: the shared lock serializes every environment-mutating test in
    // this binary, and all five variables are restored before the guard drops.
    let _env_guard = super::process_tools_e2e::lock_env();
    let old_handler_sandbox = std::env::var_os("HARN_HANDLER_SANDBOX");
    let old_rustc_wrapper = std::env::var_os("RUSTC_WRAPPER");
    let old_cargo_wrapper = std::env::var_os("CARGO_BUILD_RUSTC_WRAPPER");
    let old_workspace_wrapper = std::env::var_os("RUSTC_WORKSPACE_WRAPPER");
    let old_cargo_workspace_wrapper = std::env::var_os("CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER");
    unsafe {
        std::env::set_var("HARN_HANDLER_SANDBOX", "warn");
        std::env::set_var("RUSTC_WRAPPER", "/outside/sandbox/sccache");
        std::env::set_var(
            "CARGO_BUILD_RUSTC_WRAPPER",
            "/outside/sandbox/cargo-sccache",
        );
        std::env::set_var(
            "RUSTC_WORKSPACE_WRAPPER",
            "/outside/sandbox/workspace-sccache",
        );
        std::env::set_var(
            "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
            "/outside/sandbox/cargo-workspace-sccache",
        );
    }
    push_execution_policy(CapabilityPolicy {
        sandbox_profile: SandboxProfile::Worktree,
        workspace_roots: vec![workspace.path().to_string_lossy().into_owned()],
        ..CapabilityPolicy::default()
    });

    let cwd = workspace.path().to_string_lossy();
    let inherited_response = call(command_request(&cwd));

    let mut caller_request = command_request(&cwd);
    let mut caller_env = harn_vm::value::DictMap::new();
    caller_env.insert("RUSTC_WRAPPER".into(), value("/caller/sccache"));
    caller_env.insert(
        "CARGO_BUILD_RUSTC_WRAPPER".into(),
        value("/caller/cargo-sccache"),
    );
    caller_env.insert(
        "RUSTC_WORKSPACE_WRAPPER".into(),
        value("/caller/workspace-sccache"),
    );
    caller_env.insert(
        "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER".into(),
        value("/caller/cargo-workspace-sccache"),
    );
    caller_request.insert("env".into(), VmValue::dict(caller_env));
    caller_request.insert(
        "env_remove".into(),
        VmValue::List(Arc::new(
            [
                "rustc_wrapper",
                "cargo_build_rustc_wrapper",
                "rustc_workspace_wrapper",
                "cargo_build_rustc_workspace_wrapper",
            ]
            .into_iter()
            .map(value)
            .collect(),
        )),
    );
    caller_request.insert("env_mode".into(), value("patch"));
    let caller_response = call(caller_request);

    pop_execution_policy();
    unsafe {
        match old_handler_sandbox {
            Some(value) => std::env::set_var("HARN_HANDLER_SANDBOX", value),
            None => std::env::remove_var("HARN_HANDLER_SANDBOX"),
        }
        match old_rustc_wrapper {
            Some(value) => std::env::set_var("RUSTC_WRAPPER", value),
            None => std::env::remove_var("RUSTC_WRAPPER"),
        }
        match old_cargo_wrapper {
            Some(value) => std::env::set_var("CARGO_BUILD_RUSTC_WRAPPER", value),
            None => std::env::remove_var("CARGO_BUILD_RUSTC_WRAPPER"),
        }
        match old_workspace_wrapper {
            Some(value) => std::env::set_var("RUSTC_WORKSPACE_WRAPPER", value),
            None => std::env::remove_var("RUSTC_WORKSPACE_WRAPPER"),
        }
        match old_cargo_workspace_wrapper {
            Some(value) => std::env::set_var("CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER", value),
            None => std::env::remove_var("CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER"),
        }
    }

    for (source, response) in [
        ("inherited", inherited_response),
        ("caller-supplied", caller_response),
    ] {
        let VmValue::Dict(response) = response.expect("sandboxed command should run") else {
            panic!("sandboxed command response must be a dict");
        };
        assert_eq!(
            response_string(&response, "stdout"),
            "<>|<>|<>|<>",
            "the real host-process path must override {source} and Cargo-configured wrappers"
        );
    }
}
