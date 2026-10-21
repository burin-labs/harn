use super::*;

#[test]
fn system_runtime_reads_resolved_resources_without_opening_private_directories() {
    assert!(
        Path::new(SANDBOX_EXEC_PATH).exists(),
        "a real Seatbelt probe is required"
    );
    let resolver = std::fs::read("/etc/resolv.conf").expect("host resolver configuration");
    assert!(
        !resolver.is_empty(),
        "the positive control must read something"
    );
    assert!(Path::new("/usr/share/zoneinfo/UTC").is_file());

    // Host-readable and outside every granted root. UserTemp is deliberately
    // absent, so an accidental /private or /private/var grant cannot hide here.
    let outside = tempfile::tempdir_in("/private/var/tmp").expect("private host fixture");
    let outside_file = outside.path().join("outside-runtime.txt");
    std::fs::write(&outside_file, "outside-runtime-root").expect("seed negative control");
    assert_eq!(
        std::fs::read_to_string(&outside_file).unwrap(),
        "outside-runtime-root"
    );

    let mut policy = super::tests::macos_policy_with_workspace_ops(&["read_text"]);
    policy.workspace_roots.clear();
    policy.process_sandbox.presets = Some(vec![ProcessSandboxPreset::SystemRuntime]);
    let profile = render_profile(&policy);
    let run = |args: &[&str]| {
        Command::new(SANDBOX_EXEC_PATH)
            .args(["-p", &profile, "--"])
            .args(args)
            .output()
            .expect("run a command under the real profile")
    };

    let read = run(&["/bin/cat", "/etc/resolv.conf"]);
    assert!(
        read.status.success(),
        "{}",
        String::from_utf8_lossy(&read.stderr)
    );
    assert_eq!(read.stdout, resolver);
    let zones = run(&["/bin/ls", "-1", "/usr/share/zoneinfo"]);
    assert!(
        zones.status.success(),
        "{}",
        String::from_utf8_lossy(&zones.stderr)
    );
    assert!(String::from_utf8_lossy(&zones.stdout)
        .lines()
        .any(|line| line == "UTC"));

    let denied = run(&["/bin/cat", outside_file.to_str().unwrap()]);
    assert!(
        !denied.status.success(),
        "an unrelated private file became readable"
    );
    assert!(
        denied.stdout.is_empty(),
        "the denied read returned private data"
    );
}
