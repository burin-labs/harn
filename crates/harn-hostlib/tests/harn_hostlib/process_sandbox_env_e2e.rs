//! Real-child environment checks for the hostlib process-sandbox projection.

#![cfg(unix)]

use std::sync::Arc;

use harn_hostlib::tools::ToolsCapability;
use harn_hostlib::{BuiltinRegistry, HostlibCapability, HostlibError};
use harn_vm::VmValue;

fn call(request: harn_vm::value::DictMap) -> Result<VmValue, HostlibError> {
    // This test is about toolchain-wrapper neutralization, not credential
    // scope, and it inherits on purpose. Since harn#8477 an inheriting spawn
    // has to say so, so it says so here rather than relying on the absence of
    // a policy, which now refuses.
    let _environment = harn_vm::stdlib::process::declare_session_environment_if_absent(
        harn_vm::security::SessionEnvironment::inherited(),
    );
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

    let canonical_workspace = workspace.path().canonicalize().unwrap();
    let workspace_decisions = || {
        harn_vm::process_sandbox::rustc_wrapper::rustc_wrapper_decisions()
            .into_iter()
            .filter(|decision| {
                std::path::Path::new(&decision.cwd)
                    .canonicalize()
                    .ok()
                    .as_ref()
                    == Some(&canonical_workspace)
            })
            .collect::<Vec<_>>()
    };
    assert!(
        workspace_decisions().is_empty(),
        "shell launches must disable wrappers without measuring a compiler build"
    );

    std::fs::create_dir(workspace.path().join("src")).unwrap();
    std::fs::write(workspace.path().join("Cargo.toml"), "[package]\nname = \"replacement-env\"\nversion = \"0.0.0\"\nedition = \"2021\"\n[workspace]\n").unwrap();
    std::fs::write(workspace.path().join("src/main.rs"), "fn main() {}\n").unwrap();
    let mut replacement = command_request(&cwd);
    replacement.insert(
        "argv".into(),
        VmValue::List(Arc::new(
            ["cargo", "build", "--offline"]
                .into_iter()
                .map(value)
                .collect(),
        )),
    );
    replacement.insert("env_mode".into(), value("replace"));
    let mut replacement_env = harn_vm::value::DictMap::new();
    for key in ["PATH", "HOME", "RUSTUP_HOME"] {
        if let Ok(entry) = std::env::var(key) {
            replacement_env.insert(key.into(), value(&entry));
        }
    }
    replacement_env.insert("CARGO_HOME".into(), value(&cwd));
    replacement_env.insert(
        "CARGO_TARGET_DIR".into(),
        value(&workspace.path().join("target").to_string_lossy()),
    );
    replacement.insert("env".into(), VmValue::dict(replacement_env.clone()));
    let replacement_response = call(replacement.clone());
    assert!(replacement_response.is_ok(), "{replacement_response:?}");
    assert!(
        workspace
            .path()
            .join("target/debug/replacement-env")
            .is_file(),
        "the real replacement-environment build must finish"
    );
    let decisions = workspace_decisions();
    assert_eq!(decisions.len(), 1);
    assert_eq!(
        decisions[0].disposition,
        harn_vm::process_sandbox::rustc_wrapper::RustcWrapperDisposition::NotConfigured,
        "replacement launches must not probe any of the four inherited non-null wrappers: {:?}",
        decisions[0]
    );

    use std::os::unix::fs::PermissionsExt;
    let wrapper = workspace.path().join("count-wrapper");
    let count = workspace.path().join("wrapper-count");
    std::fs::write(
        &wrapper,
        format!("#!/bin/sh\nprintf x >> '{}'\nexit 1\n", count.display()),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    let cargo_config = workspace.path().join(".cargo/config.toml");
    std::fs::create_dir(cargo_config.parent().unwrap()).unwrap();
    std::fs::write(
        &cargo_config,
        format!("[build]\nrustc-wrapper = \"{}\"\n", wrapper.display()),
    )
    .unwrap();
    let VmValue::Dict(response) = call(replacement.clone()).unwrap() else {
        panic!("expected configured-wrapper build result");
    };
    assert!(
        matches!(response.get("exit_code"), Some(VmValue::Int(0))),
        "{response:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&count).unwrap_or_default(),
        "x",
        "a new Cargo config must invalidate NotConfigured"
    );
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\n# replacement executable\nprintf x >> '{}'\nexit 1\n",
            count.display()
        ),
    )
    .unwrap();
    let VmValue::Dict(response) = call(replacement.clone()).unwrap() else {
        panic!("expected replacement-wrapper build result");
    };
    assert!(
        matches!(response.get("exit_code"), Some(VmValue::Int(0))),
        "{response:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&count).unwrap(),
        "xx",
        "a replaced wrapper must be measured again"
    );
    std::fs::remove_file(&cargo_config).unwrap();
    std::fs::write(&count, "").unwrap();
    replacement_env.insert("RUSTC_WRAPPER".into(), value(&wrapper.to_string_lossy()));
    replacement.insert("env".into(), VmValue::dict(replacement_env));
    let mut indirect = replacement.clone();
    for response in [call(replacement.clone()), call(replacement)] {
        let VmValue::Dict(response) = response.unwrap() else {
            panic!("expected process result");
        };
        assert!(
            matches!(response.get("exit_code"), Some(VmValue::Int(0))),
            "{response:?}"
        );
    }
    assert_eq!(
        std::fs::read_to_string(&count).unwrap(),
        "x",
        "per-spawn cleanup tokens must not repeat the real compiler probe"
    );

    let decisions_before_shell = workspace_decisions().len();
    indirect.insert(
        "argv".into(),
        VmValue::List(Arc::new(vec![
            value("sh"),
            value("-c"),
            value(
                "export RUSTC_WRAPPER=\"$1\"; exec cargo build --offline --target-dir shell-target",
            ),
            value("shell-wrapper-control"),
            value(&wrapper.to_string_lossy()),
        ])),
    );
    let VmValue::Dict(response) = call(indirect).unwrap() else {
        panic!("expected shell process result");
    };
    assert!(
        matches!(response.get("exit_code"), Some(VmValue::Int(code)) if *code != 0),
        "the explicitly reset failing wrapper must reach nested Cargo: {response:?}"
    );
    assert_eq!(std::fs::read_to_string(&count).unwrap(), "xx");
    assert_eq!(workspace_decisions().len(), decisions_before_shell);

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
