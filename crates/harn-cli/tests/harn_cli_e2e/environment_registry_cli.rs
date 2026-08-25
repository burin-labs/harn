use crate::test_util::process::run_harn_e2e;

#[test]
fn cli_rejects_unknown_harn_environment_name_before_dispatch() {
    let output = run_harn_e2e(&["--version"], &[("HARN_LLM_TIMOUT", "30")]);

    assert_eq!(output.exit_code, 2);
    assert!(output.stdout.is_empty());
    assert!(output.stderr.contains("HARN-ENV-001"), "{}", output.stderr);
    assert!(
        output.stderr.contains("HARN_LLM_TIMOUT"),
        "{}",
        output.stderr
    );
    assert!(
        output.stderr.contains("HARN_LLM_TIMEOUT"),
        "{}",
        output.stderr
    );
    assert!(
        output
            .stderr
            .contains("Use `HARN_EXT_<NAME>` for settings owned by a calling tool"),
        "{}",
        output.stderr
    );
    assert!(!output.stderr.contains("=30"), "{}", output.stderr);
}

#[test]
fn cli_accepts_caller_owned_environment_name_in_extension_namespace() {
    let output = run_harn_e2e(&["--version"], &[("HARN_EXT_RELEASE_REPO", "/tmp/repo")]);

    assert_eq!(output.exit_code, 0, "{}", output.stderr);
    assert!(output.stdout.starts_with("harn "), "{}", output.stdout);
    assert!(output.stderr.is_empty(), "{}", output.stderr);
}

#[test]
fn cli_rejects_invalid_registered_value_without_rendering_it() {
    let invalid_value = "not-a-duration";
    let output = run_harn_e2e(&["--version"], &[("HARN_LLM_TIMEOUT", invalid_value)]);

    assert_eq!(output.exit_code, 2);
    assert!(output.stderr.contains("HARN-ENV-002"), "{}", output.stderr);
    assert!(
        output.stderr.contains("HARN_LLM_TIMEOUT"),
        "{}",
        output.stderr
    );
    assert!(!output.stderr.contains(invalid_value), "{}", output.stderr);
}
