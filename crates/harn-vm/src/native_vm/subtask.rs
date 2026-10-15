//! Native capability and credential context at child-interpreter boundaries.

use crate::{
    orchestration::{scope_ambient, AmbientExecutionScope},
    secrets::SecretProvider,
    stdlib::pool::PoolRegistry,
    tool_registry::preparation_scope::SubtaskPreparationScope,
    vm::{subtask, VmExecutionScope},
};
use std::{
    future::Future,
    sync::Arc,
    task::{Context, Poll},
};

#[cfg(test)]
mod cross_thread_tests;
#[cfg(test)]
mod placement_tests;
#[cfg(all(test, unix))]
mod preparation_tests;

struct NativeSubtaskScope {
    ambient: AmbientExecutionScope,
    pools: Arc<PoolRegistry>,
    provider: Option<Arc<dyn SecretProvider>>,
    preparation: SubtaskPreparationScope,
}

impl VmExecutionScope for NativeSubtaskScope {
    fn poll(
        &mut self,
        cx: &mut Context<'_>,
        continuation: &mut dyn FnMut(&mut Context<'_>) -> Poll<()>,
    ) -> Poll<()> {
        let Self {
            ambient,
            pools,
            provider,
            preparation,
        } = self;
        ambient.poll_scoped(false, || {
            crate::secrets::with_secret_provider_poll(provider, || {
                preparation.poll(|| {
                    crate::stdlib::pool::with_pool_registry_poll(pools, || continuation(cx))
                })
            })
        })
    }
}

pub(super) fn capture_inline(registry: Arc<PoolRegistry>) -> Box<dyn VmExecutionScope> {
    Box::new(NativeSubtaskScope {
        ambient: AmbientExecutionScope::capture_for_inline_subtask(),
        pools: registry,
        provider: crate::secrets::active_secret_provider(),
        preparation: SubtaskPreparationScope::capture(true),
    })
}

pub(crate) fn prepare<F: Future>(
    registry: Arc<PoolRegistry>,
    future: F,
) -> subtask::PreparedSubtask<impl Future<Output = F::Output>> {
    subtask::prepare(capture_inline(registry), future)
}

pub(crate) fn spawn_child<F>(
    registry: Arc<PoolRegistry>,
    future: F,
) -> tokio::task::JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    subtask::spawn(prepare(registry, future))
}

/// Long-lived children inherit restrictions but own their session lifecycle.
pub(crate) fn spawn_inherited_child<F>(
    registry: Arc<PoolRegistry>,
    future: F,
) -> tokio::task::JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let scope = Box::new(NativeSubtaskScope {
        ambient: AmbientExecutionScope::capture_inherited(),
        pools: registry,
        provider: None,
        preparation: SubtaskPreparationScope::capture(false),
    });
    subtask::spawn(subtask::prepare(scope, future))
}

/// Select placement for a complete native execution, including nested children.
pub fn scope_placement<F: Future>(
    placement: subtask::SubtaskPlacement,
    inner: F,
) -> impl Future<Output = F::Output> {
    let mut scope = AmbientExecutionScope::capture_for_inline_subtask();
    scope.set_subtask_placement(Some(placement));
    scope_ambient(scope, inner)
}
