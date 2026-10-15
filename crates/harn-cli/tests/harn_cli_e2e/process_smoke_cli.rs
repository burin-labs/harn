//! The release process fixture must reach commands on supported Macs and a
//! measured typed refusal on hosts that cannot protect their launch environment.

#[test]
pub(super) fn process_smoke_matches_the_hosts_measured_confinement() {
    let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/smoke/process.harn")
        .canonicalize()
        .expect("release process fixture");
    let output = crate::test_util::process::run_harn_e2e(&["run", fixture.to_str().unwrap()], &[]);
    assert_eq!(output.exit_code, 0, "{}\n{}", output.stdout, output.stderr);
    let expected = if harn_vm::process_sandbox::active_backend_filesystem_available() {
        "process ok on darwin"
    } else {
        "process confinement refusal verified on unsupported macOS host"
    };
    assert!(output.stdout.contains(expected), "{}", output.stdout);
}
