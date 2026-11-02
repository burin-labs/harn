use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::orchestration::{
    pop_execution_policy, push_execution_policy, CapabilityPolicy, ProcessSandboxPolicy,
    ProcessSandboxPreset, SandboxProfile,
};
use crate::tool_annotations::SideEffectLevel;

use super::{command_output, ProcessCommandConfig, WORKSPACE_TMPDIR_NAME};

struct PolicyGuard;

#[cfg(target_os = "macos")]
#[test]
fn swiftpm_default_backend_executes_xctest_with_private_foundation_temp() {
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(
        workspace.path().join("Package.swift"),
        "// swift-tools-version: 6.0\nimport PackageDescription\nlet package = Package(name: \"PrivateTemp\", targets: [.testTarget(name: \"PrivateTempTests\")])\n",
    ).unwrap();
    let tests = workspace.path().join("Tests/PrivateTempTests");
    std::fs::create_dir_all(&tests).unwrap();
    std::fs::write(tests.join("PrivateTempTests.swift"), r#"
import Foundation
import XCTest

final class PrivateTempTests: XCTestCase {
    func testNativeTemporaryStorage() throws {
        let expected = URL(fileURLWithPath: try XCTUnwrap(ProcessInfo.processInfo.environment["TMPDIR"]))
            .resolvingSymlinksInPath().standardizedFileURL
        let actual = FileManager.default.temporaryDirectory.resolvingSymlinksInPath().standardizedFileURL
        XCTAssertEqual(actual, expected)
        let target = actual.appendingPathComponent("foundation-write")
        try "PRIVATE-FOUNDATION-TEMP".write(to: target, atomically: true, encoding: .utf8)
        XCTAssertEqual(try String(contentsOf: target, encoding: .utf8), "PRIVATE-FOUNDATION-TEMP")
        print("PRIVATE-FOUNDATION-TEMP")
    }
}
"#).unwrap();
    // Reach a descendant launched from a project script as well as the default
    // SwiftPM backend. No --build-system option or shared-temp grant is used.
    std::fs::write(
        workspace.path().join("test.sh"),
        "#!/bin/sh\nset -eu\nexec swift test --jobs 2 --filter PrivateTempTests\n",
    )
    .unwrap();
    let _policy = enter_policy(workspace.path());
    let result = command_output(
        "/bin/sh",
        &["test.sh".to_string()],
        &ProcessCommandConfig {
            cwd: Some(workspace.path().to_path_buf()),
            ..Default::default()
        },
    );
    if crate::sandbox_host::assert_unavailable_result(&result) {
        assert!(!workspace.path().join(".harn-tmp/foundation-write").exists());
        return;
    }
    let output = result.expect("confined project script");
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("PRIVATE-FOUNDATION-TEMP")
            && stdout.contains("Executed 1 test, with 0 failures"),
        "Swift test was not executed: {stdout}"
    );
    assert_eq!(
        std::fs::read_to_string(workspace.path().join(".harn-tmp/foundation-write")).unwrap(),
        "PRIVATE-FOUNDATION-TEMP"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn swiftpm_from_a_project_script_uses_the_outer_sandbox() {
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(
        workspace.path().join("Package.swift"),
        "// swift-tools-version: 6.0\nimport PackageDescription\nlet package = Package(name: \"ScriptSandboxProbe\")\n",
    )
    .unwrap();
    let _policy = enter_policy(workspace.path());
    for invocation in [
        "swift package dump-package",
        "xcrun --sdk macosx swift package dump-package",
    ] {
        std::fs::write(
            workspace.path().join("manifest.sh"),
            format!("#!/bin/sh\nset -eu\nexec {invocation}\n"),
        )
        .unwrap();
        let result = command_output(
            "/bin/sh",
            &["manifest.sh".to_string()],
            &ProcessCommandConfig {
                cwd: Some(workspace.path().to_path_buf()),
                ..ProcessCommandConfig::default()
            },
        );
        if crate::sandbox_host::assert_unavailable_result(&result) {
            continue;
        }
        let output =
            result.unwrap_or_else(|error| panic!("project script {invocation}: {error:?}"));
        assert!(
            output.status.success(),
            "{invocation}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let manifest: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(manifest["name"], "ScriptSandboxProbe", "{invocation}");
        assert!(!String::from_utf8_lossy(&output.stderr).contains("sandbox_apply"));
    }
}

#[cfg(target_os = "macos")]
#[test]
fn swiftpm_adapted_environment_keeps_write_and_read_denials() {
    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let outside_file = outside.path().join("outside.txt");
    std::fs::write(&outside_file, "outside-unchanged").unwrap();
    let denied_file = workspace.path().join("denied.txt");
    std::fs::write(&denied_file, "known-denied-fixture").unwrap();
    let _guard = enter_policy(workspace.path());
    let config = ProcessCommandConfig {
        cwd: Some(workspace.path().to_path_buf()),
        ..ProcessCommandConfig::default()
    };
    let env = super::active_workspace_process_env();
    let path = env
        .iter()
        .find(|(key, _)| key == "PATH")
        .expect("adapted PATH");
    assert!(std::env::split_paths(&path.1)
        .next()
        .unwrap()
        .join("swift")
        .is_file());
    let write = |target: &Path| {
        command_output(
            "/bin/sh",
            &[
                "-c".to_string(),
                "printf known-write > \"$1\"".to_string(),
                "write-probe".to_string(),
                target.display().to_string(),
            ],
            &config,
        )
    };
    let inside_file = workspace.path().join("inside.txt");
    let inside = write(&inside_file);
    if crate::sandbox_host::assert_unavailable_result(&inside) {
        assert!(!inside_file.exists());
        assert_eq!(
            std::fs::read_to_string(&outside_file).unwrap(),
            "outside-unchanged"
        );
        return;
    }
    assert!(inside.unwrap().status.success());
    assert_eq!(
        std::fs::read_to_string(&inside_file).unwrap(),
        "known-write"
    );
    assert!(matches!(
        write(&outside_file),
        Err(crate::VmError::CategorizedError {
            category: crate::value::ErrorCategory::ToolRejected,
            ..
        })
    ));
    assert_eq!(
        std::fs::read_to_string(&outside_file).unwrap(),
        "outside-unchanged"
    );

    let mut policy = crate::orchestration::current_execution_policy().unwrap();
    policy.process_sandbox.read_deny_roots = vec![denied_file.display().to_string()];
    // Replace the one policy slot owned by the guard, retaining the same
    // isolated environment and generated launchers throughout the read matrix.
    pop_execution_policy();
    push_execution_policy(policy.clone());
    let read = |target: &Path| command_output("/bin/cat", &[target.display().to_string()], &config);
    assert_eq!(read(&inside_file).unwrap().stdout, b"known-write");
    assert!(matches!(
        read(&denied_file),
        Err(crate::VmError::CategorizedError {
            category: crate::value::ErrorCategory::ToolRejected,
            ..
        })
    ));
    policy.process_sandbox.read_deny_roots.clear();
    pop_execution_policy();
    push_execution_policy(policy);
    assert_eq!(read(&denied_file).unwrap().stdout, b"known-denied-fixture");
}

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
        process_sandbox: Box::new(ProcessSandboxPolicy {
            presets: Some(vec![
                ProcessSandboxPreset::SystemRuntime,
                ProcessSandboxPreset::DeveloperToolchains,
                ProcessSandboxPreset::PackageManagerConfig,
            ]),
            ..ProcessSandboxPolicy::default()
        }),
        ..CapabilityPolicy::default()
    });
    crate::stdlib::process::set_session_environment(Some(environment));
    PolicyGuard
}

#[test]
fn an_in_process_env_read_writes_nothing_and_a_spawn_materializes() {
    let workspace = tempfile::tempdir().unwrap();
    let _guard = enter_policy(workspace.path());
    let read = |name: &str| crate::stdlib::process::session_env_var(name).unwrap();

    // Each provider's credential lookup reads the environment; building the
    // child environment for every read rewrote toolchain launchers per step.
    let tmpdir = read("TMPDIR").expect("the workspace temp dir is a default");
    let root = workspace.path().canonicalize().unwrap();
    assert_eq!(Path::new(&tmpdir), root.join(WORKSPACE_TMPDIR_NAME));
    assert!(read("GOCACHE").is_some());
    let created: Vec<_> = std::fs::read_dir(workspace.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert!(
        created.is_empty(),
        "an environment read created {created:?}"
    );

    let spawn = super::active_workspace_process_env();
    assert!(Path::new(&tmpdir).is_dir(), "a spawn creates the temp dir");
    let spawn_tmpdir = spawn.iter().find(|(key, _)| key == "TMPDIR").unwrap();
    assert_eq!(
        spawn_tmpdir.1, tmpdir,
        "a read and a spawn agree on the value"
    );
}

#[test]
fn an_in_process_env_read_follows_a_session_environment_change() {
    let workspace = tempfile::tempdir().unwrap();
    let root = workspace.path().canonicalize().unwrap();
    let cache = |name: &str| root.join(name).display().to_string();
    let _guard = enter_policy_with_environment(
        workspace.path(),
        isolated_environment([("GOCACHE".to_string(), cache("first-go-cache"))]),
    );
    let read = || crate::stdlib::process::session_env_var("GOCACHE").unwrap();
    assert_eq!(read(), Some(cache("first-go-cache")));

    crate::stdlib::process::set_session_environment(Some(isolated_environment([(
        "GOCACHE".to_string(),
        cache("second-go-cache"),
    )])));
    assert_eq!(read(), Some(cache("second-go-cache")));
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

#[cfg(unix)]
#[test]
fn a_confined_child_keeps_the_installed_pnpm_home_read_only() {
    let workspace = tempfile::tempdir().unwrap();
    let installed = tempfile::tempdir().unwrap();
    let marker = installed.path().join("installed-manager");
    std::fs::write(&marker, "INSTALLED-PNPM").unwrap();
    let home = installed.path().display().to_string();
    let _guard = enter_policy_with_environment(
        workspace.path(),
        isolated_environment([("PNPM_HOME".to_string(), home.clone())]),
    );
    let mut policy = crate::orchestration::current_execution_policy().unwrap();
    pop_execution_policy();
    policy.process_sandbox.read_roots.push(home.clone());
    push_execution_policy(policy);
    assert_eq!(
        crate::stdlib::process::session_env_var("PNPM_HOME").unwrap(),
        Some(home)
    );
    let run = |source: &str| {
        command_output(
            "/bin/sh",
            &["-c".to_string(), source.to_string()],
            &ProcessCommandConfig {
                cwd: Some(workspace.path().to_path_buf()),
                ..Default::default()
            },
        )
    };
    let read = run("cat \"$PNPM_HOME/installed-manager\"");
    if crate::sandbox_host::assert_unavailable_result(&read) {
        return;
    }
    let read = read.unwrap();
    assert!(read.status.success(), "{read:?}");
    assert_eq!(read.stdout, b"INSTALLED-PNPM");
    let write = run("printf poisoned > \"$PNPM_HOME/installed-manager\"");
    assert!(
        matches!(
            &write,
            Err(crate::VmError::CategorizedError {
                category: crate::value::ErrorCategory::ToolRejected,
                ..
            })
        ),
        "{write:?}"
    );
    assert_eq!(std::fs::read_to_string(marker).unwrap(), "INSTALLED-PNPM");
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

fn run(program: &Path, args: &[&str], workspace: &Path) -> Option<std::process::Output> {
    let args = args
        .iter()
        .map(|arg| (*arg).to_string())
        .collect::<Vec<_>>();
    let result = command_output(
        &program.display().to_string(),
        &args,
        &ProcessCommandConfig {
            cwd: Some(workspace.to_path_buf()),
            ..ProcessCommandConfig::default()
        },
    );
    if crate::sandbox_host::assert_unavailable_result(&result) {
        return None;
    }
    let output = result
        .unwrap_or_else(|error| panic!("{} {args:?} failed to spawn: {error}", program.display()));
    assert!(
        output.status.success(),
        "{} {args:?} failed:\n{}",
        program.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    Some(output)
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
    let Some(output) = run(&artifact, &[], workspace.path()) else {
        return;
    };
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
    let Some(output) = run(&env, &[], workspace.path()) else {
        return;
    };
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
        "CCACHE_DIR",
        "CCACHE_TEMPDIR",
    ] {
        let value = PathBuf::from(values.get(key).unwrap_or_else(|| panic!("{key} missing")));
        assert!(value.starts_with(&cache), "{key} escaped cache: {value:?}");
    }
    // The child must inherit the user's real HOME and user-site. See the
    // matching unit test in `workspace_env.rs` for why.
    for key in ["HOME", "PYTHONUSERBASE", "PNPM_HOME"] {
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

    for key in ["GOMODCACHE", "XDG_CACHE_HOME"] {
        let environment = isolated_environment([(key.to_string(), cache.display().to_string())]);
        let _policy = enter_policy_with_environment(&workspace, environment);

        let shell = find_program("sh").expect("shell executable");
        let command = format!("cat \"${key}/prewarmed\"");
        let Some(output) = run(&shell, &["-c", &command], &workspace) else {
            return;
        };
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            "cache-hit\n",
            "{key}"
        );
    }
}

#[test]
fn external_inherited_cache_is_relocated_inside_workspace() {
    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let workspace = workspace.path().canonicalize().unwrap();
    let outside = outside.path().canonicalize().unwrap();

    for (key, suffix) in [("GOMODCACHE", "go-mod"), ("XDG_CACHE_HOME", "xdg-cache")] {
        let environment = isolated_environment([(key.to_string(), outside.display().to_string())]);
        let _policy = enter_policy_with_environment(&workspace, environment);

        let env = find_program("env").expect("env executable");
        let Some(output) = run(&env, &[], &workspace) else {
            return;
        };
        let values = output_environment(output);
        let observed = PathBuf::from(values.get(key).unwrap_or_else(|| panic!("{key} missing")));
        assert_eq!(
            observed,
            workspace.join(".harn-toolchain-cache").join(suffix)
        );
        assert!(!observed.starts_with(&outside));
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn gradle_cache_child_preserves_workspace_paths_or_reports_unavailable_confinement() {
    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let workspace = workspace.path().canonicalize().unwrap();
    let outside = outside.path().canonicalize().unwrap();
    let warm = workspace.join("warm-gradle");
    std::fs::create_dir(&warm).unwrap();
    std::fs::write(warm.join("prewarmed"), "gradle-cache-hit\n").unwrap();
    let link = workspace.join("external-cache-link");
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    for inherited in [&warm, &outside, &link] {
        let environment = isolated_environment([
            ("PATH".to_string(), "/usr/bin:/bin".to_string()),
            (
                "GRADLE_USER_HOME".to_string(),
                inherited.display().to_string(),
            ),
            (
                "GRADLE_TOKEN".to_string(),
                "synthetic-must-not-reach-child".to_string(),
            ),
        ]);
        let _policy = enter_policy_with_environment(&workspace, environment);
        let expected = if inherited == &warm {
            warm.clone()
        } else {
            workspace.join(".harn-toolchain-cache/gradle")
        };
        let mode = if inherited == &warm {
            "warm"
        } else {
            "relocated"
        };
        let args = ["-c", "test -z \"${GRADLE_TOKEN+x}\" || exit 61; test \"$GRADLE_USER_HOME\" = \"$1\" || exit 62; if test \"$2\" = warm; then cat \"$GRADLE_USER_HOME/prewarmed\"; else mkdir -p \"$GRADLE_USER_HOME\" && printf private-cache > \"$GRADLE_USER_HOME/child-write\" && printf 'cache-relocated\\n'; fi", "cache-probe", expected.to_str().unwrap(), mode].map(str::to_string);
        let result = command_output(
            "/bin/sh",
            &args,
            &ProcessCommandConfig {
                cwd: Some(workspace.clone()),
                ..Default::default()
            },
        );
        if crate::sandbox_host::assert_unavailable_result(&result) {
            crate::sandbox_host::boundary_not_measured(
                "gradle_cache_child",
                "typed host confinement refusal observed; cache path was not measured",
            );
            return;
        }
        let output = result.expect("confined Gradle cache child");
        assert!(
            output.status.success(),
            "launcher cache assertion for {mode}: status={}, stderr={}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            if mode == "warm" {
                "gradle-cache-hit\n"
            } else {
                "cache-relocated\n"
            }
        );
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);
    }
    println!("confined Gradle cache warm-read, external and symlink relocation, and secret-negative controls reached");
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
    let result = command_output(
        &env.display().to_string(),
        &[],
        &ProcessCommandConfig {
            cwd: Some(workspace),
            env: vec![("GOMODCACHE".to_string(), explicit.display().to_string())],
            ..ProcessCommandConfig::default()
        },
    );
    if crate::sandbox_host::assert_unavailable_result(&result) {
        return;
    }
    let output = result.unwrap();
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
            expose_to: Default::default(),
        }],
        &|name| (name == "EXPLICIT_HOME").then(|| "/caller/home".to_string()),
    )
    .unwrap();
    crate::stdlib::process::set_session_environment(Some(profile));

    let env = find_program("env").expect("env executable");
    let Some(output) = run(&env, &[], workspace.path()) else {
        return;
    };
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
    // Measure a real command even when none of the optional toolchains exist.
    let env = find_program("env").expect("env executable");
    if run(&env, &[], workspace.path()).is_none() {
        return;
    }
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
        let output = run(&go, &["env", "GOCACHE"], workspace.path()).expect("qualified host");
        assert_eq!(
            PathBuf::from(String::from_utf8_lossy(&output.stdout).trim()),
            cache.join("go-build")
        );
        run(&go, &["test", "./..."], workspace.path()).expect("qualified host");
    }

    if let Some(cargo) = find_program("cargo") {
        std::fs::create_dir(workspace.path().join("src")).unwrap();
        std::fs::write(
            workspace.path().join("Cargo.toml"),
            "[package]\nname = \"cache_probe\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(workspace.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        run(&cargo, &["build", "--offline"], workspace.path()).expect("qualified host");
    }

    if let Some(python) = find_program("python3") {
        let output =
            run(&python, &["-m", "site", "--user-site"], workspace.path()).expect("qualified host");
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
        let output =
            run(&npm, &["config", "get", "cache"], workspace.path()).expect("qualified host");
        assert_eq!(
            PathBuf::from(String::from_utf8_lossy(&output.stdout).trim()),
            cache.join("npm")
        );
        run(&npm, &["test"], workspace.path()).expect("qualified host");
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

#[cfg(unix)]
#[test]
fn a_confined_child_runs_the_installed_pinned_pnpm_without_the_registry() {
    use std::os::unix::fs::PermissionsExt;
    let workspace = tempfile::tempdir().unwrap();
    let installed = tempfile::tempdir().unwrap();
    let launcher = |dir: &Path, marker: &str| {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join("pnpm");
        std::fs::write(&path, format!("#!/bin/sh\necho {marker}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    };
    let outer = installed.path().join("outer");
    launcher(&outer, "OUTER-PNPM");
    launcher(
        &installed.path().join(".tools/pnpm/9.9.9/bin"),
        "PINNED-PNPM",
    );
    let home = installed.path().display().to_string();
    let _guard = enter_policy_with_environment(
        workspace.path(),
        isolated_environment([
            ("PNPM_HOME".to_string(), home.clone()),
            (
                "PATH".to_string(),
                format!("{}:/usr/bin:/bin", outer.display()),
            ),
        ]),
    );
    let mut policy = crate::orchestration::current_execution_policy().unwrap();
    pop_execution_policy();
    policy.process_sandbox.read_roots.push(home);
    push_execution_policy(policy);
    let pnpm_for = |pin: &str| {
        std::fs::write(
            workspace.path().join("package.json"),
            format!("{{\"packageManager\": \"pnpm@{pin}+sha512.abc\"}}"),
        )
        .unwrap();
        command_output(
            "/bin/sh",
            &["-c".to_string(), "pnpm".to_string()],
            &ProcessCommandConfig {
                cwd: Some(workspace.path().to_path_buf()),
                ..Default::default()
            },
        )
    };
    let pinned = pnpm_for("9.9.9");
    if crate::sandbox_host::assert_unavailable_result(&pinned) {
        return;
    }
    assert_eq!(pinned.unwrap().stdout, b"PINNED-PNPM\n");
    // A pin that is not installed leaves the search path alone.
    assert_eq!(pnpm_for("9.9.8").unwrap().stdout, b"OUTER-PNPM\n");
}
