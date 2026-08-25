//! One owner for running a Harn child interpreter as a subtask.
//!
//! `spawn`, `parallel`, `parallel each`, `parallel settle`, the parallel
//! stream fan-out, and `pool.submit` all create the same thing: a child
//! interpreter that runs a closure concurrently with its parent. Each one
//! needs the same three things, and each used to arrange them differently.
//!
//! 1. An isolated copy of the parent's ambient execution scope. It is captured
//!    eagerly, while the parent's scope is still swapped in, so the subtask
//!    carries the agent session, policies, and event attribution that were
//!    live at the moment it was created.
//! 2. The parent's pool registry, so `pool.*` inside the subtask reaches the
//!    same pools instead of a fresh per-thread fallback.
//! 3. A place on the runtime to run.
//!
//! [`prepare`] does the first two. [`spawn`] and [`spawn_into`] do the third.
//! Nothing else in the crate decides where a child interpreter runs.
//!
//! # Placement
//!
//! [`SubtaskPlacement`] selects between the creating thread and the runtime's
//! worker threads. Worker placement is the default now that every capability
//! reachable from a child interpreter has an execution-scoped cross-thread
//! owner. It gives CPU-bound fan-out real parallelism:
//! a tight compute loop never yields, so pinned subtasks run one at a time no
//! matter how many workers are idle.
//!
//! Both placements require the subtask future to be `Send + 'static`. That is
//! deliberate. The bound is a property of the seam, not of the placement, so
//! changing the default cannot turn a compiling program into a broken one.

use std::future::Future;
use std::sync::Arc;
use std::task::{Context, Poll};

use crate::orchestration::{scope_ambient, AmbientExecutionScope};
use crate::stdlib::pool::{with_pool_registry_scope, PoolRegistry};
use pin_project_lite::pin_project;

pin_project! {
    /// Proof that a future carries the ambient scope required at a runtime
    /// thread boundary.
    ///
    /// The constructor stays private to this module. Callers can only obtain
    /// one through [`prepare`], and the spawn functions only accept this type,
    /// so a new child-interpreter path cannot accidentally bypass scope
    /// capture while still compiling.
    pub(crate) struct PreparedSubtask<F> {
        #[pin]
        inner: F,
    }
}

impl<F: Future> Future for PreparedSubtask<F> {
    type Output = F::Output;

    fn poll(self: std::pin::Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        self.project().inner.poll(context)
    }
}

impl<F: Future> PreparedSubtask<F> {
    /// Transform the output without discarding the proof carried by this
    /// future. Executors use this to attach source-order indices before
    /// inserting a branch into a `JoinSet`.
    pub(crate) fn map_output<M, T>(self, map: M) -> PreparedSubtask<impl Future<Output = T>>
    where
        M: FnOnce(F::Output) -> T,
    {
        PreparedSubtask {
            inner: async move { map(self.await) },
        }
    }
}

/// Where a subtask runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SubtaskPlacement {
    /// Run on the thread that created the subtask. Branches interleave at
    /// await points and never migrate, preserving thread-affine capability and
    /// embedding-host contracts.
    CurrentThread,
    /// Run on the runtime's worker threads. CPU-bound branches of one fan-out
    /// then run at the same time on different cores. `current_thread` remains
    /// an explicit compatibility mode for deliberately single-threaded hosts.
    #[default]
    Worker,
}

/// The environment variable that selects placement for a whole process.
pub const PLACEMENT_ENV: &str = "HARN_VM_SUBTASK_PLACEMENT";
/// Canonical values accepted for [`PLACEMENT_ENV`]. The environment registry
/// consumes this same vocabulary, so startup validation and placement parsing
/// cannot drift.
pub const PLACEMENT_VALUES: &[&str] = &["worker", "current_thread"];

/// An operator supplied a placement outside the runtime-owned vocabulary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubtaskPlacementParseError {
    value: String,
}

impl std::fmt::Display for SubtaskPlacementParseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "invalid {PLACEMENT_ENV} value {:?}; expected one of {}",
            self.value,
            PLACEMENT_VALUES.join(", ")
        )
    }
}

impl std::error::Error for SubtaskPlacementParseError {}

impl SubtaskPlacement {
    /// Parse an operator-supplied placement name against the runtime-owned
    /// closed vocabulary.
    pub fn from_env_value(value: &str) -> Result<Self, SubtaskPlacementParseError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "worker" => Ok(Self::Worker),
            "current_thread" => Ok(Self::CurrentThread),
            _ => Err(SubtaskPlacementParseError {
                value: value.to_string(),
            }),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Worker => "worker",
            Self::CurrentThread => "current_thread",
        }
    }
}

impl std::fmt::Display for SubtaskPlacement {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.name())
    }
}

/// The process-wide placement, read from the environment once.
fn placement_from_environment() -> SubtaskPlacement {
    static RESOLVED: std::sync::OnceLock<SubtaskPlacement> = std::sync::OnceLock::new();
    *RESOLVED.get_or_init(|| {
        let Ok(value) = std::env::var(PLACEMENT_ENV) else {
            return SubtaskPlacement::default();
        };
        SubtaskPlacement::from_env_value(&value).unwrap_or_else(|error| panic!("{error}"))
    })
}

thread_local! {
    /// An execution-scoped placement override. It is captured by
    /// [`AmbientExecutionScope`], so a nested subtask keeps the placement its
    /// execution tree was started with instead of falling back to the process
    /// default on a worker thread.
    static SUBTASK_PLACEMENT_CONTEXT: std::cell::RefCell<Option<SubtaskPlacement>> =
        const { std::cell::RefCell::new(None) };
}

/// Swap the execution-scoped placement override. Paired with
/// [`AmbientExecutionScope`]'s per-poll swap.
pub(crate) fn swap_subtask_placement_context(
    next: Option<SubtaskPlacement>,
) -> Option<SubtaskPlacement> {
    SUBTASK_PLACEMENT_CONTEXT.with(|slot| std::mem::replace(&mut *slot.borrow_mut(), next))
}

/// The placement new subtasks created on this thread will use.
pub fn placement() -> SubtaskPlacement {
    SUBTASK_PLACEMENT_CONTEXT
        .with(|slot| *slot.borrow())
        .unwrap_or_else(placement_from_environment)
}

/// Run `inner` with `placement` installed for every subtask its execution tree
/// creates. The override rides [`AmbientExecutionScope`], so it survives the
/// awaits and thread migrations inside `inner`.
pub fn scope_placement<F: Future>(
    placement: SubtaskPlacement,
    inner: F,
) -> impl Future<Output = F::Output> {
    let mut scope = AmbientExecutionScope::capture_for_inline_subtask();
    scope.set_subtask_placement(Some(placement));
    scope_ambient(scope, inner)
}

/// Wrap a child-interpreter body so it carries its parent's ambient execution
/// scope and pool registry.
///
/// Capture happens here, synchronously, while the creating task's scope is
/// still swapped in. A subtask created from inside a fan-out worker whose own
/// scope has already been swapped out would otherwise read an empty or sibling
/// context: subtasks are independent runtime tasks, not nested inside the
/// parent's poll.
pub(crate) fn prepare<F: Future>(
    registry: Arc<PoolRegistry>,
    future: F,
) -> PreparedSubtask<impl Future<Output = F::Output>> {
    PreparedSubtask {
        inner: scope_ambient(
            AmbientExecutionScope::capture_for_inline_subtask(),
            with_pool_registry_scope(registry, future),
        ),
    }
}

/// Put a prepared subtask on the runtime.
pub(crate) fn spawn<F>(future: PreparedSubtask<F>) -> tokio::task::JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    match placement() {
        SubtaskPlacement::Worker => tokio::spawn(future),
        SubtaskPlacement::CurrentThread => tokio::task::spawn_local(future),
    }
}

/// Put a prepared subtask on the runtime as a member of `set`.
pub(crate) fn spawn_into<F>(
    set: &mut tokio::task::JoinSet<F::Output>,
    future: PreparedSubtask<F>,
) -> tokio::task::AbortHandle
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    match placement() {
        SubtaskPlacement::Worker => set.spawn(future),
        SubtaskPlacement::CurrentThread => {
            // A current-thread execution tree is the deterministic placement.
            // Give each child its first poll in source order before admitting
            // the next child; Tokio's local ready queue does not promise the
            // same order across separately constructed runtimes. A pending
            // future is polled again immediately by the spawned task, replacing
            // this noop waker with its real scheduler waker.
            let mut future = Box::pin(future);
            let mut context = Context::from_waker(std::task::Waker::noop());
            match future.as_mut().poll(&mut context) {
                Poll::Ready(value) => set.spawn_local(async move { value }),
                Poll::Pending => set.spawn_local(future),
            }
        }
    }
}

/// Prepare and spawn in one step, for callers that do not hold the future
/// between the two.
pub(crate) fn spawn_child<F>(
    registry: Arc<PoolRegistry>,
    future: F,
) -> tokio::task::JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    spawn(prepare(registry, future))
}

/// Spawn a long-lived child that inherits execution policy but owns an
/// independent session lifecycle.
pub(crate) fn spawn_inherited_child<F>(
    registry: Arc<PoolRegistry>,
    future: F,
) -> tokio::task::JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    spawn(PreparedSubtask {
        inner: scope_ambient(
            AmbientExecutionScope::capture_inherited(),
            with_pool_registry_scope(registry, future),
        ),
    })
}

#[cfg(test)]
#[path = "subtask/cross_thread_tests.rs"]
mod cross_thread_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placement_names_round_trip() {
        assert_eq!(
            SubtaskPlacement::from_env_value("worker"),
            Ok(SubtaskPlacement::Worker)
        );
        assert_eq!(
            SubtaskPlacement::from_env_value(" CURRENT_THREAD "),
            Ok(SubtaskPlacement::CurrentThread)
        );
        assert_eq!(
            SubtaskPlacement::from_env_value("sideways")
                .expect_err("invalid placement must not become an absent override")
                .to_string(),
            "invalid HARN_VM_SUBTASK_PLACEMENT value \"sideways\"; expected one of worker, current_thread"
        );
        assert_eq!(SubtaskPlacement::Worker.to_string(), "worker");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn scoped_placement_reaches_the_spawn_seam() {
        assert_eq!(placement(), SubtaskPlacement::Worker);
        let observed =
            scope_placement(SubtaskPlacement::CurrentThread, async { placement() }).await;
        assert_eq!(observed, SubtaskPlacement::CurrentThread);
        assert_eq!(placement(), SubtaskPlacement::Worker);
    }

    #[test]
    fn placement_selects_the_executor_thread() {
        fn observed_thread(
            runtime: &tokio::runtime::Runtime,
            placement: SubtaskPlacement,
        ) -> std::thread::ThreadId {
            runtime.block_on(async {
                tokio::task::LocalSet::new()
                    .run_until(scope_placement(placement, async {
                        spawn(PreparedSubtask {
                            inner: async { std::thread::current().id() },
                        })
                        .await
                        .expect("subtask completes")
                    }))
                    .await
            })
        }

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("test runtime");
        let creating_thread = std::thread::current().id();

        assert_ne!(
            observed_thread(&runtime, SubtaskPlacement::Worker),
            creating_thread
        );
        assert_eq!(
            observed_thread(&runtime, SubtaskPlacement::CurrentThread),
            creating_thread
        );
    }
}
