//! The one place `harn-serve` spawns a thread that drives the Harn VM.
//!
//! Every transport in this crate — A2A and MCP over HTTP, ACP over WebSocket,
//! the embedded ACP channel, and the in-process ACP client — runs the VM on a
//! dedicated thread with its own current-thread Tokio runtime. Each of those
//! threads needs [`harn_vm::RUNTIME_STACK_SIZE`], the same stack the CLI gives
//! the VM, because a stack overflow aborts the process rather than failing one
//! request.
//!
//! Routing every spawn through this module keeps that a property of one
//! function instead of a rule each new transport has to remember.

use std::io;
use std::thread::{Builder, JoinHandle};

/// Spawns `run` on a named thread sized to run the Harn VM.
pub(crate) fn spawn<F>(name: impl Into<String>, run: F) -> io::Result<JoinHandle<()>>
where
    F: FnOnce() + Send + 'static,
{
    Builder::new()
        .name(name.into())
        .stack_size(harn_vm::RUNTIME_STACK_SIZE)
        .spawn(run)
}

/// Spawns `run` on a VM-sized thread, panicking if the thread cannot start.
///
/// For call sites that have no way to report the failure to a caller. Failing
/// to spawn leaves the transport unable to serve anything, so surfacing it
/// immediately beats handing back a handle that silently answers nothing.
pub(crate) fn spawn_or_panic<F>(name: &'static str, run: F) -> JoinHandle<()>
where
    F: FnOnce() + Send + 'static,
{
    spawn(name, run).unwrap_or_else(|error| panic!("spawn {name} VM thread: {error}"))
}

#[cfg(test)]
mod tests {
    /// Uses `depth * 16 KiB` of stack, defeating optimization so the frames are
    /// really allocated.
    #[inline(never)]
    fn burn_stack(depth: usize) -> u8 {
        let mut frame = [0u8; 16 * 1024];
        frame[depth % frame.len()] = depth as u8;
        let frame = std::hint::black_box(frame);
        if depth == 0 {
            return frame[0];
        }
        frame[0].wrapping_add(burn_stack(depth - 1))
    }

    /// Set on the re-exec'd child so it runs the probe instead of forking again.
    const PROBE_CHILD: &str = "HARN_SERVE_VM_THREAD_STACK_PROBE";

    /// The contract this module exists for: 4 MiB is past Rust's 2 MiB default
    /// thread stack and well under `RUNTIME_STACK_SIZE`, so the probe survives
    /// only if the helper applied the larger size.
    ///
    /// It runs in a re-exec'd child with `RUST_MIN_STACK` cleared, because
    /// every Rust test lane in this repo exports `RUST_MIN_STACK=16777216` and
    /// that alone makes an unsized `thread::spawn` big enough. Asserting in
    /// this process would pass with or without the fix and guard nothing —
    /// which is exactly how the missing stack size stayed invisible: the serve
    /// transports were tested under an environment variable no shipped binary
    /// sets.
    #[test]
    fn spawned_threads_get_more_than_the_default_stack() {
        if std::env::var_os(PROBE_CHILD).is_some() {
            let handle = super::spawn("vm-thread-stack-probe", || {
                std::hint::black_box(burn_stack(4 * 1024 * 1024 / (16 * 1024)));
            })
            .expect("spawn probe thread");
            handle.join().expect("probe thread completed");
            return;
        }

        let status =
            std::process::Command::new(std::env::current_exe().expect("current test executable"))
                .args([
                    "--exact",
                    "vm_thread::tests::spawned_threads_get_more_than_the_default_stack",
                    "--test-threads=1",
                ])
                .env(PROBE_CHILD, "1")
                .env_remove("RUST_MIN_STACK")
                .status()
                .expect("re-exec the probe without RUST_MIN_STACK");

        assert!(
            status.success(),
            "a thread from `vm_thread::spawn` overflowed on 4 MiB with \
             RUST_MIN_STACK unset ({status}), so it inherited the 2 MiB default \
             instead of harn_vm::RUNTIME_STACK_SIZE — that is what every shipped \
             `harn serve` transport would run the VM on"
        );
    }

    /// A guard against a new transport hand-rolling `thread::Builder` and
    /// inheriting the 2 MiB default: every VM-driving thread in this crate is
    /// expected to come from `vm_thread`. The scan is deliberately narrow — it
    /// only flags a spawn that builds a Tokio runtime on the new thread, which
    /// is what "drives the VM" looks like here.
    #[test]
    fn vm_driving_threads_are_spawned_through_this_module() {
        let crate_src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders = Vec::new();
        let mut scanned = 0usize;
        let mut stack = vec![crate_src.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("read harn-serve src") {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().is_none_or(|ext| ext != "rs") {
                    continue;
                }
                if path == crate_src.join("vm_thread.rs") {
                    continue;
                }
                scanned += 1;
                let source = std::fs::read_to_string(&path).expect("read source");
                // A thread that builds a Tokio runtime on itself is how "drives
                // the VM" looks in this crate. Threads that do something else —
                // the rustls fixture in the A2A tests, for one — are fine on the
                // default stack and are deliberately not flagged.
                for pattern in ["std::thread::spawn(", "thread::Builder::new()"] {
                    for (offset, _) in source.match_indices(pattern) {
                        let window_end = (offset + 600).min(source.len());
                        let window = match source.get(offset..window_end) {
                            Some(window) => window,
                            // A multi-byte boundary; the next match still covers
                            // this file, and the Tokio marker is ASCII.
                            None => continue,
                        };
                        if window.contains("tokio::runtime::Builder") {
                            offenders.push(format!(
                                "{}: `{pattern}` builds a Tokio runtime on the new thread",
                                path.display()
                            ));
                        }
                    }
                }
            }
        }
        assert!(scanned > 10, "scan found only {scanned} sources to check");
        assert!(
            offenders.is_empty(),
            "spawn VM-driving threads through `crate::vm_thread` so they get \
             harn_vm::RUNTIME_STACK_SIZE; found:\n  {}",
            offenders.join("\n  ")
        );
    }
}
