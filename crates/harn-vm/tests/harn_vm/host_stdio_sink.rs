//! The host-provided stdio sink, measured on the descriptor.
//!
//! The claim is that with a sink installed, a runtime diagnostic reaches the
//! sink and nothing reaches file descriptor 2. In-process assertions cannot
//! establish the second half: the test harness intercepts the print macros
//! above the descriptor, so a captured string proves only that the macro was
//! reached, and a descriptor that was written anyway would still read clean.
//!
//! So the measurement happens in a child process whose descriptor 2 is a pipe
//! this process holds. Whatever reaches that descriptor, from any layer and
//! any thread, arrives here as bytes. The control runs the same child with no
//! sink installed and requires the diagnostic to appear, because a zero read
//! through a path that never carries anything is not evidence.

use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use harn_vm::host_stdio::{
    clear_host_stdio_sink, host_stdio_sink_installed, install_host_stdio_sink, HostStdioSink,
    HostStdioStream,
};

/// Distinctive enough that its presence in the child's descriptor output
/// cannot be some other part of the runtime or the test harness talking.
const DIAGNOSTIC: &str = "harn-host-stdio-sink-diagnostic-8622";
const MODE_VARIABLE: &str = "HARN_HOST_STDIO_SINK_MODE";
const SINK_REPORT_PREFIX: &str = "sink-received:";

#[derive(Default)]
struct RecordingSink {
    writes: Mutex<Vec<(HostStdioStream, String)>>,
}

impl RecordingSink {
    fn text_for(&self, stream: HostStdioStream) -> String {
        self.writes
            .lock()
            .expect("recording sink poisoned")
            .iter()
            .filter(|(seen, _)| *seen == stream)
            .map(|(_, text)| text.as_str())
            .collect()
    }
}

impl HostStdioSink for RecordingSink {
    fn write(&self, stream: HostStdioStream, text: &str) {
        self.writes
            .lock()
            .expect("recording sink poisoned")
            .push((stream, text.to_string()));
    }
}

fn diagnostic_source() -> String {
    format!("fn main(harness: Harness) {{ harness.stdio.eprintln(\"{DIAGNOSTIC}\") }}")
}

struct ChildOutput {
    stdout: String,
    stderr: String,
}

/// The mode this process was launched in, when it is a child.
///
/// A child is this same test executable running this same test: re-entering
/// one test under an environment variable keeps the measurement in the default
/// suite, where an `#[ignore]`d helper would not run at all.
fn child_mode() -> Option<String> {
    std::env::var(MODE_VARIABLE).ok()
}

/// Run the child in one mode and collect what reached each descriptor.
fn run_child(mode: &str) -> ChildOutput {
    let executable = std::env::current_exe().expect("test executable path");
    let output = Command::new(executable)
        .args([
            "--exact",
            "host_stdio_sink::host_sink_takes_the_diagnostic_and_descriptor_two_stays_silent",
            "--nocapture",
            "--test-threads",
            "1",
        ])
        .env(MODE_VARIABLE, mode)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("child test process");

    let child = ChildOutput {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    };
    assert!(
        output.status.success(),
        "child in mode {mode} failed: {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        child.stdout,
        child.stderr
    );
    child
}

/// The falsifier. A sink is installed, a runtime diagnostic is produced, and
/// descriptor 2 in that process must carry none of it.
#[test]
fn host_sink_takes_the_diagnostic_and_descriptor_two_stays_silent() {
    if let Some(mode) = child_mode() {
        run_as_child(&mode);
        return;
    }

    let child = run_child("sink");

    assert!(
        child
            .stdout
            .contains(&format!("{SINK_REPORT_PREFIX}{DIAGNOSTIC}")),
        "the sink did not receive the diagnostic; child stdout:\n{}",
        child.stdout
    );
    assert!(
        !child.stderr.contains(DIAGNOSTIC),
        "bytes reached descriptor 2 with a sink installed:\n{}",
        child.stderr
    );
}

/// The control. The same child, the same diagnostic, no sink: the bytes must
/// appear on descriptor 2, which is what proves the measurement above reads a
/// real zero rather than reading nothing at all.
#[test]
fn without_a_sink_the_diagnostic_reaches_descriptor_two() {
    if child_mode().is_some() {
        return;
    }

    let child = run_child("descriptor");

    assert!(
        child.stderr.contains(DIAGNOSTIC),
        "the control never wrote to descriptor 2, so the falsifier measures nothing; stderr:\n{}",
        child.stderr
    );
    assert!(
        !child.stdout.contains(SINK_REPORT_PREFIX),
        "no sink was installed yet one reported a write; child stdout:\n{}",
        child.stdout
    );
}

/// Installing and clearing are observable, and clearing gives the descriptor
/// back rather than leaving the process permanently diverted.
#[test]
fn clearing_the_sink_returns_the_descriptor() {
    if child_mode().is_some() {
        return;
    }

    let sink = Arc::new(RecordingSink::default());
    assert!(!host_stdio_sink_installed());

    let replaced = install_host_stdio_sink(sink);
    assert!(replaced.is_none());
    assert!(host_stdio_sink_installed());

    let cleared = clear_host_stdio_sink();
    assert!(cleared.is_some());
    assert!(!host_stdio_sink_installed());
}

/// The child's half: produce the diagnostic and report what the sink saw.
fn run_as_child(mode: &str) {
    let sink = Arc::new(RecordingSink::default());
    if mode == "sink" {
        install_host_stdio_sink(sink.clone());
    }

    crate::support::run(&diagnostic_source()).expect("diagnostic program");

    // Written straight to the descriptor rather than through `print!`, which
    // the harness intercepts, so the parent sees it whatever the harness does
    // with captured output.
    let received = sink.text_for(HostStdioStream::Stderr);
    if !received.is_empty() {
        let mut stdout = std::io::stdout().lock();
        let _ = write!(stdout, "{SINK_REPORT_PREFIX}{}", received.trim_end());
        let _ = stdout.flush();
    }
    clear_host_stdio_sink();
}
