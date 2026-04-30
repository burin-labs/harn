#[path = "process.rs"]
mod process;

use std::io::{BufRead, BufReader, Read};
use std::process::Child;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

// MCP support is loaded via `#[path = "support/mcp.rs"] mod mcp_support;`
// so its parent namespace is the test binary's crate root. Test
// binaries that include `mcp_support` also declare `mod test_util;`, so
// reach the timing constants through that sibling rather than loading
// `timing.rs` a second time (clippy::duplicate_mod). Keeping the
// `super::` path inside the support module makes the dependency
// explicit at compile time.
use super::test_util::timing;

pub use process::HarnProcessTestNoLock;

#[allow(dead_code)]
pub fn lock_mcp_process_tests() -> HarnProcessTestNoLock {
    // The cross-process lock that this used to acquire was retired in favor
    // of tempdir + ephemeral-port isolation; see `support::process` for the
    // full rationale. Returning the unit sentinel keeps existing call sites
    // compiling and ergonomically correct (`let _lock = ...;`).
    process::lock_harn_process_tests()
}

pub fn spawn_stderr_reader(
    stderr: impl Read + Send + 'static,
) -> (Receiver<String>, JoinHandle<String>) {
    let (tx, rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        let mut collected = String::new();
        for line in BufReader::new(stderr).lines() {
            let line = line.expect("stderr line");
            collected.push_str(&line);
            collected.push('\n');
            let _ = tx.send(line);
        }
        collected
    });
    (rx, handle)
}

pub fn wait_for_child_log_suffix(
    child: &mut Child,
    rx: &Receiver<String>,
    needle: &str,
    timeout: Duration,
    label: &str,
) -> String {
    let deadline = Instant::now() + timeout;
    let mut observed = Vec::new();
    while Instant::now() < deadline {
        match rx.recv_timeout(timing::LOG_RECV_POLL_INTERVAL) {
            Ok(line) if line.contains(needle) => {
                return line
                    .split(needle)
                    .nth(1)
                    .unwrap_or_default()
                    .trim()
                    .to_string();
            }
            Ok(line) => observed.push(line),
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!(
                    "{label} stderr stream closed before readiness log `{needle}` appeared\nstderr={}",
                    observed.join("\n")
                );
            }
        }
    }

    let _ = child.kill();
    let _ = child.wait();
    panic!(
        "timed out waiting for {label} readiness log `{needle}`\nstderr={}",
        observed.join("\n")
    );
}
