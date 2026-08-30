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
    enter_policy_with_environment(workspace, crate::security::SessionEnvironment::isolated())
}

fn enter_policy_with_environment(
    workspace: &Path,
    environment: crate::security::SessionEnvironment,
) -> PolicyGuard {
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
    crate::stdlib::process::set_session_environment(Some(environment));
    PolicyGuard
}

fn isolated_environment(
    entries: impl IntoIterator<Item = (String, String)>,
) -> crate::security::SessionEnvironment {
    crate::security::SessionEnvironment::launch_from_snapshot(
        crate::security::EnvironmentPolicyKind::Isolated,
        Vec::new(),
        entries.into_iter().collect(),
        &|_| None,
    )
    .unwrap()
}

fn find_program(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|path| find_program_in_path(name, &path))
}

fn find_program_in_path(name: &str, path: &std::ffi::OsStr) -> Option<PathBuf> {
    std::env::split_paths(path)
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable_file(candidate))
}

fn is_executable_file(candidate: &Path) -> bool {
    let Ok(metadata) = candidate.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(unix)]
#[test]
fn program_lookup_skips_non_executable_path_entries() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().unwrap();
    let inert_dir = root.path().join("inert");
    let executable_dir = root.path().join("executable");
    std::fs::create_dir_all(&inert_dir).unwrap();
    std::fs::create_dir_all(&executable_dir).unwrap();
    let inert = inert_dir.join("env");
    let executable = executable_dir.join("env");
    std::fs::write(&inert, "not executable").unwrap();
    std::fs::write(&executable, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = std::env::join_paths([inert_dir, executable_dir]).unwrap();

    assert_eq!(find_program_in_path("env", &path), Some(executable));
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

fn output_environment(output: std::process::Output) -> BTreeMap<String, String> {
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

#[cfg(target_os = "linux")]
#[test]
fn absolute_executable_outside_workspace_runs_without_widening_its_parent() {
    let workspace = tempfile::tempdir().unwrap();
    let artifact_dir = tempfile::tempdir().unwrap();
    let artifact = artifact_dir.path().join("verified-harn-artifact");
    std::fs::copy("/bin/true", &artifact).unwrap();

    let _policy = enter_policy(workspace.path());
    let output = run(&artifact, &[], workspace.path());
    assert!(output.status.success());

    let sibling = artifact_dir.path().join("not-granted.txt");
    std::fs::write(&sibling, "outside capability scope").unwrap();
    let denied = command_output(
        "/bin/cat",
        &[sibling.display().to_string()],
        &ProcessCommandConfig {
            cwd: Some(workspace.path().to_path_buf()),
            ..ProcessCommandConfig::default()
        },
    );
    assert!(
        denied.is_err(),
        "granting one executable must not grant reads from its parent directory"
    );
}

#[test]
fn spawned_process_observes_workspace_toolchain_environment() {
    let workspace = tempfile::tempdir().unwrap();
    let _policy = enter_policy(workspace.path());
    let env = find_program("env").expect("env executable");
    let output = run(&env, &[], workspace.path());
    let values = output_environment(output);

    let root = workspace.path().canonicalize().unwrap();
    let cache = root.join(".harn-toolchain-cache");
    for key in [
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
        "CCACHE_DIR",
        "CCACHE_TEMPDIR",
    ] {
        let value = PathBuf::from(values.get(key).unwrap_or_else(|| panic!("{key} missing")));
        assert!(value.starts_with(&cache), "{key} escaped cache: {value:?}");
    }
    // The child must inherit the user's real HOME and user-site. See the
    // matching unit test in `workspace_env.rs` for why.
    for key in ["HOME", "PYTHONUSERBASE"] {
        let value = PathBuf::from(values.get(key).map(String::as_str).unwrap_or_default());
        assert!(
            !value.starts_with(&cache),
            "{key} must NOT be relocated into the toolchain cache: {value:?}"
        );
    }
    assert!(
        !values.contains_key("NPM_CONFIG_STORE_DIR"),
        "npm must not receive pnpm's unsupported store-dir option"
    );
    assert_eq!(
        PathBuf::from(values.get("TMPDIR").unwrap()),
        root.join(WORKSPACE_TMPDIR_NAME)
    );
}

#[cfg(unix)]
#[test]
fn safe_inherited_workspace_cache_reaches_sandboxed_child() {
    let workspace = tempfile::tempdir().unwrap();
    let workspace = workspace.path().canonicalize().unwrap();
    let cache = workspace.join(".outer-harness-cache/go-mod");
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::write(cache.join("prewarmed"), "cache-hit\n").unwrap();

    let environment =
        isolated_environment([("GOMODCACHE".to_string(), cache.display().to_string())]);
    let _policy = enter_policy_with_environment(&workspace, environment);

    let shell = find_program("sh").expect("shell executable");
    let output = run(&shell, &["-c", "cat \"$GOMODCACHE/prewarmed\""], &workspace);
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "cache-hit\n");
}

#[test]
fn external_inherited_cache_is_relocated_inside_workspace() {
    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let workspace = workspace.path().canonicalize().unwrap();
    let outside = outside.path().canonicalize().unwrap();

    let environment =
        isolated_environment([("GOMODCACHE".to_string(), outside.display().to_string())]);
    let _policy = enter_policy_with_environment(&workspace, environment);

    let env = find_program("env").expect("env executable");
    let values = output_environment(run(&env, &[], &workspace));
    let observed = PathBuf::from(values.get("GOMODCACHE").expect("GOMODCACHE missing"));
    assert_eq!(
        observed,
        workspace.join(".harn-toolchain-cache").join("go-mod")
    );
    assert!(!observed.starts_with(outside));
}

#[test]
fn explicit_process_cache_env_wins_over_safe_inherited_cache() {
    let workspace = tempfile::tempdir().unwrap();
    let workspace = workspace.path().canonicalize().unwrap();
    let inherited = workspace.join("inherited-go-mod");
    let explicit = workspace.join("explicit-go-mod");
    std::fs::create_dir_all(&inherited).unwrap();
    std::fs::create_dir_all(&explicit).unwrap();

    let environment =
        isolated_environment([("GOMODCACHE".to_string(), inherited.display().to_string())]);
    let _policy = enter_policy_with_environment(&workspace, environment);

    let env = find_program("env").expect("env executable");
    let output = command_output(
        &env.display().to_string(),
        &[],
        &ProcessCommandConfig {
            cwd: Some(workspace),
            env: vec![("GOMODCACHE".to_string(), explicit.display().to_string())],
            ..ProcessCommandConfig::default()
        },
    )
    .unwrap();
    assert!(output.status.success());
    let values = output_environment(output);
    assert_eq!(
        PathBuf::from(values.get("GOMODCACHE").expect("GOMODCACHE missing")),
        explicit
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
            for_command: None,
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
        // The user site must be the REAL one. This is the assertion that would
        // have caught the py-smoke failure: a relocated user site is a directory
        // the user never installed anything into, so every `pip install --user`
        // package is invisible to the agent while remaining visible in the
        // user's terminal.
        assert!(
            !user_site.starts_with(cache.join("python-user")),
            "python's user site must not be relocated into the toolchain cache: {user_site:?}"
        );
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
