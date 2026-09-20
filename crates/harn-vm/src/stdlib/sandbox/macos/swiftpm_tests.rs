use super::*;

fn executable(path: &Path, source: &str) {
    std::fs::write(path, source).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

#[test]
fn generated_launchers_execute_the_same_arguments_as_direct_adaptation() {
    let workspace = tempfile::tempdir().unwrap();
    let real = workspace.path().join("selected tool's executable");
    executable(
        &real,
        "#!/bin/bash\nif (( $# )); then printf '%s\\0' \"$@\"; fi\n",
    );
    let cases: &[(&str, &[&str])] = &[
        ("swift", &["test", "--filter", "a b", "--cache-path=.cache"]),
        (
            "swift",
            &["run", "app", "", "--", "--cache-path=application-option"],
        ),
        ("swift", &["--version"]),
        ("swift", &[]),
        (
            "swift",
            &[
                "test",
                "--manifest-cache",
                "none",
                "--disable-sandbox",
                "--security-path=.sec",
            ],
        ),
        (
            "xcrun",
            &[
                "--sdk",
                "macosx",
                "--toolchain",
                "Selected Toolchain",
                "swift",
                "package",
                "dump-package",
            ],
        ),
        ("xcrun", &["--sdk=macosx", "-r", "swift", "test"]),
        ("xcrun", &["--find", "swift"]),
        ("xcrun", &["--sdk", "swift", "clang", "--version"]),
        ("xcrun", &["--future-option", "swift", "test"]),
    ];
    for (name, raw_args) in cases {
        let target = ScopedMutationTarget {
            root: workspace.path().to_path_buf(),
            relative: PathBuf::from(format!("launchers/{name}")),
        };
        write_launcher(&target, launcher_source(name, &real).as_bytes()).unwrap();
        let args = raw_args
            .iter()
            .map(|arg| arg.to_string())
            .collect::<Vec<_>>();
        let output = std::process::Command::new(target.root.join(&target.relative))
            .args(&args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{name} {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).unwrap();
        let actual = stdout
            .split_terminator('\0')
            .map(str::to_string)
            .collect::<Vec<_>>();
        assert_eq!(actual, compatible_args(name, &args), "{name} {args:?}");
    }
}

#[test]
fn path_adaptation_preserves_selected_tools_and_is_idempotent() {
    let workspace = tempfile::tempdir().unwrap();
    let tools = workspace.path().join("selected toolchain");
    std::fs::create_dir(&tools).unwrap();
    executable(
        &tools.join("swift"),
        "#!/bin/sh\nprintf selected-toolchain\n",
    );
    let original = tools.display().to_string();
    let first = adapted_path(workspace.path(), &original).unwrap().unwrap();
    let second = adapted_path(workspace.path(), &first).unwrap().unwrap();
    assert_eq!(first, second);
    assert_eq!(std::env::split_paths(&first).count(), 2);
    let adapter = std::env::split_paths(&first).next().unwrap().join("swift");
    let output = std::process::Command::new(adapter)
        .arg("--version")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"selected-toolchain");
}

#[test]
fn adapter_creation_refuses_a_symlinked_cache_parent() {
    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let marker = outside.path().join("marker");
    std::fs::write(&marker, "outside-stays-unchanged").unwrap();
    std::os::unix::fs::symlink(
        outside.path(),
        workspace.path().join(".harn-toolchain-cache"),
    )
    .unwrap();
    let result = adapted_path(workspace.path(), "/usr/bin:/bin");
    assert!(
        result.is_err(),
        "a workspace symlink must not redirect adapter writes"
    );
    assert_eq!(
        std::fs::read_to_string(marker).unwrap(),
        "outside-stays-unchanged"
    );
    assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 1);
}

#[test]
fn unrestricted_and_explicitly_empty_paths_get_no_adapter() {
    let workspace = tempfile::tempdir().unwrap();
    let mut policy = CapabilityPolicy {
        workspace_roots: vec![workspace.path().display().to_string()],
        sandbox_profile: crate::orchestration::SandboxProfile::Unrestricted,
        ..CapabilityPolicy::default()
    };
    let original = vec![("PATH".to_string(), "/usr/bin:/bin".to_string())];
    let mut env = original.clone();
    inject_env(&mut env, &policy);
    assert_eq!(env, original);
    policy.sandbox_profile = crate::orchestration::SandboxProfile::Worktree;
    let mut empty = vec![("PATH".to_string(), String::new())];
    inject_env(&mut empty, &policy);
    assert_eq!(empty, vec![("PATH".to_string(), String::new())]);
    let fallback = super::super::super::handler_sandbox_test_guard();
    fallback.set("off");
    let mut disabled = original.clone();
    inject_env(&mut disabled, &policy);
    assert_eq!(disabled, original);
    assert_eq!(std::fs::read_dir(workspace.path()).unwrap().count(), 0);
}
