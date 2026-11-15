use std::collections::BTreeMap;
use std::io::Read;
use std::os::unix::fs::FileExt;
use std::sync::Arc;

use crate::process::{EnvMode, OutputCapture, OwnerDeathPolicy, SpawnSpec, WaitOutcome};
use crate::tools::proc::stage_output::{StageCapture, StageOutput, StageOutputBytes};

#[test]
fn payload_written_entry_cannot_confirm_failed_stage_relay() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("forge.py"),
        r"import os, stat
found = []
for fd in range(3, 10):
    try:
        if stat.S_ISREG(os.fstat(fd).st_mode) and os.pread(fd, 1, 0) == b'1':
            os.pwrite(fd, b'2', 0)
            found.append(fd)
    except OSError:
        pass
assert len(found) == 1, found
with open('payload-entry-written', 'w') as marker:
    marker.write('reached')
print('stage')
",
    )
    .unwrap();
    let private_sink = tempfile::NamedTempFile::new().unwrap();
    let stdout = Arc::new(std::fs::File::open(private_sink.path()).unwrap());
    let stderr = Arc::new(tempfile::tempfile().unwrap());
    let entered = Arc::new(tempfile::tempfile().unwrap());
    let args = vec![
        "-c".into(),
        "printf setup && /usr/bin/python3 forge.py".into(),
    ];
    let harn_vm::shells::OutputStageInvocation::FinalPosixStage(plan) =
        harn_vm::shells::plan_invocation_output_stage("/bin/sh", &args)
    else {
        panic!("canonical literal stage");
    };
    let mut process = crate::process::default_spawner()
        .spawn(SpawnSpec {
            builtin: "stage_receipt_control",
            program: "/bin/sh".into(),
            args,
            cwd: Some(root.path().to_path_buf()),
            env: BTreeMap::new(),
            env_remove: Vec::new(),
            env_mode: EnvMode::Replace,
            use_stdin: false,
            configure_process_group: true,
            owner_death: OwnerDeathPolicy::KillContainment,
            output_capture: OutputCapture::PipeWithStage {
                stdout,
                stderr,
                entered: entered.clone(),
                plan,
            },
        })
        .unwrap();
    let status = process
        .wait_with_timeout(Some(harn_clock::test_support::HANG_CEILING), &|| false)
        .unwrap();
    assert!(matches!(status, WaitOutcome::Exited(status) if status.code == Some(0)));
    assert_eq!(
        std::fs::read_to_string(root.path().join("payload-entry-written")).unwrap(),
        "reached"
    );
    let mut byte = [0];
    entered.read_exact_at(&mut byte, 0).unwrap();
    assert_eq!(
        byte, *b"2",
        "the payload actually forged the old completion byte"
    );
    let mut aggregate = Vec::new();
    process
        .take_stdout()
        .unwrap()
        .read_to_end(&mut aggregate)
        .unwrap();
    assert_eq!(aggregate, b"setupstage\n");
    assert_eq!(
        std::fs::metadata(private_sink.path()).unwrap().len(),
        0,
        "the read-only stage sink received no bytes"
    );
    assert!(
        !process.stage_output_complete(),
        "the guardian must report the actual relay failure"
    );
    let capture = StageCapture::Shell {
        argv: vec!["/usr/bin/python3".into(), "forge.py".into()],
        readers: None,
        entered: entered.clone(),
    };
    let observed = capture
        .finish(
            "forged-stage-receipt",
            StageOutputBytes {
                relay_complete: process.stage_output_complete(),
                stdout: &aggregate,
                stderr: &[],
                stage_stdout: &[],
                stage_stderr: &[],
            },
            true,
            super::proc::CaptureConfig::default(),
            &harn_vm::session_inputs::OutputFilter::capture_for_spawn(),
        )
        .unwrap();
    assert!(
        matches!(observed, StageOutput::Unmeasured { .. }),
        "payload-writable entry bytes cannot prove a failed relay completed: {observed:?}"
    );
    entered.write_all_at(b"1", 0).unwrap();
    let observed = capture
        .finish(
            "failed-stage-valid-entry",
            StageOutputBytes {
                relay_complete: process.stage_output_complete(),
                stdout: &aggregate,
                stderr: &[],
                stage_stdout: &[],
                stage_stderr: &[],
            },
            true,
            super::proc::CaptureConfig::default(),
            &harn_vm::session_inputs::OutputFilter::capture_for_spawn(),
        )
        .unwrap();
    assert!(
        matches!(observed, StageOutput::Unmeasured { .. }),
        "a valid start byte cannot replace the failed relay receipt"
    );
}
