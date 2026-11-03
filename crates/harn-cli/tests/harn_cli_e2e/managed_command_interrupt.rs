#![cfg(any(target_os = "linux", target_os = "macos"))]

use std::io::{BufRead, BufReader};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixListener;
use std::process::Stdio;

use crate::test_util::process::{harn_e2e_command, ChildGuard};

const CHILD: &str = r#"
import json, os, signal, socket, subprocess, sys
child_source = "import signal; signal.signal(signal.SIGTERM, signal.SIG_IGN); print('ready', flush=True); signal.pause()" if sys.argv[2] == 'immune' else "import signal; print('ready', flush=True); signal.pause()"
child = subprocess.Popen([sys.executable, '-c', child_source], stdout=subprocess.PIPE)
assert child.stdout.readline() == b'ready\n'
def retire(signum, frame):
    child.terminate()
    child.wait()
    sys.exit(143)
signal.signal(signal.SIGTERM, signal.SIG_IGN if sys.argv[2] == 'immune' else retire)
channel = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
channel.connect(sys.argv[1])
channel.sendall((json.dumps({'reader': os.getpid(), 'child': child.pid, 'token': os.environ['HARN_PROCESS_CLEANUP_TOKEN']}) + '\n').encode())
signal.pause()
"#;

struct StartedProcesses(Vec<i32>);

impl Drop for StartedProcesses {
    fn drop(&mut self) {
        for pid in &self.0 {
            unsafe { libc::kill(*pid, libc::SIGKILL) };
        }
    }
}

fn interrupted_command(signal: i32, transport: &str, immune: bool, parallel: bool, json: bool) {
    let root = tempfile::tempdir().expect("command interruption fixture");
    let socket = root.path().join("ready.sock");
    let listener = UnixListener::bind(&socket).expect("bind readiness channel");
    let fixture = root.path().join("child.py");
    std::fs::write(&fixture, CHILD).expect("write child fixture");
    let script = root.path().join("interrupt.harn");
    let start = if parallel { "parallel 2 { _ ->" } else { "" };
    let end = if parallel { "}" } else { "" };
    std::fs::write(
        &script,
        format!(
            "import {{ command_run }} from \"std/command\"\n\
             fn main(harness: Harness) {{\n\
               {start}\n\
               const result = command_run(harness.tools, {{argv: [\"python3\", argv[0], argv[1], argv[2]], capture: {{transport: \"{transport}\"}}}}, {{timeout_ms: 30000}})\n\
               harness.stdio.println(json_stringify(result))\n\
               {end}\n\
             }}\n"
        ),
    )
    .expect("write command fixture");
    let mut runner = ChildGuard(
        harn_e2e_command()
            .args(["run", "--standalone", "--no-sandbox"])
            .args(if json { vec!["--json"] } else { vec![] })
            .arg(&script)
            .arg("--")
            .arg(&fixture)
            .arg(&socket)
            .arg(if immune { "immune" } else { "graceful" })
            .current_dir(root.path())
            .env("HARN_SECRET_PROVIDERS", "env")
            .env("HARN_LLM_CALLS_DISABLED", "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn actual Harn CLI"),
    );
    let mut cleanup = StartedProcesses(Vec::new());
    let mut tokens = std::collections::BTreeSet::new();
    for _ in 0..if parallel { 2 } else { 1 } {
        let mut event = libc::pollfd {
            fd: listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        assert_eq!(
            unsafe { libc::poll(&raw mut event, 1, 10_000) },
            1,
            "the actual reader and child must reach the readiness channel"
        );
        let (channel, _) = listener.accept().expect("accept child readiness");
        let mut ready = String::new();
        BufReader::new(channel)
            .read_line(&mut ready)
            .expect("read started process identities");
        let ready: serde_json::Value =
            serde_json::from_str(&ready).expect("typed readiness record");
        let token = ready["token"].as_str().unwrap();
        assert!(token.starts_with("harn-cleanup-"));
        assert!(
            tokens.insert(token.to_string()),
            "each call must own a distinct cleanup identity"
        );
        let pids = ["reader", "child"].map(|name| {
            let pid = i32::try_from(ready[name].as_u64().expect("positive started PID"))
                .expect("native PID fits i32");
            assert!(pid > 1);
            assert_eq!(
                harn_hostlib::process_liveness::process_liveness(pid as u32),
                harn_hostlib::process_liveness::ProcessLiveness::Alive,
                "{name} must actually be running before interruption"
            );
            pid
        });
        cleanup.0.extend(pids);
    }
    assert_eq!(unsafe { libc::kill(runner.0.id() as i32, signal) }, 0);
    if json {
        let stdout = runner.0.stdout.take().expect("JSON event stream");
        let mut terminal = 0;
        for line in BufReader::new(stdout).lines() {
            let event: serde_json::Value = serde_json::from_str(&line.expect("JSON event"))
                .expect("well-formed JSON envelope");
            let data = &event["data"];
            let kind = data["event_type"].as_str().expect("typed event kind");
            if matches!(kind, "result" | "error") {
                terminal += 1;
                if kind == "result" {
                    assert_eq!(data["exit_code"], 124, "JSON must retain interruption");
                }
                for pid in &cleanup.0 {
                    assert_eq!(
                        harn_hostlib::process_liveness::process_liveness(*pid as u32),
                        harn_hostlib::process_liveness::ProcessLiveness::Dead,
                        "JSON reported terminal interruption while process {pid} could still execute"
                    );
                }
            }
        }
        assert_eq!(terminal, 1, "exactly one terminal event must be measured");
    }
    let status = runner.0.wait().expect("wait for interrupted run");
    assert_eq!(
        status.code(),
        Some(124),
        "cancellation must remain terminal"
    );
    for pid in &cleanup.0 {
        assert_eq!(
            harn_hostlib::process_liveness::process_liveness(*pid as u32),
            harn_hostlib::process_liveness::ProcessLiveness::Dead,
            "run reported terminal interruption while process {pid} could still execute"
        );
    }
    cleanup.0.clear();
}

#[test]
fn sigterm_retires_the_started_reader_and_child_before_exit() {
    interrupted_command(libc::SIGTERM, "pipe", false, false, false);
}

#[test]
fn sigint_escalates_term_immune_reader_and_child_before_exit() {
    interrupted_command(libc::SIGINT, "pipe", true, false, false);
}

#[test]
fn file_capture_retires_the_started_reader_and_child_before_exit() {
    interrupted_command(libc::SIGTERM, "file", false, false, false);
}

#[test]
fn vm_stop_retires_both_parallel_calls_and_their_started_children() {
    interrupted_command(libc::SIGTERM, "pipe", true, true, false);
}

#[test]
fn json_terminal_retains_interruption_after_file_capture_cleanup() {
    interrupted_command(libc::SIGTERM, "file", false, false, true);
}

#[test]
fn json_terminal_follows_retirement_of_both_parallel_command_trees() {
    interrupted_command(libc::SIGTERM, "pipe", true, true, true);
}

#[test]
fn normal_command_completion_preserves_success_and_explicit_exit() {
    let root = tempfile::tempdir().expect("normal command fixture");
    let script = root.path().join("normal.harn");
    for code in [0, 42] {
        std::fs::write(&script, format!(
            "import {{ command_run }} from \"std/command\"\n\
             fn main(harness: Harness) {{\n\
               const result = command_run(harness.tools, {{argv: [\"pwd\"]}})\n\
               require result.success && result.stdout.trim() != \"\", \"real command must complete\"\n\
               return {code}\n\
             }}\n"
        )).expect("write normal command fixture");
        let output = harn_e2e_command()
            .args(["run", "--standalone", "--no-sandbox", "--json"])
            .arg(&script)
            .current_dir(root.path())
            .env("HARN_SECRET_PROVIDERS", "env")
            .env("HARN_LLM_CALLS_DISABLED", "1")
            .output()
            .expect("run actual normal command");
        assert_eq!(
            output.status.code(),
            Some(code),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let events = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        let terminal = events
            .iter()
            .filter(|event| {
                matches!(
                    event["data"]["event_type"].as_str(),
                    Some("result" | "error")
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(terminal.len(), 1);
        assert_eq!(terminal[0]["data"]["event_type"], "result");
        assert_eq!(terminal[0]["data"]["exit_code"], code);
    }
}
