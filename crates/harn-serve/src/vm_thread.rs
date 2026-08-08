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

    // A new transport that hand-rolls `thread::Builder` instead of coming
    // through this module is caught by
    // `harn_vm::runtime_stack::tests::vm_driving_threads_ask_for_the_runtime_stack`,
    // which scans the whole workspace. That check lives with the constant it
    // enforces because the same defect shipped in three crates at once, and a
    // per-crate scan can only ever see one of them.
}
