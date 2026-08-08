//! Per-poll scopes for host- and Harn-supplied policy overlays.
//!
//! The policy types stay independent, but their async lifetime has one owner:
//! [`AmbientExecutionScope`]. Every adapter below appends exactly one typed
//! policy slot to a full caller snapshot and delegates polling to the same
//! swap machinery.

use std::future::Future;

use super::{scope_ambient, AmbientExecutionScope, Scoped};
use crate::autonomy::AutonomyPolicy;
use crate::llm::permissions::DynamicPermissionPolicy;
use crate::orchestration::{CapabilityPolicy, CommandPolicy, ToolApprovalPolicy};

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
}
