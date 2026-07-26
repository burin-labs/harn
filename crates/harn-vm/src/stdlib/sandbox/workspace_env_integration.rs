use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::orchestration::{
    pop_execution_policy, push_execution_policy, CapabilityPolicy, ProcessSandboxPolicy,
    ProcessSandboxPreset, SandboxProfile,
};
use crate::tool_annotations::SideEffectLevel;

use super::{command_output, ProcessCommandConfig, WORKSPACE_TMPDIR_NAME};

struct PolicyGuard;

impl Drop for PolicyGuard {
    fn drop(&mut self) {
        crate::stdlib::process::set_session_environment(None);
        pop_execution_policy();
    }
}

fn enter_policy(workspace: &Path) -> PolicyGuard {
    push_execution_policy(CapabilityPolicy {
        workspace_roots: vec![workspace.display().to_string()],
        side_effect_level: Some(SideEffectLevel::ProcessExec.as_str().to_string()),
        sandbox_profile: SandboxProfile::Worktree,
        process_sandbox: ProcessSandboxPolicy {
            presets: Some(vec![
                ProcessSandboxPreset::SystemRuntime,
                ProcessSandboxPreset::DeveloperToolchains,
                ProcessSandboxPreset::PackageManagerConfig,
            ]),
            ..ProcessSandboxPolicy::default()
        },
        ..CapabilityPolicy::default()
    });
    crate::stdlib::process::set_session_environment(Some(
        crate::security::SessionEnvironment::isolated(),
    ));
    PolicyGuard
}

fn find_program(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join(name))
            .find(|candidate| candidate.is_file())
    })
}

fn run(program: &Path, args: &[&str], workspace: &Path) -> std::process::Output {
    let args = args
        .iter()
        .map(|arg| (*arg).to_string())
        .collect::<Vec<_>>();
    let output = command_output(
        &program.display().to_string(),
        &args,
        &ProcessCommandConfig {
            cwd: Some(workspace.to_path_buf()),
            ..ProcessCommandConfig::default()
        },
    )
    .unwrap_or_else(|error| panic!("{} {args:?} failed to spawn: {error}", program.display()));
    assert!(
        output.status.success(),
        "{} {args:?} failed:\n{}",
        program.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

#[test]
fn spawned_process_observes_workspace_toolchain_environment() {
    let workspace = tempfile::tempdir().unwrap();
    let _policy = enter_policy(workspace.path());
    let env = find_program("env").expect("env executable");
    let output = run(&env, &[], workspace.path());
    let values = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect::<BTreeMap<_, _>>();

    let root = workspace.path().canonicalize().unwrap();
    let cache = root.join(".harn-toolchain-cache");
    for key in [
        "HOME",
        "XDG_CACHE_HOME",
        "GOCACHE",
        "GOMODCACHE",
        "GOPATH",
        "CARGO_TARGET_DIR",
        "PIP_CACHE_DIR",
        "UV_CACHE_DIR",
        "NPM_CONFIG_CACHE",
        "YARN_CACHE_FOLDER",
        "PNPM_HOME",
        "NPM_CONFIG_STORE_DIR",
        "PYTHONUSERBASE",
    ] {
        let value = PathBuf::from(values.get(key).unwrap_or_else(|| panic!("{key} missing")));
        assert!(value.starts_with(&cache), "{key} escaped cache: {value:?}");
    }
    assert_eq!(
        PathBuf::from(values.get("TMPDIR").unwrap()),
        root.join(WORKSPACE_TMPDIR_NAME)
    );
}

#[test]
fn explicit_session_env_grant_wins_over_workspace_default() {
    use crate::security::{EnvironmentPolicyKind, GrantSourceSpec, GrantSpec, SessionEnvironment};

    let workspace = tempfile::tempdir().unwrap();
    let _policy = enter_policy(workspace.path());
    let profile = SessionEnvironment::launch(
        EnvironmentPolicyKind::Granted,
        vec![GrantSpec {
            name: "explicit-home".to_string(),
            source: GrantSourceSpec::Env {
                var: "EXPLICIT_HOME".to_string(),
            },
            expose_as_env: Some("HOME".to_string()),
        }],
        &|name| (name == "EXPLICIT_HOME").then(|| "/caller/home".to_string()),
    )
    .unwrap();
    crate::stdlib::process::set_session_environment(Some(profile));

    let env = find_program("env").expect("env executable");
    let output = run(&env, &[], workspace.path());
    let home = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .find_map(|line| line.strip_prefix("HOME="))
        .unwrap()
        .to_string();
    assert_eq!(home, "/caller/home");
}

#[test]
fn installed_toolchains_use_the_workspace_owned_state() {
    let workspace = tempfile::tempdir().unwrap();
    let _policy = enter_policy(workspace.path());
    let cache = workspace
        .path()
        .canonicalize()
        .unwrap()
        .join(".harn-toolchain-cache");

    if let Some(go) = find_program("go") {
        std::fs::write(
            workspace.path().join("go.mod"),
            "module example.test/cache\n\ngo 1.22\n",
        )
        .unwrap();
        std::fs::write(
            workspace.path().join("cache_test.go"),
            "package cache\nimport \"testing\"\nfunc TestCache(t *testing.T) {}\n",
        )
        .unwrap();
        let output = run(&go, &["env", "GOCACHE"], workspace.path());
        assert_eq!(
            PathBuf::from(String::from_utf8_lossy(&output.stdout).trim()),
            cache.join("go-build")
        );
        run(&go, &["test", "./..."], workspace.path());
    }

    if let Some(cargo) = find_program("cargo") {
        std::fs::create_dir(workspace.path().join("src")).unwrap();
        std::fs::write(
            workspace.path().join("Cargo.toml"),
            "[package]\nname = \"cache_probe\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(workspace.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        run(&cargo, &["build", "--offline"], workspace.path());
    }

    if let Some(python) = find_program("python3") {
        let output = run(&python, &["-m", "site", "--user-site"], workspace.path());
        let user_site = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
        assert!(user_site.starts_with(cache.join("python-user")));
        let pytest = user_site.join("pytest");
        std::fs::create_dir_all(&pytest).unwrap();
        std::fs::write(pytest.join("__init__.py"), "").unwrap();
        std::fs::write(pytest.join("__main__.py"), "print('workspace pytest')\n").unwrap();
        run(&python, &["-m", "pytest"], workspace.path());
    }

    if let Some(npm) = find_program("npm") {
        std::fs::write(
            workspace.path().join("package.json"),
            r#"{"private":true,"scripts":{"test":"node test.js"}}"#,
        )
        .unwrap();
        std::fs::write(workspace.path().join("test.js"), "").unwrap();
        let output = run(&npm, &["config", "get", "cache"], workspace.path());
        assert_eq!(
            PathBuf::from(String::from_utf8_lossy(&output.stdout).trim()),
            cache.join("npm")
        );
        run(&npm, &["test"], workspace.path());
    }
}

#[test]
fn os_sandbox_still_denies_writes_outside_the_workspace() {
    if !super::active_backend_available() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let outside_home = root.path().join("real-home");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&outside_home).unwrap();
    let _policy = enter_policy(&workspace);
    let touch = find_program("touch").expect("touch executable");
    let denied = outside_home.join("denied");
    let args = vec![denied.display().to_string()];
    let result = command_output(
        &touch.display().to_string(),
        &args,
        &ProcessCommandConfig {
            cwd: Some(workspace),
            ..ProcessCommandConfig::default()
        },
    );
    assert!(result.is_err(), "outside-home write unexpectedly succeeded");
    assert!(!denied.exists());
}
