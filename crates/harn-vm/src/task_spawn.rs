//! The one way harn-vm starts a task or a blocking job.
//!
//! Ambient execution context (policy, session, persistent state and the branch
//! a run tape replays) lives in thread-locals that a scoped future installs
//! around each poll. A raw spawn drops it: the new task runs under whatever the
//! thread holds when it is polled, so a branch prefix's work could run its
//! effects again against the recording's state. Each helper here captures the
//! spawner's scope and carries it into the task. `clippy.toml` bans the raw
//! APIs in this crate.
#![allow(clippy::disallowed_methods)]

use std::future::Future;

use tokio::task::{AbortHandle, JoinHandle, JoinSet};

use crate::orchestration::{scope_ambient, scope_inline_subtask, AmbientExecutionScope};

/// Spawn `future` on the current `LocalSet` as part of the spawning task: it
/// inherits that task's whole ambient scope.
pub(crate) fn spawn_local<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future + 'static,
    F::Output: 'static,
{
    tokio::task::spawn_local(scope_inline_subtask(future))
}

/// Spawn `future` on the current `LocalSet` under `scope`, for a task that
/// needs other inheritance than an inline subtask, such as a worker's.
pub(crate) fn spawn_local_in<F>(scope: AmbientExecutionScope, future: F) -> JoinHandle<F::Output>
where
    F: Future + 'static,
    F::Output: 'static,
{
    tokio::task::spawn_local(scope_ambient(scope, future))
}

/// [`spawn_local`] into `set`.
pub(crate) fn spawn_local_into<F>(set: &mut JoinSet<F::Output>, future: F) -> AbortHandle
where
    F: Future + 'static,
    F::Output: 'static,
{
    set.spawn_local(scope_inline_subtask(future))
}

/// Spawn a `Send` future on the runtime with the spawner's ambient scope.
pub(crate) fn spawn<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    tokio::spawn(scope_inline_subtask(future))
}

/// [`spawn`] on the runtime behind `handle`.
pub(crate) fn spawn_on<F>(handle: &tokio::runtime::Handle, future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    handle.spawn(scope_inline_subtask(future))
}

/// Run lifecycle cleanup on its own runtime inside the spawner's branch, if
/// any, so a branch run's terminal persistence lands in the branch. Cleanup
/// carries its session runtimes itself; outside a branch this adds nothing to
/// the run-end path.
pub(crate) fn spawn_lifecycle_cleanup<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    match crate::run_tape::current_branch() {
        Some(branch) => crate::vm::subtask::spawn_lifecycle_cleanup(
            crate::persistent_state::run_in_branch(branch, future),
        ),
        None => crate::vm::subtask::spawn_lifecycle_cleanup(future),
    }
}

/// Run `f` on the blocking pool with the spawner's ambient scope.
pub(crate) fn spawn_blocking<F, R>(f: F) -> JoinHandle<R>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    crate::orchestration::run_blocking_with_ambient(f)
}

/// Start an OS thread holding the VM stack contract, under the spawner's
/// ambient scope. Panics if the OS cannot create the thread.
pub(crate) fn thread<F, T>(body: F) -> std::thread::JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    crate::runtime_stack::spawn(crate::orchestration::carry_ambient(body))
}

/// [`thread`] named `name`, reporting a spawn failure to the caller.
pub(crate) fn named_thread<F, T>(
    name: impl Into<String>,
    body: F,
) -> std::io::Result<std::thread::JoinHandle<T>>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    crate::runtime_stack::builder()
        .name(name.into())
        .spawn(crate::orchestration::carry_ambient(body))
}

/// [`thread`] inside a [`crate::runtime_stack::scope`].
pub(crate) fn scoped_thread<'scope, 'env, F, T>(
    scope: crate::runtime_stack::RuntimeScope<'scope, 'env>,
    body: F,
) -> std::thread::ScopedJoinHandle<'scope, T>
where
    F: FnOnce() -> T + Send + 'scope,
    T: Send + 'scope,
{
    scope.spawn(crate::orchestration::carry_ambient(body))
}
