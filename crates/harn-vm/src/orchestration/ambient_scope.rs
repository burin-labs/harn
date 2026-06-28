//! Per-task ambient execution scope.
//!
//! Capability/identity context — execution policy, approval policy, command
//! policy, dynamic permissions, the bridge-trust + command-hook depths, and the
//! runtime-context overlay — is held in thread-local LIFO stacks. That model is
//! sound for a single synchronous call stack, but a guard held across an
//! `.await` is **not**: workers are spawned with [`tokio::task::spawn_local`],
//! so several of them interleave on one thread (and, under a work-stealing
//! multi-thread runtime, migrate between threads). A child that pushes its
//! policy, awaits its model call, and resumes would otherwise read whatever a
//! *sibling* pushed in the meantime — cross-wiring each child's file scoping,
//! tool ceiling, approval, and event attribution.
//!
//! [`AmbientExecutionScope`] gives every spawned worker its **own** copy of
//! these stacks. [`scope_ambient`] wraps the worker future so the task's scope
//! is swapped into the thread-locals on poll-enter and swapped back out on
//! poll-exit (the same technique `tracing::Instrument` uses for span context).
//! Only the currently-polling task's scope is ever live on a thread, so the
//! cooperative/work-stealing interleaving is invisible to capability checks.
//! Each swap is an O(1) `mem::replace` of a `Vec`/`usize`, so the per-poll cost
//! is a handful of pointer swaps regardless of stack depth.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use pin_project_lite::pin_project;

use super::command_policy::{
    swap_command_policy_hook_depth, swap_command_policy_stack, CommandPolicy,
};
use super::policy::{
    swap_approval_policy_stack, swap_execution_policy_stack, swap_trusted_bridge_depth,
    CapabilityPolicy, ToolApprovalPolicy,
};
use crate::autonomy::{swap_autonomy_policy_stack, AutonomyPolicy};
use crate::connectors::harn_module::swap_active_harn_connector_ctx;
use crate::connectors::ConnectorCtx;
use crate::llm::permissions::{swap_dynamic_permission_stack, DynamicPermissionPolicy};
use crate::runtime_context::{swap_runtime_context_overlay_stack, RuntimeContextOverlay};
use crate::stdlib::template::llm_context::{swap_llm_render_stack, LlmRenderContext};

/// An isolated snapshot of every ambient capability/identity stack a worker
/// task owns while it runs. `Default` is the empty scope (no policies, depth 0).
#[derive(Default, Clone)]
pub(crate) struct AmbientExecutionScope {
    execution: Vec<CapabilityPolicy>,
    approval: Vec<ToolApprovalPolicy>,
    command: Vec<CommandPolicy>,
    permissions: Vec<DynamicPermissionPolicy>,
    runtime_context: Vec<RuntimeContextOverlay>,
    autonomy: Vec<AutonomyPolicy>,
    llm_render: Vec<LlmRenderContext>,
    connector_ctx: Vec<ConnectorCtx>,
    trusted_depth: usize,
    command_hook_depth: usize,
}

/// Clone the contents of one ambient stack without disturbing it: swap it out,
/// clone, swap it back. Used only at spawn time (rare), so the double swap is
/// immaterial.
fn clone_via_swap<T: Clone>(swap: impl Fn(Vec<T>) -> Vec<T>) -> Vec<T> {
    let owned = swap(Vec::new());
    let cloned = owned.clone();
    let _ = swap(owned);
    cloned
}

impl AmbientExecutionScope {
    /// Snapshot the ambient context a child inherits from its parent at spawn
    /// time: the command-policy stack, dynamic-permission stack, and the
    /// runtime-context overlay (so the child's events keep the parent's
    /// `run_id`/`workflow_id` while it layers its own `worker_id` on top).
    ///
    /// Execution and approval policy are deliberately *not* captured here: the
    /// worker re-establishes its own base execution policy and approval policy
    /// explicitly at startup, and the bridge-trust / command-hook depths begin
    /// fresh for a new logical call stack. The transient per-call frames
    /// (`llm_render`, `connector_ctx`) start empty too — they are pushed by the
    /// child's own `llm_call` / connector export — and only need isolation, not
    /// inheritance. Autonomy policy IS inherited (the child runs under the
    /// parent's autonomy tier).
    pub(crate) fn capture_inherited() -> Self {
        Self {
            command: clone_via_swap(swap_command_policy_stack),
            permissions: clone_via_swap(swap_dynamic_permission_stack),
            runtime_context: clone_via_swap(swap_runtime_context_overlay_stack),
            autonomy: clone_via_swap(swap_autonomy_policy_stack),
            ..Self::default()
        }
    }

    /// Install this scope into the ambient thread-locals, returning whatever was
    /// installed before so the caller can restore it. O(1) per stack.
    fn swap_in(self) -> Self {
        Self {
            execution: swap_execution_policy_stack(self.execution),
            approval: swap_approval_policy_stack(self.approval),
            command: swap_command_policy_stack(self.command),
            permissions: swap_dynamic_permission_stack(self.permissions),
            runtime_context: swap_runtime_context_overlay_stack(self.runtime_context),
            autonomy: swap_autonomy_policy_stack(self.autonomy),
            llm_render: swap_llm_render_stack(self.llm_render),
            connector_ctx: swap_active_harn_connector_ctx(self.connector_ctx),
            trusted_depth: swap_trusted_bridge_depth(self.trusted_depth),
            command_hook_depth: swap_command_policy_hook_depth(self.command_hook_depth),
        }
    }
}

pin_project! {
    /// A future that runs `inner` with `scope` installed as the ambient
    /// execution scope. See the module docs.
    pub(crate) struct Scoped<F> {
        #[pin]
        inner: F,
        scope: Option<AmbientExecutionScope>,
    }
}

/// Run `inner` with its own isolated [`AmbientExecutionScope`]. The scope is
/// swapped into the thread-locals around every poll, so the task never observes
/// — and never leaks to — a sibling's capability context.
pub(crate) fn scope_ambient<F: Future>(scope: AmbientExecutionScope, inner: F) -> Scoped<F> {
    Scoped {
        inner,
        scope: Some(scope),
    }
}

/// Restores the outer scope (and saves the task's own scope back) on drop, so
/// the thread-locals are left correct even if the inner poll panics.
struct RestoreGuard<'a> {
    outer: Option<AmbientExecutionScope>,
    slot: &'a mut Option<AmbientExecutionScope>,
}

impl Drop for RestoreGuard<'_> {
    fn drop(&mut self) {
        if let Some(outer) = self.outer.take() {
            *self.slot = Some(outer.swap_in());
        }
    }
}

impl<F: Future> Future for Scoped<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<F::Output> {
        let this = self.project();
        // Install this task's scope, capturing whatever the polling thread had.
        let task_scope = this.scope.take().unwrap_or_default();
        let outer = task_scope.swap_in();
        let _restore = RestoreGuard {
            outer: Some(outer),
            slot: this.scope,
        };
        this.inner.poll(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::{current_execution_policy, push_execution_policy};

    fn policy_named(tool: &str) -> CapabilityPolicy {
        CapabilityPolicy {
            tools: vec![tool.to_string()],
            ..Default::default()
        }
    }

    /// Two cooperatively-scheduled tasks on one `LocalSet`, each pushing a
    /// distinct execution policy and then yielding twice so the sibling runs in
    /// between. With per-task scoping each task must read back ONLY its own
    /// policy; the un-scoped thread-local stack would hand a task its sibling's
    /// top-of-stack (the fan-out cross-wiring bug).
    #[tokio::test]
    async fn scoped_tasks_do_not_cross_wire_execution_policy() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let alpha = tokio::task::spawn_local(scope_ambient(
                    AmbientExecutionScope::default(),
                    async {
                        push_execution_policy(policy_named("alpha"));
                        tokio::task::yield_now().await;
                        tokio::task::yield_now().await;
                        current_execution_policy().map(|p| p.tools)
                    },
                ));
                let beta = tokio::task::spawn_local(scope_ambient(
                    AmbientExecutionScope::default(),
                    async {
                        push_execution_policy(policy_named("beta"));
                        tokio::task::yield_now().await;
                        tokio::task::yield_now().await;
                        current_execution_policy().map(|p| p.tools)
                    },
                ));
                assert_eq!(alpha.await.unwrap(), Some(vec!["alpha".to_string()]));
                assert_eq!(beta.await.unwrap(), Some(vec!["beta".to_string()]));
            })
            .await;
        // The outer thread is left clean — neither task's policy leaked out.
        assert!(current_execution_policy().is_none());
    }

    /// A task's scope must not leak into work that runs after it on the same
    /// thread once the scoped future has completed.
    #[tokio::test]
    async fn scope_is_restored_after_completion() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                tokio::task::spawn_local(scope_ambient(AmbientExecutionScope::default(), async {
                    push_execution_policy(policy_named("gamma"));
                    tokio::task::yield_now().await;
                }))
                .await
                .unwrap();
            })
            .await;
        assert!(current_execution_policy().is_none());
    }
}
