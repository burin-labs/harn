use harn_hostlib::process::{spawn_process, EnvMode, OutputCapture, OwnerDeathPolicy, SpawnSpec};
use std::io::Read;
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
    let _environment = super::process_tools_e2e::declare_inherited();
    let _guardian = harn_hostlib::process::owner_death::install_guardian_reexec_args([
        "--exact",
        "process_tools_e2e::owner_death_guardian_fixture",
        "--nocapture",
    ]);
    let mut child = spawn_process(SpawnSpec {
        builtin: "replaced_executable_fixture",
        program: "/bin/sh".into(),
        args: vec!["-c".into(), "printf 'reexec survived replacement'".into()],
        cwd: None,
        env: Default::default(),
        env_remove: Vec::new(),
        env_mode: EnvMode::Patch,
        use_stdin: false,
        configure_process_group: true,
        owner_death: OwnerDeathPolicy::KillContainment,
        output_capture: OutputCapture::Pipe,
    })
    .expect("a running session must launch contained commands after executable replacement");
    let mut stdout = String::new();
    child
        .take_stdout()
        .unwrap()
        .read_to_string(&mut stdout)
        .unwrap();
    assert_eq!(child.wait().unwrap().code, Some(0));
    // The test executable's libtest guardian prints its own startup banner.
    assert!(
        stdout.ends_with("reexec survived replacement"),
        "{stdout:?}"
    );
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
