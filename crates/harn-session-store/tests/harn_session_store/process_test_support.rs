use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use wait_timeout::ChildExt;

const CHILD_TIMEOUT: Duration = Duration::from_secs(10);

pub(super) struct ProcessTestChild {
    child: Child,
    stdout: Option<BufReader<std::process::ChildStdout>>,
}

impl ProcessTestChild {
    pub(super) fn spawn(test_name: &str, configure: impl FnOnce(&mut Command)) -> Self {
        let mut command = Command::new(std::env::current_exe().expect("current test executable"));
        command.arg(test_name).arg("--nocapture");
        configure(&mut command);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut child = command.spawn().expect("spawn test child process");
        let stdout = BufReader::new(child.stdout.take().expect("child stdout"));
        Self {
            child,
            stdout: Some(stdout),
        }
    }

    pub(super) fn wait_for(&mut self, marker: &str) {
        let mut reader = self.stdout.take().expect("child stdout");
        let expected_marker = marker.to_string();
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let reader_thread = std::thread::spawn(move || {
            let result = loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) => {
                        break Err(format!("child exited before emitting {expected_marker}"));
                    }
                    Ok(_) if line.split_whitespace().last() == Some(expected_marker.as_str()) => {
                        break Ok(());
                    }
                    Ok(_) => {}
                    Err(error) => break Err(format!("could not read child output: {error}")),
                }
            };
            let _ = sender.send((reader, result));
        });
        match receiver.recv_timeout(CHILD_TIMEOUT) {
            Ok((reader, result)) => {
                self.stdout = Some(reader);
                reader_thread.join().expect("join child output reader");
                result.unwrap_or_else(|error| panic!("{error}"));
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                self.kill_and_reap();
                reader_thread.join().expect("join timed-out output reader");
                panic!("child did not emit {marker} within {CHILD_TIMEOUT:?}");
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                reader_thread.join().expect("join failed output reader");
                panic!("child output reader disconnected before emitting {marker}");
            }
        }
    }

    pub(super) fn send(&mut self, input: &[u8]) {
        self.child
            .stdin
            .as_mut()
            .expect("child stdin")
            .write_all(input)
            .expect("write child stdin");
    }

    pub(super) fn wait_success(&mut self) {
        let Some(status) = self
            .child
            .wait_timeout(CHILD_TIMEOUT)
            .expect("wait for child")
        else {
            self.kill_and_reap();
            panic!("child did not exit within {CHILD_TIMEOUT:?}");
        };
        assert_eq!(status.code(), Some(0), "child status");
    }

    pub(super) fn kill_and_reap(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for ProcessTestChild {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            self.kill_and_reap();
        }
    }
}
