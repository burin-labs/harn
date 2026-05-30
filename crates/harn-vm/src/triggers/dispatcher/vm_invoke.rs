use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use futures::pin_mut;
use tokio::sync::broadcast;

use crate::trust_graph::{policy_for_autonomy_tier, AutonomyTier};
use crate::value::VmValue;

use super::state::{ACTIVE_DISPATCH_CONTEXT, ACTIVE_DISPATCH_WAIT_LEASE};
use super::types::{
    DispatchContext, DispatchError, DispatchExecutionPolicyGuard, DispatchWaitLease, Dispatcher,
};
use super::util::{
    dispatch_cancel_requested, dispatch_error_from_vm_error, event_to_handler_value, recv_cancel,
    split_binding_key,
};
use super::TriggerEvent;

impl Dispatcher {
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn invoke_vm_callable(
        &self,
        closure: &crate::value::VmClosure,
        binding_key: &str,
        event: &TriggerEvent,
        replay_of_event_id: Option<&String>,
        agent_id: &str,
        action: &str,
        autonomy_tier: AutonomyTier,
        wait_lease: Option<DispatchWaitLease>,
        cancel_rx: &mut broadcast::Receiver<()>,
    ) -> Result<VmValue, DispatchError> {
        let mut vm = self.base_vm.child_vm();
        let cancel_token = Arc::new(std::sync::atomic::AtomicBool::new(false));
        if self.state.shutting_down.load(Ordering::SeqCst) {
            cancel_token.store(true, Ordering::SeqCst);
        }
        self.state
            .cancel_tokens
            .lock()
            .expect("dispatcher cancel tokens poisoned")
            .push(cancel_token.clone());
        vm.install_cancel_token(cancel_token.clone());
        let arg = event_to_handler_value(event)?;
        let args = [arg];
        let tier_policy = policy_for_autonomy_tier(autonomy_tier);
        let effective_policy = match crate::orchestration::current_execution_policy() {
            Some(parent) => parent
                .intersect(&tier_policy)
                .map_err(DispatchError::Local)?,
            None => tier_policy,
        };
        let _execution_context_guard = DispatchProcessContextGuard::install(&vm);
        crate::orchestration::push_execution_policy(effective_policy);
        let _policy_guard = DispatchExecutionPolicyGuard;
        // Bind a child of the dispatcher's base VM as the async-builtin context
        // for the handler's execution. The dispatcher fires from a spawned task
        // (timeout sleeper, stream poller, etc.) with no ambient `task_local`
        // context — it carries its own `base_vm` precisely because triggers run
        // detached — so handler-invoked async builtins must resolve their
        // `clone_async_builtin_child_vm` root here, or the resumed agent loop
        // hangs waiting on a context that never appears. See harn#2667.
        let future = crate::vm::scope_async_builtin(
            self.base_vm.child_vm(),
            vm.call_closure_pub(closure, &args),
        );
        pin_mut!(future);
        let (binding_id, binding_version) = split_binding_key(binding_key);
        let prior_context = ACTIVE_DISPATCH_CONTEXT.with(|slot| {
            slot.borrow_mut().replace(DispatchContext {
                trigger_event: event.clone(),
                replay_of_event_id: replay_of_event_id.cloned(),
                binding_id,
                binding_version,
                agent_id: agent_id.to_string(),
                action: action.to_string(),
                autonomy_tier,
            })
        });
        let prior_wait_lease = ACTIVE_DISPATCH_WAIT_LEASE
            .with(|slot| std::mem::replace(&mut *slot.borrow_mut(), wait_lease));
        let prior_hitl_state = crate::stdlib::hitl::take_hitl_state();
        crate::stdlib::hitl::reset_hitl_state();
        let mut poll = tokio::time::interval(Duration::from_millis(100));
        let result = loop {
            tokio::select! {
                result = &mut future => break result,
                _ = recv_cancel(cancel_rx) => {
                    cancel_token.store(true, Ordering::SeqCst);
                }
                _ = poll.tick() => {
                    if dispatch_cancel_requested(
                        &self.event_log,
                        binding_key,
                        &event.id.0,
                        replay_of_event_id,
                    )
                    .await? {
                        cancel_token.store(true, Ordering::SeqCst);
                    }
                }
            }
        };
        ACTIVE_DISPATCH_CONTEXT.with(|slot| {
            *slot.borrow_mut() = prior_context;
        });
        ACTIVE_DISPATCH_WAIT_LEASE.with(|slot| {
            *slot.borrow_mut() = prior_wait_lease;
        });
        crate::stdlib::hitl::restore_hitl_state(prior_hitl_state);
        {
            let mut tokens = self
                .state
                .cancel_tokens
                .lock()
                .expect("dispatcher cancel tokens poisoned");
            tokens.retain(|token| !Arc::ptr_eq(token, &cancel_token));
        }

        if cancel_token.load(Ordering::SeqCst) {
            if dispatch_cancel_requested(
                &self.event_log,
                binding_key,
                &event.id.0,
                replay_of_event_id,
            )
            .await?
            {
                Err(DispatchError::Cancelled(
                    "trigger cancel request cancelled local handler".to_string(),
                ))
            } else {
                Err(DispatchError::Cancelled(
                    "dispatcher shutdown cancelled local handler".to_string(),
                ))
            }
        } else {
            result.map_err(dispatch_error_from_vm_error)
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn invoke_vm_callable_with_timeout(
        &self,
        closure: &crate::value::VmClosure,
        binding_key: &str,
        event: &TriggerEvent,
        replay_of_event_id: Option<&String>,
        agent_id: &str,
        action: &str,
        autonomy_tier: AutonomyTier,
        cancel_rx: &mut broadcast::Receiver<()>,
        timeout: Option<Duration>,
    ) -> Result<VmValue, DispatchError> {
        let future = self.invoke_vm_callable(
            closure,
            binding_key,
            event,
            replay_of_event_id,
            agent_id,
            action,
            autonomy_tier,
            None,
            cancel_rx,
        );
        pin_mut!(future);
        if let Some(timeout) = timeout {
            match tokio::time::timeout(timeout, future).await {
                Ok(result) => result,
                Err(_) => Err(DispatchError::Local(
                    "predicate evaluation timed out".to_string(),
                )),
            }
        } else {
            future.await
        }
    }
}

struct DispatchProcessContextGuard {
    prior: Option<crate::orchestration::RunExecutionRecord>,
    installed: bool,
}

impl DispatchProcessContextGuard {
    fn install(vm: &crate::vm::Vm) -> Self {
        let execution_source_dir = vm
            .source_dir
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned());
        let fallback_cwd = vm
            .project_root()
            .map(|path| path.to_string_lossy().into_owned());
        let prior = crate::stdlib::process::current_execution_context();

        let next = match prior.clone() {
            Some(mut context) => {
                if context.cwd.is_none() {
                    context.cwd = fallback_cwd;
                }
                if context.source_dir.is_none() {
                    context.source_dir = execution_source_dir;
                }
                Some(context)
            }
            None if fallback_cwd.is_some() || execution_source_dir.is_some() => {
                Some(crate::orchestration::RunExecutionRecord {
                    cwd: fallback_cwd,
                    source_dir: execution_source_dir,
                    env: Default::default(),
                    adapter: None,
                    repo_path: None,
                    worktree_path: None,
                    branch: None,
                    base_ref: None,
                    cleanup: None,
                })
            }
            None => None,
        };
        let installed = next.is_some();
        if let Some(next) = next {
            crate::stdlib::process::set_thread_execution_context(Some(next));
        }
        Self { prior, installed }
    }
}

impl Drop for DispatchProcessContextGuard {
    fn drop(&mut self) {
        if self.installed {
            crate::stdlib::process::set_thread_execution_context(self.prior.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::DispatchProcessContextGuard;

    fn run_record(
        cwd: Option<&std::path::Path>,
        source_dir: Option<&std::path::Path>,
    ) -> crate::orchestration::RunExecutionRecord {
        crate::orchestration::RunExecutionRecord {
            cwd: cwd.map(|path| path.to_string_lossy().into_owned()),
            source_dir: source_dir.map(|path| path.to_string_lossy().into_owned()),
            env: Default::default(),
            adapter: None,
            repo_path: None,
            worktree_path: None,
            branch: None,
            base_ref: None,
            cleanup: None,
        }
    }

    #[test]
    fn dispatch_context_preserves_existing_execution_cwd() {
        crate::stdlib::process::reset_process_state();
        let existing_cwd = tempfile::tempdir().unwrap();
        let handler_source = tempfile::tempdir().unwrap();
        crate::stdlib::process::set_thread_execution_context(Some(run_record(
            Some(existing_cwd.path()),
            None,
        )));
        let mut vm = crate::vm::Vm::new();
        vm.set_source_dir(handler_source.path());

        {
            let _guard = DispatchProcessContextGuard::install(&vm);
            let current = crate::stdlib::process::current_execution_context().unwrap();
            assert_eq!(
                current.cwd.as_deref(),
                Some(existing_cwd.path().to_string_lossy().as_ref())
            );
            assert_eq!(
                current.source_dir.as_deref(),
                Some(handler_source.path().to_string_lossy().as_ref())
            );
        }

        let restored = crate::stdlib::process::current_execution_context().unwrap();
        assert_eq!(
            restored.cwd.as_deref(),
            Some(existing_cwd.path().to_string_lossy().as_ref())
        );
        assert!(restored.source_dir.is_none());
        crate::stdlib::process::reset_process_state();
    }

    #[test]
    fn dispatch_context_installs_vm_root_without_existing_context() {
        crate::stdlib::process::reset_process_state();
        let handler_source = tempfile::tempdir().unwrap();
        let mut vm = crate::vm::Vm::new();
        vm.set_source_dir(handler_source.path());

        {
            let _guard = DispatchProcessContextGuard::install(&vm);
            let current = crate::stdlib::process::current_execution_context().unwrap();
            assert_eq!(
                current.cwd.as_deref(),
                Some(handler_source.path().to_string_lossy().as_ref())
            );
            assert_eq!(
                current.source_dir.as_deref(),
                Some(handler_source.path().to_string_lossy().as_ref())
            );
        }

        assert!(crate::stdlib::process::current_execution_context().is_none());
        crate::stdlib::process::reset_process_state();
    }
}
