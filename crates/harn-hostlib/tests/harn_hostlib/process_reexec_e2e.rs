use super::support::{require_dict, require_int, require_str};
use harn_vm::VmDictExt;
use harn_vm::VmValue;
use std::process::Command;

const FIXTURE: &str = "process_reexec_e2e::replaced_executable_fixture";
const REPLACE_ENV: &str = "HARN_TEST_REPLACE_EXECUTABLE";

#[test]
fn replaced_executable_fixture() {
    if std::env::var_os(REPLACE_ENV).is_none() {
        return;
    }
    let executable = std::env::current_exe().unwrap();
    std::fs::remove_file(&executable).unwrap();
    assert!(!executable.exists());
    assert!(std::env::current_exe()
        .unwrap()
        .to_string_lossy()
        .ends_with(" (deleted)"));
    let mut request = harn_vm::value::DictMap::new();
    request.put_str("mode", "argv");
    request.insert(
        harn_vm::value::intern_key("argv"),
        VmValue::list(vec![
            VmValue::string("/bin/sh"),
            VmValue::string("-c"),
            VmValue::string("printf 'reexec survived replacement'"),
        ]),
    );
    let response = super::process_tools_e2e::call("hostlib_tools_run_command", request)
        .expect("a running session must still launch commands after its executable is replaced");
    let data = require_dict(response);
    assert_eq!(require_int(&data, "exit_code"), 0);
    assert_eq!(require_str(&data, "stdout"), "reexec survived replacement");
}

#[test]
fn contained_commands_survive_executable_replacement() {
    let dir = tempfile::tempdir().unwrap();
    let executable = dir.path().join("replaced-host");
    std::fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
    let output = Command::new(&executable)
        .args(["--exact", FIXTURE, "--nocapture"])
        .env(REPLACE_ENV, "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !executable.exists(),
        "the fixture must exercise the unlinked-image case"
    );
}
