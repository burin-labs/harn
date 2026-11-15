use std::collections::BTreeMap;
use std::io::{self, Read};
use std::sync::{mpsc, Arc};

use crate::process::{EnvMode, OutputCapture, OwnerDeathPolicy, SpawnSpec, WaitOutcome};
use crate::tools::proc::stage_output::{StageCapture, StageOutput, StageOutputBytes};

type Reader = (
    mpsc::Receiver<io::Result<Vec<u8>>>,
    std::thread::JoinHandle<()>,
);

fn start_read(mut reader: Box<dyn Read + Send>) -> Reader {
    let (sender, receiver) = mpsc::sync_channel(1);
    let thread = harn_parser::runtime_stack::spawn(move || {
        let mut bytes = Vec::new();
        let result = reader.read_to_end(&mut bytes).map(|_| bytes);
        let _ = sender.send(result);
    });
    (receiver, thread)
}

fn finish_read((receiver, thread): Reader) -> Vec<u8> {
    let bytes =
        harn_clock::test_support::recv_within("owned capture reader EOF", &receiver).unwrap();
    thread.join().unwrap();
    bytes
}

#[test]
fn private_clause_observation_cannot_replace_failed_output_relay() {
    for failed in [false, true] {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("forge.py"),
            r"import fcntl, os, socket, stat
if os.environ['FORGE_OBSERVATION'] == '1':
    left, right = socket.socketpair()
    controls = {left.fileno(), right.fileno()}
    seen, attempted = set(), []
    for fd in range(3, 10):
        try:
            flags = fcntl.fcntl(fd, fcntl.F_GETFL)
            if stat.S_ISSOCK(os.fstat(fd).st_mode) and flags & os.O_ACCMODE != os.O_RDONLY:
                if fd in controls:
                    seen.add(fd)
                else:
                    os.write(fd, b'999\n')
                    attempted.append(fd)
        except OSError:
            pass
    assert len(seen) == 2 and len(attempted) == 2, (seen, attempted)
with open('payload-reached', 'w') as marker:
    marker.write('reached')
print('stage')
",
        )
        .unwrap();
        let private_sink = tempfile::NamedTempFile::new().unwrap();
        let mut spec = SpawnSpec {
            builtin: "stage_receipt_control",
            program: "/bin/sh".into(),
            args: vec![
                "-c".into(),
                "printf setup && /usr/bin/python3 forge.py".into(),
            ],
            cwd: Some(root.path().to_path_buf()),
            env: BTreeMap::from([(
                "FORGE_OBSERVATION".into(),
                if failed { "0" } else { "1" }.into(),
            )]),
            env_remove: Vec::new(),
            env_mode: EnvMode::Replace,
            use_stdin: false,
            configure_process_group: true,
            owner_death: OwnerDeathPolicy::KillContainment,
            output_capture: OutputCapture::Pipe,
        };
        let mut capture = StageCapture::for_spawn(
            &mut spec,
            &super::proc::CaptureConfig {
                final_stage: true,
                ..Default::default()
            },
        )
        .unwrap()
        .unwrap();
        if failed {
            let OutputCapture::PipeWithStage { stdout, .. } = &mut spec.output_capture else {
                panic!("actual private stage transport");
            };
            *stdout = Arc::new(std::fs::File::open(private_sink.path()).unwrap());
        }
        let readers = capture.take_readers().unwrap();
        let stage_stdout = start_read(readers.stdout);
        let stage_stderr = start_read(readers.stderr);
        let observation = start_read(readers.observation);
        let mut process = crate::process::default_spawner().spawn(spec).unwrap();
        let stdout = start_read(process.take_stdout().unwrap());
        let stderr = start_read(process.take_stderr().unwrap());
        let status = process
            .wait_with_timeout(Some(harn_clock::test_support::HANG_CEILING), &|| false)
            .unwrap();
        assert!(matches!(status, WaitOutcome::Exited(status) if status.code == Some(0)));
        assert_eq!(
            std::fs::read_to_string(root.path().join("payload-reached")).unwrap(),
            "reached"
        );
        let stdout = finish_read(stdout);
        let stderr = finish_read(stderr);
        let stage_stdout = finish_read(stage_stdout);
        let stage_stderr = finish_read(stage_stderr);
        let observation = finish_read(observation);
        assert_eq!(
            observation, b"1\n2\n",
            "payload writes cannot reach the clause channel"
        );
        assert_eq!(process.stage_output_complete(), !failed);
        let observed = capture
            .finish(
                StageOutputBytes {
                    relay_complete: process.stage_output_complete(),
                    stdout: &stdout,
                    stderr: &stderr,
                    stage_stdout: &stage_stdout,
                    stage_stderr: &stage_stderr,
                    observation: &observation,
                },
                Some(0),
                &super::proc::CaptureConfig::default(),
                &harn_vm::session_inputs::OutputFilter::capture_for_spawn(),
            )
            .unwrap();
        if failed {
            assert_eq!(std::fs::metadata(private_sink.path()).unwrap().len(), 0);
            assert!(matches!(observed, StageOutput::Unmeasured { .. }));
        } else {
            assert!(matches!(observed, StageOutput::Captured { .. }));
        }
    }
}
