use std::fs;
use std::path::Path;
use std::process::{ExitStatus, Output, Stdio};
use std::time::Duration;

use futures::StreamExt;
use harn_vm::event_log::{EventLog, EventLogBackendKind, EventLogConfig, LogEvent, Topic};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, BufReader, Lines};
use tokio::process::{Child, ChildStderr, Command};

use crate::test_util::process::harn_e2e_binary;

pub const SHUTDOWN_NEEDLE: &str = "graceful shutdown complete";
const STARTUP_NEEDLE: &str = "HTTP listener ready on";
const PROCESS_READY_TIMEOUT: Duration = Duration::from_mins(1);
const PROCESS_EXIT_TIMEOUT: Duration = Duration::from_secs(30);

pub fn write_file(dir: &Path, relative: &str, contents: &str) {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

pub struct OrchestratorProcess {
    child: Child,
    stderr_lines: Lines<BufReader<ChildStderr>>,
    stderr: String,
}

impl OrchestratorProcess {
    pub fn spawn(temp: &TempDir, envs: &[(&str, &str)]) -> Self {
        let mut command = Command::new(harn_e2e_binary());
        command
            .current_dir(temp.path())
            .args([
                "orchestrator",
                "serve",
                "--config",
                "harn.toml",
                "--state-dir",
                "./state",
                "--role",
                "single-tenant",
                "--bind",
                "127.0.0.1:0",
            ])
            .stderr(Stdio::piped())
            .stdout(Stdio::null())
            .kill_on_drop(true);
        for (key, value) in envs {
            command.env(key, value);
        }
        let mut child = command.spawn().expect("spawn orchestrator");
        let stderr = child.stderr.take().expect("orchestrator stderr pipe");
        Self {
            child,
            stderr_lines: BufReader::new(stderr).lines(),
            stderr: String::new(),
        }
    }

    pub async fn wait_for_listener_url(&mut self) -> String {
        let result = tokio::time::timeout(PROCESS_READY_TIMEOUT, async {
            while let Some(line) = self
                .stderr_lines
                .next_line()
                .await
                .expect("orchestrator stderr line")
            {
                self.stderr.push_str(&line);
                self.stderr.push('\n');
                if let Some((_, suffix)) = line.split_once(STARTUP_NEEDLE) {
                    return suffix.trim().to_string();
                }
            }
            panic!(
                "orchestrator stderr closed before log `{STARTUP_NEEDLE}`\nstderr={}",
                self.stderr
            );
        })
        .await;
        result.unwrap_or_else(|_| {
            panic!(
                "timed out waiting for orchestrator log `{STARTUP_NEEDLE}`\nstderr={}",
                self.stderr
            )
        })
    }

    pub async fn wait_for_exit_code(mut self, expected: i32) -> String {
        let status = self.wait_for_exit().await;
        self.collect_stderr().await;
        assert_eq!(
            status.code(),
            Some(expected),
            "unexpected orchestrator exit status: {status}\nstderr={}",
            self.stderr
        );
        self.stderr.clone()
    }

    pub async fn terminate_gracefully(mut self) -> String {
        let pid = i32::try_from(self.child.id().expect("live orchestrator pid"))
            .expect("orchestrator pid fits i32");
        // SAFETY: `pid` belongs to the live child owned by this value and
        // SIGTERM has no memory-safety preconditions.
        let result = unsafe { libc::kill(pid, libc::SIGTERM) };
        assert_eq!(
            result,
            0,
            "failed to send SIGTERM to orchestrator: {}",
            std::io::Error::last_os_error()
        );
        let status = self.wait_for_exit().await;
        self.collect_stderr().await;
        assert!(
            status.success(),
            "orchestrator exited unsuccessfully: {status}\nstderr={}",
            self.stderr
        );
        self.stderr.clone()
    }

    async fn wait_for_exit(&mut self) -> ExitStatus {
        tokio::time::timeout(PROCESS_EXIT_TIMEOUT, self.child.wait())
            .await
            .expect("timed out waiting for orchestrator exit")
            .expect("wait for orchestrator exit")
    }

    async fn collect_stderr(&mut self) {
        while let Some(line) = self
            .stderr_lines
            .next_line()
            .await
            .expect("orchestrator stderr line")
        {
            self.stderr.push_str(&line);
            self.stderr.push('\n');
        }
    }
}

impl Drop for OrchestratorProcess {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

pub async fn run_orchestrator_command(
    temp: &TempDir,
    subcommand: &str,
    args: &[&str],
    envs: &[(&str, &str)],
) -> Output {
    let mut command = Command::new(harn_e2e_binary());
    command
        .current_dir(temp.path())
        .arg("orchestrator")
        .arg(subcommand)
        .args(["--config", "harn.toml", "--state-dir", "./state"])
        .args(args);
    for (key, value) in envs {
        command.env(key, value);
    }
    command.output().await.expect("run harn command")
}

pub fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

pub fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

pub async fn wait_for_topic_event(
    state_dir: &Path,
    topic_name: &str,
    predicate: impl Fn(&LogEvent) -> bool,
) -> LogEvent {
    let mut config = EventLogConfig::for_base_dir(state_dir).unwrap();
    let file_dir = state_dir.join("events");
    if file_dir.join("topics").is_dir() {
        config.backend = EventLogBackendKind::File;
        config.file_dir = file_dir;
    }
    let log = harn_vm::event_log::open_event_log(&config).unwrap();
    let topic = Topic::new(topic_name).unwrap();
    let existing = log.read_range(&topic, None, usize::MAX).await.unwrap();
    if let Some((_, event)) = existing.iter().find(|(_, event)| predicate(event)) {
        return event.clone();
    }
    let after = existing.last().map(|(sequence, _)| *sequence);
    let mut events = log.subscribe(&topic, after).await.unwrap();
    tokio::time::timeout(PROCESS_EXIT_TIMEOUT, async {
        loop {
            let (_, event) = events
                .next()
                .await
                .expect("event stream closed")
                .expect("event stream read");
            if predicate(&event) {
                return event;
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for matching {topic_name} event"))
}
