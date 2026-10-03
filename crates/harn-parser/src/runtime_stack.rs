//! The native stack contract for every thread that can run Harn code.
//!
//! Harn has two independent stack hazards, each with its own owner:
//!
//! * Walking an arbitrarily deep *value* — `x = [x]` in a loop — is made
//!   stack-size independent by `harn_vm::value::recursion`, which grows the
//!   native stack on demand and tears values down iteratively.
//! * Walking an arbitrarily deep *program* — parse, type-check, compile, and
//!   evaluate all recurse over nested syntax — is not. It relies on the thread
//!   simply having enough stack, and that is what [`RUNTIME_STACK_SIZE`] is.
//!
//! This module owns the second contract because the parser is the lowest crate
//! that recurses over a program: the module loader, the VM, and every host
//! depend on it. A thread in any crate that can reach the parser or the VM is
//! created here, through [`spawn`], [`builder`], or [`scope`]. Rust's 2 MiB
//! default is never the right answer for such a thread, and a stack overflow
//! aborts the whole process rather than failing one request.
//!
//! `RUST_MIN_STACK` does not substitute. Every CI test lane exports it, which
//! makes an unsized thread large enough in CI and nowhere else, so a host that
//! relies on it passes its own tests and aborts the first time a customer runs
//! a deep enough script. `harn_vm`'s `runtime_stack` tests scan the workspace
//! and refuse a thread created any other way.

use std::io;
use std::thread::{Builder, JoinHandle, Scope, ScopedJoinHandle};

/// Native stack a thread needs to parse any source the parser accepts.
///
/// The parser recurses once per nesting level, and an unoptimized build spends
/// about 140 KiB of stack on each level of nested expressions. Rust's 2 MiB
/// default thread stack is exhausted after about a dozen levels, long before
/// [`crate::MAX_NESTING_DEPTH`] refuses the source.
///
/// The size follows the refusal, not the typical program: the parser must be
/// able to reach the nesting limit and report it.
pub const PARSE_STACK_SIZE: usize = 16 * 1024 * 1024;

/// Native stack size a thread needs in order to run Harn code.
///
/// Parsing, compilation, and execution walk nested program structure with
/// recursive frames. The size is set by the deepest descent the runtime
/// promises to *refuse* rather than the deepest it expects to run. A nested
/// agent descent costs roughly 2 MiB of native stack per level, so 16 MiB could
/// carry only seven levels while the nested-execution budget declares eight:
/// the refusal was undeliverable, and the process aborted on the level that
/// should have been denied. A bound the stack cannot reach is not a bound.
pub const RUNTIME_STACK_SIZE: usize = 32 * 1024 * 1024;

// Every thread created here runs the parser too.
const _: () = assert!(RUNTIME_STACK_SIZE >= PARSE_STACK_SIZE);

/// A thread builder that already holds [`RUNTIME_STACK_SIZE`].
///
/// Use it to name a thread or to handle a spawn failure. Do not call
/// `stack_size` on it: the size is this module's decision.
pub fn builder() -> Builder {
    Builder::new().stack_size(RUNTIME_STACK_SIZE)
}

/// [`std::thread::spawn`] with [`RUNTIME_STACK_SIZE`].
///
/// Panics if the OS cannot create the thread, as `std::thread::spawn` does.
pub fn spawn<F, T>(body: F) -> JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    builder().spawn(body).expect("failed to spawn thread")
}

/// [`std::thread::scope`] whose spawned threads hold [`RUNTIME_STACK_SIZE`].
///
/// `std::thread::Scope::spawn` always uses the default stack, so a scope from
/// the standard library cannot hold this contract. This one can.
pub fn scope<'env, F, T>(body: F) -> T
where
    F: for<'scope> FnOnce(RuntimeScope<'scope, 'env>) -> T,
{
    std::thread::scope(|inner| body(RuntimeScope { inner }))
}

/// A scope whose threads hold [`RUNTIME_STACK_SIZE`]. See [`scope`].
#[derive(Clone, Copy)]
pub struct RuntimeScope<'scope, 'env: 'scope> {
    inner: &'scope Scope<'scope, 'env>,
}

impl<'scope, 'env> RuntimeScope<'scope, 'env> {
    /// [`std::thread::Scope::spawn`] with [`RUNTIME_STACK_SIZE`].
    ///
    /// Panics if the OS cannot create the thread, as the standard library does.
    pub fn spawn<F, T>(&self, body: F) -> ScopedJoinHandle<'scope, T>
    where
        F: FnOnce() -> T + Send + 'scope,
        T: Send + 'scope,
    {
        builder()
            .spawn_scoped(self.inner, body)
            .expect("failed to spawn scoped thread")
    }

    /// Spawn a named scoped thread, reporting a spawn failure to the caller.
    pub fn spawn_named<F, T>(
        &self,
        name: impl Into<String>,
        body: F,
    ) -> io::Result<ScopedJoinHandle<'scope, T>>
    where
        F: FnOnce() -> T + Send + 'scope,
        T: Send + 'scope,
    {
        builder().name(name.into()).spawn_scoped(self.inner, body)
    }
}

/// Run `body` on a thread that holds the [`RUNTIME_STACK_SIZE`] contract.
///
/// A caller that drives the VM from a thread it did not create borrows
/// whatever stack that thread was given. The test harness is where this keeps
/// happening: a case that builds a Tokio runtime on the libtest thread creates
/// no thread of its own, so it runs the VM on libtest's stack. That stack is
/// large enough only because every CI lane exports `RUST_MIN_STACK`, and a
/// developer machine without it aborts the whole test binary on one ordinary
/// agent loop (harn#7962). An abort is not a failed case: every later case in
/// the binary silently never runs.
///
/// Panics propagate to the caller unchanged, so a failing assertion inside
/// `body` still fails its own test.
pub fn on_vm_stack<R: Send>(body: impl FnOnce() -> R + Send) -> R {
    scope(|scope| {
        scope
            .spawn_named("harn-vm-contract-stack", body)
            .expect("spawn a thread holding the VM stack contract")
            .join()
            .unwrap_or_else(|payload| std::panic::resume_unwind(payload))
    })
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
    const PROBE_CHILD: &str = "HARN_RUNTIME_STACK_PROBE_CHILD";

    /// 8 MiB is past Rust's 2 MiB default and under [`super::RUNTIME_STACK_SIZE`].
    const PROBE_BYTES: usize = 8 * 1024 * 1024;

    /// Every way this module creates a thread survives a probe the default
    /// stack cannot.
    ///
    /// It runs in a re-exec'd child with `RUST_MIN_STACK` cleared, because
    /// every Rust test lane exports `RUST_MIN_STACK=16777216` and that alone
    /// makes an unsized thread big enough. Asserting in this process would pass
    /// with or without the contract.
    #[test]
    fn every_spawn_form_holds_the_runtime_stack() {
        if std::env::var_os(PROBE_CHILD).is_some() {
            let depth = PROBE_BYTES / (16 * 1024);
            super::spawn(move || burn_stack(depth))
                .join()
                .expect("spawn");
            super::builder()
                .name("probe".to_owned())
                .spawn(move || burn_stack(depth))
                .expect("builder spawn")
                .join()
                .expect("builder");
            super::scope(|scope| {
                scope
                    .spawn(move || burn_stack(depth))
                    .join()
                    .expect("scope spawn");
                scope
                    .spawn_named("probe", move || burn_stack(depth))
                    .expect("spawn_named")
                    .join()
                    .expect("scope spawn_named");
            });
            super::on_vm_stack(move || burn_stack(depth));
            return;
        }

        let probe = |child_env: Option<&str>| {
            let mut command =
                std::process::Command::new(std::env::current_exe().expect("test executable"));
            command
                .args([
                    "--exact",
                    "runtime_stack::tests::every_spawn_form_holds_the_runtime_stack",
                    "--test-threads=1",
                ])
                .env_remove("RUST_MIN_STACK");
            if let Some(value) = child_env {
                command.env(PROBE_CHILD, value);
            }
            command.status().expect("re-exec the probe")
        };
        let status = probe(Some("1"));
        assert!(
            status.success(),
            "a runtime_stack thread overflowed {PROBE_BYTES} bytes with RUST_MIN_STACK \
             unset ({status}), so it took Rust's 2 MiB default"
        );
    }

    /// The negative control: the same probe on a default-stack thread dies,
    /// so the test above is measuring the stack size and not a probe too small
    /// to matter.
    #[test]
    fn the_probe_overflows_a_default_stack() {
        if std::env::var_os(PROBE_CHILD).is_some() {
            return;
        }
        let status = std::process::Command::new(std::env::current_exe().expect("test executable"))
            .args([
                "--exact",
                "runtime_stack::tests::default_stack_probe_child",
                "--test-threads=1",
            ])
            .env_remove("RUST_MIN_STACK")
            .env(PROBE_CHILD, "1")
            .status()
            .expect("re-exec the default-stack probe");
        assert!(
            !status.success(),
            "the probe survived Rust's 2 MiB default stack, so it proves nothing"
        );
    }

    /// Only run as the re-exec'd child of `the_probe_overflows_a_default_stack`.
    #[test]
    fn default_stack_probe_child() {
        if std::env::var_os(PROBE_CHILD).is_none() {
            return;
        }
        let depth = PROBE_BYTES / (16 * 1024);
        // The thread under test: deliberately the standard library's default.
        let _ = std::thread::spawn(move || burn_stack(depth)).join();
    }
}
