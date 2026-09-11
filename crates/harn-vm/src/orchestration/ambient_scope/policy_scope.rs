//! Per-poll scopes for host- and Harn-supplied policy overlays.
//!
//! The policy types stay independent, but their async lifetime has one owner:
//! [`AmbientExecutionScope`]. Every adapter below appends exactly one typed
//! policy slot to a full caller snapshot and delegates polling to the same
//! swap machinery.

use std::future::Future;
use std::sync::Arc;

use super::{scope_ambient, AmbientExecutionScope, Scoped};
use crate::autonomy::AutonomyPolicy;
use crate::llm::permissions::DynamicPermissionPolicy;
use crate::orchestration::{CapabilityPolicy, CommandPolicy, ToolApprovalPolicy};

/// The execution policy owned by a deferred callback's registering caller.
#[derive(Clone, Debug)]
pub(crate) struct RegisteredExecutionPolicy(Option<Arc<CapabilityPolicy>>);

impl RegisteredExecutionPolicy {
    pub(crate) fn capture() -> Self {
        Self(crate::orchestration::current_execution_policy().map(Arc::new))
    }

    pub(crate) fn scope<F: Future>(&self, inner: F) -> Scoped<F> {
        let policy = self.0.as_deref().cloned();
        scope_modified(inner, |scope| {
            scope.execution = policy.into_iter().collect();
            // A host hook's temporary exemption cannot grant authority to a
            // separately registered listener. Its own policy still applies.
            scope.trusted_depth = 0;
        })
    }
}

fn scope_modified<F: Future>(
    inner: F,
    modify: impl FnOnce(&mut AmbientExecutionScope),
) -> Scoped<F> {
    let mut scope = AmbientExecutionScope::capture_for_inline_subtask();
    modify(&mut scope);
    scope_ambient(scope, inner)
}

/// Run `inner` with `policy` appended to its execution-policy stack.
///
/// The policy is part of the future's owned ambient scope and is installed
/// around every poll. Unlike holding a thread-local push/pop guard across an
/// `.await`, this remains correct when Tokio interleaves tasks on one thread or
/// migrates a task between worker threads. Every other ambient slot is
/// inherited unchanged from the caller.
pub fn scope_execution_policy<F: Future>(
    policy: CapabilityPolicy,
    inner: F,
) -> impl Future<Output = F::Output> {
    scope_modified(inner, |scope| scope.execution.push(policy))
}

/// Run `inner` with the caller's ambient context owned by this task, without
/// adding a policy to it.
///
/// The counterpart to [`scope_execution_policy`] for a span that installs no
/// policy of its own but still holds thread-local state across `.await` — a
/// resource ceiling, say. Without this, such a span has nowhere to put that
/// state except the polling thread, where a task that interleaves with it
/// reads and restores the wrong value.
pub fn scope_ambient_context<F: Future>(inner: F) -> impl Future<Output = F::Output> {
    scope_modified(inner, |_| {})
}

pub(crate) fn scope_approval_policy<F: Future>(policy: ToolApprovalPolicy, inner: F) -> Scoped<F> {
    scope_modified(inner, |scope| scope.approval.push(policy))
}

pub(crate) fn scope_command_policy<F: Future>(policy: CommandPolicy, inner: F) -> Scoped<F> {
    scope_modified(inner, |scope| scope.command.push(policy))
}

pub(crate) fn scope_dynamic_permissions<F: Future>(
    policy: DynamicPermissionPolicy,
    inner: F,
) -> Scoped<F> {
    scope_modified(inner, |scope| scope.permissions.push(policy))
}

pub(crate) fn scope_autonomy_policy<F: Future>(policy: AutonomyPolicy, inner: F) -> Scoped<F> {
    scope_modified(inner, |scope| scope.autonomy.push(policy))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::{
        clear_execution_policy_stacks, current_execution_policy, push_execution_policy,
    };

    fn policy_named(tool: &str) -> CapabilityPolicy {
        CapabilityPolicy {
            tools: vec![tool.to_string()],
            ..CapabilityPolicy::default()
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn execution_policy_scope_survives_await_and_restores_the_caller() {
        clear_execution_policy_stacks();
        push_execution_policy(policy_named("outer"));

        scope_execution_policy(policy_named("scoped"), async {
            assert_eq!(
                current_execution_policy().unwrap().tools,
                vec!["scoped".to_string()]
            );
            tokio::task::yield_now().await;
            assert_eq!(
                current_execution_policy().unwrap().tools,
                vec!["scoped".to_string()]
            );
        })
        .await;

        assert_eq!(
            current_execution_policy().unwrap().tools,
            vec!["outer".to_string()]
        );
        clear_execution_policy_stacks();
    }

    #[tokio::test]
    async fn registered_policy_survives_yield_and_restores_emitter_trust_after_error() {
        use crate::orchestration::{
            allow_trusted_bridge_calls, enforce_current_policy_for_capability, pop_execution_policy,
        };

        clear_execution_policy_stacks();
        let mut registered = policy_named("listener");
        registered.side_effect_level = Some("read_only".into());
        push_execution_policy(registered.clone());
        let captured = RegisteredExecutionPolicy::capture();
        pop_execution_policy();
        let mut emitter = policy_named("emitter");
        emitter.side_effect_level = Some("read_only".into());
        push_execution_policy(emitter.clone());
        let trusted = allow_trusted_bridge_calls();
        let state_write = || {
            enforce_current_policy_for_capability(
                harn_builtin_meta::CapabilityId::Runtime,
                "store_set",
                &[],
            )
        };

        let result = captured
            .scope(async {
                assert_eq!(current_execution_policy(), Some(registered.clone()));
                assert!(
                    state_write().is_err(),
                    "emitter trust must not bypass the listener policy"
                );
                tokio::task::yield_now().await;
                assert_eq!(current_execution_policy(), Some(registered));
                assert!(state_write().is_err());
                Err::<(), _>("listener failed")
            })
            .await;

        assert_eq!(result, Err("listener failed"));
        assert_eq!(current_execution_policy(), Some(emitter));
        assert!(
            state_write().is_ok(),
            "the emitter's trusted scope must be restored"
        );
        drop(trusted);
        assert!(
            state_write().is_err(),
            "the restored trust guard must still unwind"
        );
        pop_execution_policy();
        assert!(current_execution_policy().is_none());
    }
}
