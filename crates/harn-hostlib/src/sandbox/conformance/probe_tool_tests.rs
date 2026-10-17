use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;

use super::*;

fn path_of(dirs: &[&Path]) -> OsString {
    std::env::join_paths(dirs).expect("joinable PATH")
}

/// A missing probe tool is named so the case reads as not measured, never as
/// an over-refusal; the same probe with the tool present proceeds.
#[test]
fn a_missing_probe_tool_is_named_and_a_present_one_proceeds() {
    let empty = tempfile::tempdir().unwrap();
    let tools = tempfile::tempdir().unwrap();
    let perl = tools.path().join("perl");
    std::fs::write(&perl, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&perl, std::fs::Permissions::from_mode(0o755)).unwrap();
    let argv = vec!["perl".to_string(), "-e".to_string(), "1".to_string()];

    let missing = path_of(&[empty.path()]);
    assert_eq!(
        missing_probe_tool(&argv, Some(missing.as_os_str())),
        Some("perl".to_string())
    );
    assert_eq!(missing_probe_tool(&argv, None), Some("perl".to_string()));

    // Negative control: with the tool on PATH the case is measured as before.
    let present = path_of(&[empty.path(), tools.path()]);
    assert_eq!(missing_probe_tool(&argv, Some(present.as_os_str())), None);

    let absolute = vec![perl.display().to_string()];
    assert_eq!(missing_probe_tool(&absolute, None), None);
    let gone = vec![empty.path().join("perl").display().to_string()];
    assert_eq!(missing_probe_tool(&gone, None), gone.first().cloned());
    assert_eq!(missing_probe_tool(&[], None), None);
}

/// Every Unix probe that needs a helper tool asks this check, so the cases
/// that would spawn perl or cargo name the tool when it is absent.
#[test]
fn every_probe_program_is_checked_against_path() {
    let layout = Layout::new().expect("layout");
    let empty = tempfile::tempdir().unwrap();
    let nothing = path_of(&[empty.path()]);
    for case in [
        ConformanceCase::NetworkConnectRefused,
        ConformanceCase::UnixSocketBindUnderRoot,
        ConformanceCase::RustcWrapperThatRunsIsKept,
    ] {
        let (argv, _) = probe(case, &layout);
        assert_eq!(
            missing_probe_tool(&argv, Some(nothing.as_os_str())),
            argv.first().cloned(),
            "{case:?}"
        );
    }
}
