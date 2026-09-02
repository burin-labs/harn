use crate::value::VmDictExt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::chunk::{DirectCallState, DirectCallTarget, InlineCacheEntry};
use crate::orchestration::HookEvent;
use crate::value::{DeadlockError, VmClosure, VmError, VmValue};
use crate::BuiltinId;

use super::super::{CallArgs, CallFrame};
use super::call_support::{AwaitingTask, StepPreHookAction};

const DIRECT_CALL_QUICKEN_THRESHOLD: u8 = 3;

impl super::super::Vm {
    async fn retry_pending_task_cleanup(&mut self, public_task_id: &str) -> Result<bool, VmError> {
        let Some(pending) = self.pending_task_cleanups.get(public_task_id).cloned() else {
            return Ok(false);
        };
        crate::llm::agent_session_host::cancellation::abandon_task_sessions(
            &pending.execution_id,
            &pending.task_id,
        )
        .await?;
        self.pending_task_cleanups.remove(public_task_id);
        Ok(true)
    }

    fn step_domain_args(args: &[VmValue]) -> &[VmValue] {
        match args.first() {
            Some(VmValue::Harness(handle))
                if handle.kind() == crate::harness::HarnessKind::Root =>
            {
                &args[1..]
            }
            _ => args,
        }
    }

    fn step_hook_payload(
        event: HookEvent,
        persona: Option<&str>,
        step_name: &str,
        function_name: &str,
        args: &[VmValue],
        output: Option<VmValue>,
    ) -> VmValue {
        let mut step = std::collections::BTreeMap::new();
        step.put_str("name", step_name);
        step.put_str("function", function_name);
        step.insert(
            "args".to_string(),
            VmValue::List(std::sync::Arc::new(Self::step_domain_args(args).to_vec())),
        );
        let mut payload = std::collections::BTreeMap::new();
        payload.put_str("event", event.as_str());
        payload.put_str(
            "target",
            match persona {
                Some(persona) if !persona.is_empty() => format!("{persona}.{step_name}"),
                _ => step_name.to_string(),
            },
        );
        payload.put_str("persona", persona.unwrap_or(""));
        payload.insert("step".to_string(), VmValue::dict(step));
        if let Some(output) = output {
            payload.insert("output".to_string(), output);
        }
        VmValue::dict(payload)
    }

    fn parse_step_pre_hook_result(
        value: VmValue,
        current_args: Vec<VmValue>,
    ) -> Result<StepPreHookAction, VmError> {
        match value {
            VmValue::Nil => Ok(StepPreHookAction::Allow(current_args)),
            VmValue::String(text) if text.as_str() == "Allow" => {
                Ok(StepPreHookAction::Allow(current_args))
            }
            VmValue::Dict(map) => {
                if let Some(reason) = map.get("deny").or_else(|| map.get("reason")) {
                    return Ok(StepPreHookAction::Deny(reason.display()));
                }
                if matches!(
                    map.get("action").map(|value| value.display()).as_deref(),
                    Some("deny" | "Deny")
                ) {
                    return Ok(StepPreHookAction::Deny(
                        map.get("reason")
                            .map(|value| value.display())
                            .unwrap_or_else(|| "step hook denied execution".to_string()),
                    ));
                }
                if let Some(VmValue::List(args)) = map.get("args").or_else(|| map.get("modify")) {
                    let mut modified = Vec::with_capacity(
                        args.len()
                            + usize::from(
                                Self::step_domain_args(&current_args).len() < current_args.len(),
                            ),
                    );
                    if Self::step_domain_args(&current_args).len() < current_args.len() {
                        modified.push(current_args[0].clone());
                    }
                    modified.extend((**args).clone());
                    return Ok(StepPreHookAction::Allow(modified));
                }
                Ok(StepPreHookAction::Allow(current_args))
            }
            other => Err(VmError::Runtime(format!(
                "PreStep hook must return nil, Allow, or {{deny|args}}, got {}",
                other.type_name()
            ))),
        }
    }

    fn parse_step_post_hook_result(
        value: VmValue,
        current_output: VmValue,
    ) -> Result<VmValue, VmError> {
        match value {
            VmValue::Nil => Ok(current_output),
            VmValue::String(text) if text.as_str() == "Pass" => Ok(current_output),
            VmValue::Dict(map) => Ok(map
                .get("output")
                .or_else(|| map.get("result"))
                .or_else(|| map.get("modify"))
                .cloned()
                .unwrap_or(current_output)),
            other => Err(VmError::Runtime(format!(
                "PostStep hook must return nil, Pass, or {{output}}, got {}",
                other.type_name()
            ))),
        }
    }

    async fn run_step_pre_hooks(
        &mut self,
        closure: &VmClosure,
        mut args: Vec<VmValue>,
    ) -> Result<Vec<VmValue>, VmError> {
        let Some(definition) =
            crate::step_runtime::step_definition_for_function(&closure.func.name)
        else {
            return Ok(args);
        };
        let persona = crate::step_runtime::current_persona_name();
        let match_payload = Self::step_hook_payload(
            HookEvent::PreStep,
            persona.as_deref(),
            &definition.name,
            &definition.function,
            &args,
            None,
        );
        let manifest_hooks = crate::orchestration::matching_vm_lifecycle_hooks(
            HookEvent::PreStep,
            &crate::llm::vm_value_to_json(&match_payload),
        );
        for hook in manifest_hooks {
            let payload = Self::step_hook_payload(
                HookEvent::PreStep,
                persona.as_deref(),
                &definition.name,
                &definition.function,
                &args,
                None,
            );
            let closure = hook.resolve(self).await?;
            let raw = self.call_lifecycle_hook(&closure, payload).await?;
            let (raw, effects) = crate::orchestration::collect_hook_effects_and_action(
                HookEvent::PreStep,
                raw,
                VmValue::Nil,
            )?;
            crate::orchestration::inject_hook_effects_into_current_session(effects)?;
            match Self::parse_step_pre_hook_result(raw, args)? {
                StepPreHookAction::Allow(next_args) => args = next_args,
                StepPreHookAction::Deny(reason) => {
                    return Err(Self::step_hook_denied(&definition.name, reason));
                }
            }
        }
        let hooks = crate::step_runtime::matching_hooks(
            HookEvent::PreStep,
            persona.as_deref(),
            Some(&definition.name),
            None,
        );
        for hook in hooks {
            let payload = Self::step_hook_payload(
                hook.event,
                persona.as_deref(),
                &definition.name,
                &definition.function,
                &args,
                None,
            );
            let raw = self.call_lifecycle_hook(&hook.handler, payload).await?;
            let (raw, effects) = crate::orchestration::collect_hook_effects_and_action(
                HookEvent::PreStep,
                raw,
                VmValue::Nil,
            )?;
            crate::orchestration::inject_hook_effects_into_current_session(effects)?;
            match Self::parse_step_pre_hook_result(raw, args)? {
                StepPreHookAction::Allow(next_args) => args = next_args,
                StepPreHookAction::Deny(reason) => {
                    return Err(Self::step_hook_denied(&definition.name, reason));
                }
            }
        }
        Ok(args)
    }

    fn step_hook_denied(step_name: &str, reason: String) -> VmError {
        VmError::Thrown(VmValue::dict(std::collections::BTreeMap::from([
            (
                "category".to_string(),
                VmValue::String(arcstr::ArcStr::from("hook_denied")),
            ),
            (
                "event".to_string(),
                VmValue::String(arcstr::ArcStr::from("PreStep")),
            ),
            (
                "reason".to_string(),
                VmValue::String(arcstr::ArcStr::from(reason)),
            ),
            (
                "step".to_string(),
                VmValue::String(arcstr::ArcStr::from(step_name.to_string())),
            ),
        ])))
    }

    pub(crate) async fn run_step_post_hooks_for_current_frame(
        &mut self,
        output: VmValue,
    ) -> Result<VmValue, VmError> {
        let depth = self.frames.len();
        let Some(step) = crate::step_runtime::take_active_step(depth) else {
            return Ok(output);
        };
        let persona = step.persona.clone();
        let mut current = output;
        let hooks = crate::step_runtime::matching_hooks(
            HookEvent::PostStep,
            persona.as_deref(),
            Some(&step.definition.name),
            None,
        );
        let result = async {
            let manifest_hooks = crate::orchestration::matching_vm_lifecycle_hooks(
                HookEvent::PostStep,
                &crate::llm::vm_value_to_json(&Self::step_hook_payload(
                    HookEvent::PostStep,
                    persona.as_deref(),
                    &step.definition.name,
                    &step.definition.function,
                    &step.args,
                    Some(current.clone()),
                )),
            );
            for hook in manifest_hooks {
                let payload = Self::step_hook_payload(
                    HookEvent::PostStep,
                    persona.as_deref(),
                    &step.definition.name,
                    &step.definition.function,
                    &step.args,
                    Some(current.clone()),
                );
                let closure = hook.resolve(self).await?;
                let raw = self.call_lifecycle_hook(&closure, payload).await?;
                let (raw, effects) = crate::orchestration::collect_hook_effects_and_action(
                    HookEvent::PostStep,
                    raw,
                    VmValue::Nil,
                )?;
                crate::orchestration::inject_hook_effects_into_current_session(effects)?;
                current = Self::parse_step_post_hook_result(raw, current)?;
            }
            for hook in hooks {
                let payload = Self::step_hook_payload(
                    hook.event,
                    persona.as_deref(),
                    &step.definition.name,
                    &step.definition.function,
                    &step.args,
                    Some(current.clone()),
                );
                let raw = self.call_lifecycle_hook(&hook.handler, payload).await?;
                let (raw, effects) = crate::orchestration::collect_hook_effects_and_action(
                    HookEvent::PostStep,
                    raw,
                    VmValue::Nil,
                )?;
                crate::orchestration::inject_hook_effects_into_current_session(effects)?;
                current = Self::parse_step_post_hook_result(raw, current)?;
            }
            Ok::<VmValue, VmError>(current)
        }
        .await;
        match result {
            Ok(value) => {
                crate::step_runtime::finish_active_step(step, "completed", None);
                Ok(value)
            }
            Err(error) => {
                crate::step_runtime::finish_active_step(step, "failed", Some(error.to_string()));
                Err(error)
            }
        }
    }

    pub(in crate::vm) async fn call_user_closure(
        &mut self,
        closure: Arc<VmClosure>,
        args: Vec<VmValue>,
    ) -> Result<(), VmError> {
        if closure.func.is_generator {
            let gen = self.create_generator(&closure, &args);
            self.stack.push(gen);
        } else {
            let args = self.run_step_pre_hooks(&closure, args).await?;
            self.push_closure_frame(&closure, &args)?;
        }
        Ok(())
    }

    /// Box-pin'd to break the static recursion between `drive_until_frame_depth`
    /// (the hot dispatch loop) and `call_closure` (the hot per-callback path):
    /// a step's lifecycle hook is itself a closure, which re-enters the
    /// dispatch loop, which may again pop a step frame and fire post-hooks.
    /// Indirecting at this slow-path hook boundary keeps the recursion
    /// satisfied while the per-element dispatch path stays free of
    /// per-invocation heap allocation.
    fn call_lifecycle_hook<'a>(
        &'a mut self,
        handler: &'a Arc<VmClosure>,
        payload: VmValue,
    ) -> Pin<Box<dyn Future<Output = Result<VmValue, VmError>> + Send + 'a>> {
        Box::pin(async move {
            let active_context = crate::step_runtime::suspend_active_context();
            let result = self
                .call_closure_args(handler, CallArgs::One(&payload))
                .await;
            drop(active_context);
            result
        })
    }

    pub(in crate::vm::ops) fn stack_arg_start(&self, argc: usize) -> Result<usize, VmError> {
        self.stack
            .len()
            .checked_sub(argc)
            .ok_or_else(|| VmError::Runtime("call argument stack underflow".to_string()))
    }

    pub(in crate::vm) fn take_stack_args_from(
        &mut self,
        args_start: usize,
    ) -> Result<Vec<VmValue>, VmError> {
        if args_start > self.stack.len() {
            return Err(VmError::Runtime(
                "call argument stack underflow".to_string(),
            ));
        }
        Ok(self.stack.drain(args_start..).collect())
    }

    pub(in crate::vm::ops) fn is_special_name(name: &str) -> bool {
        matches!(
            name,
            "await"
                | "cancel"
                | "cancel_graceful"
                | "is_cancelled"
                | "__signal_on_interrupt"
                | "__signal_off_interrupt"
                | "__signal_interrupted"
                | "__signal_raise"
        )
    }

    pub(in crate::vm::ops) async fn try_call_special_name(
        &mut self,
        name: &str,
        args: &[VmValue],
    ) -> Result<bool, VmError> {
        if name == "await" {
            crate::typecheck::validate_builtin_call(name, args, None)?;
            let task_id = args.first().and_then(|a| match a {
                VmValue::TaskHandle(id) => Some(id.to_string()),
                _ => None,
            });
            if let Some(id) = task_id {
                // Awaiting one's own join handle can never complete — surface
                // a deterministic self-join deadlock instead of blocking.
                if id == self.runtime_context.task_id {
                    return Err(VmError::Deadlock(Box::new(DeadlockError::self_deadlock(
                        "task",
                        id,
                        "task is awaiting its own join handle (self-join)",
                    ))));
                }
                if let Some(task) = self.spawned_tasks.get(&id) {
                    if task.wait_task_id == self.runtime_context.task_id {
                        return Err(VmError::Deadlock(Box::new(DeadlockError::self_deadlock(
                            "task",
                            id,
                            "task is awaiting its own join handle (self-join)",
                        ))));
                    }
                    let _wait = self.wait_for_graph.wait_for_tasks(
                        &self.runtime_context.task_id,
                        [task.wait_task_id.clone()],
                    )?;
                    let handle = self
                        .spawned_tasks
                        .remove(&id)
                        .expect("spawned task was present before await wait registration");
                    // Explicitly awaited: drop it from any enclosing nursery so
                    // `scope {}` exit neither double-joins nor cancels it.
                    self.deregister_task_from_scopes(&id);
                    let joined = AwaitingTask::new(handle, self.agent_cleanup_runtimes())
                        .join()
                        .await
                        .map_err(|e| VmError::Runtime(format!("Task join error: {e}")))??;
                    let (result, task_output) = joined;
                    self.output.push_str(&task_output);
                    self.stack.push(result);
                } else {
                    self.stack.push(VmValue::Nil);
                }
            } else {
                self.stack
                    .push(args.first().cloned().unwrap_or(VmValue::Nil));
            }
            return Ok(true);
        }

        if name == "cancel" {
            crate::typecheck::validate_builtin_call(name, args, None)?;
            if let Some(VmValue::TaskHandle(id)) = args.first() {
                if let Some(task) = self.spawned_tasks.remove(id.as_str()) {
                    let runtime_task_id = task.wait_task_id.clone();
                    if let Err(error) =
                        super::call_support::abort_task_and_wait(task, self.execution_id()).await
                    {
                        self.pending_task_cleanups.insert(
                            id.to_string(),
                            super::super::PendingTaskCleanup {
                                execution_id: self.execution_id().to_string(),
                                task_id: runtime_task_id,
                            },
                        );
                        return Err(error);
                    }
                } else {
                    self.retry_pending_task_cleanup(id.as_str()).await?;
                }
            }
            self.stack.push(VmValue::Nil);
            return Ok(true);
        }

        if name == "cancel_graceful" {
            crate::typecheck::validate_builtin_call(name, args, None)?;
            let task_id = args.first().and_then(|a| match a {
                VmValue::TaskHandle(id) => Some(id.to_string()),
                _ => None,
            });
            let timeout_ms = args
                .get(1)
                .and_then(|a| match a {
                    VmValue::Int(n) => Some(*n as u64),
                    VmValue::Duration(ms) => Some((*ms).max(0) as u64),
                    _ => None,
                })
                .unwrap_or(5000);
            if let Some(id) = task_id {
                if let Some(task) = self.spawned_tasks.remove(&id) {
                    let task_runtime_id = task.wait_task_id.clone();
                    task.cancel_token
                        .store(true, std::sync::atomic::Ordering::SeqCst);
                    let mut handle = task.handle;
                    let timeout =
                        tokio::time::sleep(tokio::time::Duration::from_millis(timeout_ms));
                    tokio::pin!(timeout);
                    tokio::select! {
                        joined = &mut handle => {
                            match joined {
                                Ok(Ok((result, output))) => {
                                    self.output.push_str(&output);
                                    self.stack.push(VmValue::enum_variant("Result", "Ok", vec![result]));
                                }
                                Ok(Err(e)) => {
                                    self.stack.push(VmValue::enum_variant(
                                        "Result",
                                        "Err",
                                        vec![VmValue::String(arcstr::ArcStr::from(e.to_string()))],
                                    ));
                                }
                                Err(e) => {
                                    self.stack.push(VmValue::enum_variant(
                                        "Result",
                                        "Err",
                                        vec![VmValue::String(arcstr::ArcStr::from(format!("Task join error: {e}")))],
                                    ));
                                }
                            }
                        }
                        _ = &mut timeout => {
                            super::call_support::abort_join_and_wait(&mut handle).await;
                            if let Err(error) = crate::llm::agent_session_host::cancellation::abandon_task_sessions(
                                self.execution_id(),
                                &task_runtime_id,
                            ).await {
                                self.pending_task_cleanups.insert(
                                    id.clone(),
                                    super::super::PendingTaskCleanup {
                                        execution_id: self.execution_id().to_string(),
                                        task_id: task_runtime_id,
                                    },
                                );
                                return Err(error);
                            }
                            self.stack.push(VmValue::enum_variant(
                                "Result",
                                "Err",
                                vec![VmValue::String(arcstr::ArcStr::from(
                                    "cancel_graceful: timeout, task forcefully aborted",
                                ))],
                            ));
                        }
                    }
                } else if self.retry_pending_task_cleanup(&id).await? {
                    self.stack
                        .push(VmValue::enum_variant("Result", "Ok", vec![VmValue::Nil]));
                } else {
                    self.stack
                        .push(VmValue::enum_variant("Result", "Ok", vec![VmValue::Nil]));
                }
            } else {
                self.stack.push(VmValue::Nil);
            }
            return Ok(true);
        }

        if name == "is_cancelled" {
            crate::typecheck::validate_builtin_call(name, args, None)?;
            let cancelled = self
                .cancel_token
                .as_ref()
                .map(|t| t.load(std::sync::atomic::Ordering::SeqCst))
                .unwrap_or(false);
            self.stack.push(VmValue::Bool(cancelled));
            return Ok(true);
        }

        if name == "__signal_on_interrupt" {
            crate::typecheck::validate_builtin_call(name, args, None)?;
            let handler = args.first().cloned().ok_or_else(|| {
                VmError::Runtime("__signal_on_interrupt: handler is required".to_string())
            })?;
            let result = self.register_interrupt_handler(handler, args.get(1))?;
            self.stack.push(result);
            return Ok(true);
        }

        if name == "__signal_off_interrupt" {
            crate::typecheck::validate_builtin_call(name, args, None)?;
            let handle = args.first().ok_or_else(|| {
                VmError::Runtime("__signal_off_interrupt: handle is required".to_string())
            })?;
            self.unregister_interrupt_handler(handle)?;
            self.stack.push(VmValue::Nil);
            return Ok(true);
        }

        if name == "__signal_interrupted" {
            crate::typecheck::validate_builtin_call(name, args, None)?;
            self.stack.push(VmValue::Bool(self.interrupted()));
            return Ok(true);
        }

        if name == "__signal_raise" {
            crate::typecheck::validate_builtin_call(name, args, None)?;
            let signal = match args.first() {
                Some(VmValue::String(signal)) => signal.to_string(),
                Some(other) => {
                    return Err(VmError::TypeError(format!(
                        "__signal_raise: signal must be string, got {}",
                        other.type_name()
                    )))
                }
                None => "SIGINT".to_string(),
            };
            self.signal_interrupt(&signal)?;
            self.stack.push(VmValue::Nil);
            return Ok(true);
        }

        Ok(false)
    }

    /// Sync fast path for `Op::Call`. Peeks the callee before touching
    /// `ip`; regular user closures can enter a new frame directly from the
    /// existing stack argument slice. Anything that needs async handling
    /// leaves `ip` untouched so [`execute_call_async`] can read the operand
    /// exactly once.
    pub(super) fn execute_call_sync(&mut self) -> Option<Result<(), VmError>> {
        let (cache_site, argc) = {
            let frame = self.frames.last().unwrap();
            let argc = frame.chunk.code[frame.ip] as usize;
            (frame.inline_cache_site_for_previous_op(), argc)
        };
        let cached_state = cache_site
            .slot
            .and_then(|slot| self.peek_direct_call_state_by_index(cache_site.cache_set, slot));
        let callee_idx = self.stack.len().checked_sub(argc + 1)?;
        let cached_hit = self.try_cached_direct_call(cached_state.as_ref(), argc, callee_idx);
        let specialized_hit = cached_hit.is_some();
        let closure = match cached_hit {
            Some(closure) => closure,
            None => match self.stack.get(callee_idx)? {
                VmValue::Closure(c) => Arc::clone(c),
                _ => return None,
            },
        };
        if !Self::direct_call_cacheable(&closure) {
            return None;
        }

        // Steady-state specialized hit: the entry already holds exactly this
        // target and argc, so rewriting it would clone the target `Arc` out
        // and back in per call purely to advance a `hits` counter nothing
        // reads past the promotion threshold (deopt is driven by `misses`).
        // The counter freezes at its promotion value instead.
        if let (Some(slot), false) = (cache_site.slot, specialized_hit) {
            let next_entry = Self::next_direct_call_entry(
                cached_state,
                argc,
                DirectCallTarget::Closure(Arc::clone(&closure)),
            );
            self.set_inline_cache_entry_by_index(
                cache_site.cache_set,
                cache_site.slot_count,
                slot,
                next_entry,
            );
        }

        let frame = self.frames.last_mut().unwrap();
        frame.ip += 1;
        let args_start = self.stack.len() - argc;
        Some(self.push_closure_frame_from_stack_args(&closure, args_start, callee_idx))
    }

    fn direct_call_cacheable(closure: &VmClosure) -> bool {
        !closure.func.is_generator
            && crate::step_runtime::step_definition_for_function(&closure.func.name).is_none()
    }

    fn try_cached_direct_call(
        &self,
        cached_state: Option<&DirectCallState>,
        argc: usize,
        callee_idx: usize,
    ) -> Option<Arc<VmClosure>> {
        let DirectCallState::Specialized {
            argc: cached_argc,
            target: DirectCallTarget::Closure(cached_closure),
            ..
        } = cached_state?
        else {
            return None;
        };
        if *cached_argc != argc {
            return None;
        }
        let VmValue::Closure(callee) = self.stack.get(callee_idx)? else {
            return None;
        };
        if !Arc::ptr_eq(cached_closure, callee) {
            return None;
        }
        Some(Arc::clone(cached_closure))
    }

    fn next_direct_call_entry(
        previous_state: Option<DirectCallState>,
        argc: usize,
        target: DirectCallTarget,
    ) -> InlineCacheEntry {
        let state = match previous_state {
            Some(DirectCallState::Warmup {
                argc: cached_argc,
                target: cached_target,
                hits,
            }) if cached_argc == argc && cached_target == target => {
                let hits = hits.saturating_add(1);
                if hits >= DIRECT_CALL_QUICKEN_THRESHOLD {
                    DirectCallState::Specialized {
                        argc,
                        target,
                        hits: hits as u64,
                        misses: 0,
                    }
                } else {
                    DirectCallState::Warmup { argc, target, hits }
                }
            }
            Some(DirectCallState::Specialized {
                argc: cached_argc,
                target: cached_target,
                hits,
                misses,
            }) if cached_argc == argc && cached_target == target => DirectCallState::Specialized {
                argc,
                target,
                hits: hits.saturating_add(1),
                misses,
            },
            Some(DirectCallState::Specialized { misses: 0, .. }) => DirectCallState::Specialized {
                argc,
                target,
                hits: 1,
                misses: 1,
            },
            _ => DirectCallState::Warmup {
                argc,
                target,
                hits: 1,
            },
        };
        InlineCacheEntry::DirectCall { state }
    }

    /// Async path for `Op::Call`. Arguments stay on the VM stack until the
    /// selected callee shape requires owned arguments.
    pub(super) async fn execute_call_async(&mut self) -> Result<(), VmError> {
        let frame = self.frames.last_mut().unwrap();
        let argc = frame.chunk.code[frame.ip] as usize;
        frame.ip += 1;

        let args_start = self.stack_arg_start(argc)?;
        let callee_idx = args_start
            .checked_sub(1)
            .ok_or_else(|| VmError::Runtime("call callee stack underflow".to_string()))?;
        let callee = self
            .stack
            .get(callee_idx)
            .cloned()
            .ok_or_else(|| VmError::Runtime("call callee stack underflow".to_string()))?;

        match callee {
            VmValue::String(name) => {
                self.call_named_value_from_stack_args(&name, args_start, callee_idx, None)
                    .await?;
            }
            VmValue::Closure(closure) => {
                self.call_user_closure_from_stack_args(closure, args_start, callee_idx)
                    .await?;
            }
            VmValue::Dict(registry) => {
                let closure = match crate::vm::tool_callable::require_single_harn_tool_handler(
                    &registry,
                    || format!("Cannot call {}", VmValue::Dict(registry.clone()).display()),
                ) {
                    Ok(closure) => closure,
                    Err(error) => {
                        self.stack.truncate(callee_idx);
                        return Err(error);
                    }
                };
                self.call_user_closure_from_stack_args(closure, args_start, callee_idx)
                    .await?;
            }
            VmValue::BuiltinRef(name) => {
                self.call_exact_value_from_stack_args(
                    VmValue::BuiltinRef(name),
                    args_start,
                    callee_idx,
                )
                .await?;
            }
            VmValue::BuiltinRefId(r) => {
                self.call_exact_value_from_stack_args(
                    VmValue::BuiltinRefId(r),
                    args_start,
                    callee_idx,
                )
                .await?;
            }
            _ => {
                let message = format!("Cannot call {}", callee.display());
                self.stack.truncate(callee_idx);
                return Err(VmError::TypeError(message));
            }
        }
        Ok(())
    }

    pub(super) async fn execute_call_spread(&mut self) -> Result<(), VmError> {
        let args_val = self.pop()?;
        let callee = self.pop()?;
        let args = match args_val {
            VmValue::List(items) => (*items).clone(),
            _ => {
                return Err(VmError::TypeError(
                    "spread call requires list arguments".into(),
                ))
            }
        };
        match callee {
            VmValue::String(name) => {
                self.call_named_value(&name, args, None).await?;
            }
            VmValue::Closure(closure) => {
                self.call_user_closure(closure, args).await?;
            }
            VmValue::Dict(registry) => {
                let closure =
                    crate::vm::tool_callable::require_single_harn_tool_handler(&registry, || {
                        format!("Cannot call {}", VmValue::Dict(registry.clone()).display())
                    })?;
                self.call_user_closure(closure, args).await?;
            }
            VmValue::BuiltinRef(name) => {
                self.call_exact_value(VmValue::BuiltinRef(name), args)
                    .await?;
            }
            VmValue::BuiltinRefId(r) => {
                self.call_exact_value(VmValue::BuiltinRefId(r), args)
                    .await?;
            }
            _ => {
                return Err(VmError::TypeError(format!(
                    "Cannot call {}",
                    callee.display()
                )))
            }
        }
        Ok(())
    }

    /// Sync fast path for `Op::CallBuiltin` — the opcode user-level `f(x)`
    /// calls compile to via [`Compiler::emit_named_call`]. Peeks the
    /// operand (`u64 id + u16 name_idx + u8 argc` = 11 bytes) without
    /// touching `ip`; if the name resolves to a regular non-generator
    /// user closure with no `@step` definition attached, pushes the
    /// closure frame inline and returns `Some(Ok(()))`. Returns `None`
    /// without advancing the frame when the call needs the async path
    /// (special-name builtins like `await`/`cancel`, generators, step-
    /// decorated functions, or names that resolve to a registered
    /// builtin instead of a user closure).
    ///
    /// Mirrors [`execute_call_sync`] and [`execute_iter_next_sync`]:
    /// since `ip` is untouched on the `None` hand-off, the async path
    /// reads the operand exactly once. Collapses the same five-async-
    /// state-machine chain (`execute_call_builtin` →
    /// `call_named_value` → `try_call_special_name` →
    /// `resolve_named_closure` → `call_user_closure` →
    /// `run_step_pre_hooks`) that the steady-state untracked
    /// user-closure call would otherwise traverse, each of which
    /// resolves synchronously in the hot case but still pays the
    /// future-state-machine tax.
    pub(super) fn execute_call_builtin_sync(&mut self) -> Option<Result<(), VmError>> {
        let (chunk, cache_site, name_idx, argc) = {
            let frame = self.frames.last().unwrap();
            let chunk = Arc::clone(&frame.chunk);
            let cache_site = frame.inline_cache_site_for_previous_op();
            let name_idx = frame.chunk.read_u16(frame.ip + 8) as usize;
            let argc = frame.chunk.code[frame.ip + 10] as usize;
            (chunk, cache_site, name_idx, argc)
        };
        let cached_state = cache_site
            .slot
            .and_then(|slot| self.peek_direct_call_state_by_index(cache_site.cache_set, slot));
        let name = Self::const_str(&chunk.constants[name_idx]).ok()?;

        // Names handled by `try_call_special_name` are runtime constructs
        // (`await`, `cancel`, ...) that require the async path. The async
        // dispatcher still resolves a lexical binding first, so user values
        // retain ordinary shadowing semantics.
        if Self::is_special_name(name) {
            return None;
        }

        let closure = match self.try_cached_named_direct_call(cached_state.as_ref(), name, argc) {
            Some(closure) => closure,
            None => match self.resolve_named_closure(name) {
                Some(closure) => closure,
                None => {
                    // A present lexical value owns the name regardless of its
                    // runtime shape. Let the async path dispatch that exact
                    // value instead of falling through to a same-named builtin.
                    if self.resolve_lexical_named_value(name).is_some() {
                        return None;
                    }
                    match crate::vm::tool_callable::resolve_named_single_harn_tool_handler(
                        self, name,
                    ) {
                        Ok(Some(closure)) => closure,
                        // Not a user closure or single-tool registry, so this
                        // bare call targets a builtin. Most builtins are
                        // synchronous; dispatch them right here on the sync path
                        // instead of bailing to `execute_call_builtin_async`.
                        Ok(None) => {
                            return self.try_dispatch_sync_builtin_inline(name, argc);
                        }
                        Err(_) => return None,
                    }
                }
            },
        };
        if !Self::direct_call_cacheable(&closure) {
            return None;
        }

        if let Some(slot) = cache_site.slot {
            let next_entry = Self::next_direct_call_entry(
                cached_state,
                argc,
                DirectCallTarget::Closure(Arc::clone(&closure)),
            );
            self.set_inline_cache_entry_by_index(
                cache_site.cache_set,
                cache_site.slot_count,
                slot,
                next_entry,
            );
        }

        let frame = self.frames.last_mut().unwrap();
        frame.ip += 11;
        let args_start = self.stack.len().checked_sub(argc)?;
        Some(self.push_closure_frame_from_stack_args(&closure, args_start, args_start))
    }

    /// Dispatch a synchronous builtin directly from the `Op::CallBuiltin` sync
    /// handler, bypassing the async path for the common case.
    ///
    /// Returns `Some(result)` when the named builtin is synchronous — consuming
    /// its `argc` stack arguments and advancing `ip` past the 11 operand bytes,
    /// exactly as the closure path does. Returns `None` when the builtin is
    /// asynchronous, bridge-backed, side-effect-gated, or otherwise not
    /// sync-dispatchable, leaving the operand stack and `ip` untouched so
    /// `execute_call_builtin_async` re-reads and handles it.
    ///
    /// Only reached after `resolve_named_closure` has already established the
    /// name is not a user closure, so this never shadows a user `fn`.
    fn try_dispatch_sync_builtin_inline(
        &mut self,
        name: &str,
        argc: usize,
    ) -> Option<Result<(), VmError>> {
        let id = {
            let frame = self.frames.last()?;
            BuiltinId::from_raw(frame.chunk.read_u64(frame.ip))
        };
        let args_start = self.stack.len().checked_sub(argc)?;
        // Dispatch straight off the operand stack — `*_from_stack_args` reads
        // the argument slice in place (no owned `Vec`), exactly as the async
        // handler does at its builtin branch. `None` means the builtin is not
        // synchronously dispatchable (async / bridge / side-effect-gated); the
        // stack is left untouched so `execute_call_builtin_async` handles it.
        match self.try_call_sync_builtin_id_or_name_from_stack_args(Some(id), name, args_start) {
            Some(result) => {
                self.frames.last_mut()?.ip += 11;
                self.stack.truncate(args_start);
                Some(match result {
                    Ok(value) => {
                        self.stack.push(value);
                        Ok(())
                    }
                    Err(error) => Err(error),
                })
            }
            None => None,
        }
    }

    fn try_cached_named_direct_call(
        &self,
        cached_state: Option<&DirectCallState>,
        name: &str,
        argc: usize,
    ) -> Option<Arc<VmClosure>> {
        let DirectCallState::Specialized {
            argc: cached_argc,
            target: DirectCallTarget::Closure(cached_closure),
            ..
        } = cached_state?
        else {
            return None;
        };
        if *cached_argc != argc {
            return None;
        }
        let resolved = self.resolve_named_closure(name)?;
        if !Arc::ptr_eq(cached_closure, &resolved) {
            return None;
        }
        Some(Arc::clone(cached_closure))
    }

    /// Async path for `Op::CallBuiltin`. Arguments stay on the VM stack
    /// until the selected callee shape requires owned arguments.
    pub(super) async fn execute_call_builtin_async(&mut self) -> Result<(), VmError> {
        let (chunk, id, name_idx, argc) = {
            let frame = self.frames.last_mut().unwrap();
            let id = BuiltinId::from_raw(frame.chunk.read_u64(frame.ip));
            frame.ip += 8;
            let name_idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let argc = frame.chunk.code[frame.ip] as usize;
            frame.ip += 1;
            (Arc::clone(&frame.chunk), id, name_idx, argc)
        };
        let name = Self::const_str(&chunk.constants[name_idx])?;
        let args_start = self.stack_arg_start(argc)?;
        self.call_named_value_from_stack_args(name, args_start, args_start, Some(id))
            .await
    }

    pub(super) async fn execute_call_builtin_spread(&mut self) -> Result<(), VmError> {
        let (chunk, id, name_idx) = {
            let frame = self.frames.last_mut().unwrap();
            let id = BuiltinId::from_raw(frame.chunk.read_u64(frame.ip));
            frame.ip += 8;
            let name_idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            (Arc::clone(&frame.chunk), id, name_idx)
        };
        let name = Self::const_str(&chunk.constants[name_idx])?;
        let args_val = self.pop()?;
        let args = match args_val {
            VmValue::List(items) => (*items).clone(),
            _ => {
                return Err(VmError::TypeError(
                    "spread call requires list arguments".into(),
                ))
            }
        };
        self.call_named_value(name, args, Some(id)).await
    }

    /// Tail-call optimization body: validate arguments directly on the VM
    /// stack, reuse the caller frame's stack base and saved env, then push
    /// the callee frame in place. Tracked persona/step frames use the
    /// non-TCO path so lifecycle hooks still observe explicit boundaries.
    fn perform_tail_call_tco_from_stack_args(
        &mut self,
        closure: Arc<VmClosure>,
        args_start: usize,
        stack_truncate_to: usize,
    ) -> Result<(), VmError> {
        if stack_truncate_to > args_start || args_start > self.stack.len() {
            return Err(VmError::Runtime(
                "invalid call argument stack range".to_string(),
            ));
        }
        let original_args = CallArgs::Slice(&self.stack[args_start..]);
        let legacy_args = self.legacy_ambient_call_args(&closure, &original_args)?;
        let args = legacy_args.as_deref().unwrap_or(&self.stack[args_start..]);
        if let Err(error) = crate::typecheck::validate_user_call(&closure.func, args, None) {
            self.stack.truncate(stack_truncate_to);
            return Err(error);
        }
        if closure.func.is_generator {
            let gen = self.create_generator(&closure, args);
            self.stack.truncate(stack_truncate_to);
            return Err(VmError::Return(gen));
        }

        let mut call_env = self.closure_call_env_for_current_frame(&closure);
        // TCO: reuse the current frame's stack_base / saved_env.
        let popped = self.frames.pop().unwrap();
        let stack_base = popped.stack_base;
        let parent_env = popped.saved_env;

        if let Some(ref dir) = popped.saved_source_dir {
            crate::stdlib::set_thread_source_dir(dir);
        }

        let saved_source_dir =
            crate::stdlib::process::enter_frame_source_dir(closure.source_dir.as_deref());

        call_env.push_scope();
        let debugger = self.debugger_attached();
        let initial_env = if debugger {
            Some(call_env.clone())
        } else {
            None
        };
        self.env = call_env;
        let mut local_slots = Self::fresh_local_slots(&closure.func.chunk);
        Self::bind_param_slots(&mut local_slots, &closure.func, args, false);
        let callee_argc = closure.func.callee_arg_count(args.len());
        let initial_local_slots = if debugger {
            Some(local_slots.clone())
        } else {
            None
        };

        // Inherit the popped frame's iterator depth so iterators pushed by
        // for-loops inside the caller (`return f(...)` from inside
        // `for x in xs { ... }`) get torn down when the tail-called callee
        // eventually returns, instead of leaking into the caller's caller.
        let saved_iterator_depth = popped.saved_iterator_depth;
        self.iterators.truncate(saved_iterator_depth);
        self.stack.truncate(stack_base);
        let chunk = Arc::clone(&closure.func.chunk);
        let inline_cache_set = self.inline_cache_set_index_for_chunk(&chunk);
        self.frames.push(CallFrame {
            chunk,
            inline_cache_set,
            ip: 0,
            stack_base,
            saved_env: parent_env,
            initial_env,
            initial_local_slots,
            saved_iterator_depth,
            fn_name: closure.func.name.clone(),
            argc: callee_argc,
            saved_source_dir,
            module_functions: closure.module_functions(),
            module_state: closure.module_state(),
            local_slots,
            local_scope_base: self.env.scope_depth().saturating_sub(1),
            local_scope_depth: 0,
        });
        Ok(())
    }

    fn current_frame_has_exception_handler(&self) -> bool {
        let current_depth = self.frames.len();
        self.exception_handlers
            .iter()
            .any(|handler| handler.frame_depth == current_depth)
    }

    /// Sync fast path for `Op::TailCall`. Peeks the callee on the stack
    /// **before** touching `ip`; if it resolves to a non-generator user
    /// closure and neither the current frame nor the callee is tracked
    /// by the persona/step registries, it performs the TCO frame reuse
    /// inline and returns `Some(Ok(()))`. Anything that needs the slow
    /// path (non-resolvable callee, generator, tracked frame, tracked
    /// callee) returns `None` without touching `ip`, so the caller falls
    /// through to [`execute_tail_call_async`] which reads the argc
    /// operand exactly once.
    ///
    /// Both direct `Closure` callees and the `String` form emitted by
    /// `compile_return_stmt` (`Op::Constant <name>` + `Op::TailCall`) are
    /// handled here: [`Vm::resolve_named_closure`] is itself synchronous,
    /// so the steady-state user-level tail call — the only shape
    /// `recursive_countdown.harn` exercises — stays on the sync path.
    ///
    /// The tracked-function guards mirror the async path's check at the
    /// top of [`execute_tail_call_async`]: persona/step lifecycle state
    /// is frame-owned, and TCO that elides a frame must not skip the
    /// PreStep/PostStep hook boundaries that a non-tail call would
    /// observe.
    pub(super) fn execute_tail_call_sync(&mut self) -> Option<Result<(), VmError>> {
        let frame = self.frames.last().unwrap();
        if crate::step_runtime::is_tracked_function(&frame.fn_name) {
            return None;
        }
        if self.current_frame_has_exception_handler() {
            return None;
        }
        let argc = frame.chunk.code[frame.ip] as usize;
        let callee_idx = self.stack.len().checked_sub(argc + 1)?;
        let resolved = match self.stack.get(callee_idx)? {
            VmValue::Closure(c) => Ok(Arc::clone(c)),
            VmValue::String(name) => Err(name.clone()),
            _ => return None,
        };
        let closure = match resolved {
            Ok(closure) => closure,
            Err(name) => self.resolve_named_closure(&name)?,
        };
        if closure.func.is_generator {
            return None;
        }
        if crate::step_runtime::is_tracked_function(&closure.func.name) {
            return None;
        }

        let frame = self.frames.last_mut().unwrap();
        frame.ip += 1;
        let args_start = self.stack.len() - argc;
        Some(self.perform_tail_call_tco_from_stack_args(closure, args_start, callee_idx))
    }

    /// Async path for `Op::TailCall`. Arguments stay on the VM stack until
    /// the selected callee shape requires owned arguments.
    pub(super) async fn execute_tail_call_async(&mut self) -> Result<(), VmError> {
        let frame = self.frames.last_mut().unwrap();
        let argc = frame.chunk.code[frame.ip] as usize;
        frame.ip += 1;

        let args_start = self.stack_arg_start(argc)?;
        let callee_idx = args_start
            .checked_sub(1)
            .ok_or_else(|| VmError::Runtime("call callee stack underflow".to_string()))?;
        let callee = self
            .stack
            .get(callee_idx)
            .cloned()
            .ok_or_else(|| VmError::Runtime("call callee stack underflow".to_string()))?;

        let resolved_closure = match &callee {
            VmValue::Closure(cl) => Some(Arc::clone(cl)),
            VmValue::String(name) => match self.resolve_named_closure(name) {
                Some(closure) => Some(closure),
                None if self.resolve_lexical_named_value(name).is_some() => None,
                None => {
                    match crate::vm::tool_callable::resolve_named_single_harn_tool_handler(
                        self, name,
                    ) {
                        Ok(closure) => closure,
                        Err(error) => {
                            self.stack.truncate(callee_idx);
                            return Err(error);
                        }
                    }
                }
            },
            VmValue::Dict(registry) => {
                match crate::vm::tool_callable::single_harn_tool_handler(registry) {
                    Ok(closure) => closure,
                    Err(error) => {
                        self.stack.truncate(callee_idx);
                        return Err(error);
                    }
                }
            }
            _ => None,
        };

        if let Some(closure) = resolved_closure {
            let current_fn_name = self
                .frames
                .last()
                .map(|frame| frame.fn_name.clone())
                .unwrap_or_default();
            if crate::step_runtime::is_tracked_function(&current_fn_name)
                || crate::step_runtime::is_tracked_function(&closure.func.name)
                || self.current_frame_has_exception_handler()
            {
                // Persona/step lifecycle state is frame-owned. Keep those
                // frames explicit so PreStep/PostStep hooks see the same
                // boundaries as a non-tail call. Exception handlers also
                // store instruction pointers into their owning frame, so
                // that frame cannot be elided while a handler is active.
                let args = self.take_stack_args_from(args_start)?;
                self.stack.truncate(callee_idx);
                self.call_user_closure(closure, args).await?;
                return Ok(());
            }

            self.perform_tail_call_tco_from_stack_args(closure, args_start, callee_idx)?;
        } else {
            match callee {
                VmValue::String(name) => {
                    self.call_named_value_from_stack_args(&name, args_start, callee_idx, None)
                        .await?;
                }
                callable => {
                    self.call_exact_value_from_stack_args(callable, args_start, callee_idx)
                        .await?;
                }
            }
        }
        Ok(())
    }

    pub(super) fn execute_return(&mut self) -> VmError {
        let val = self.pop().unwrap_or(VmValue::Nil);
        VmError::Return(val)
    }

    pub(super) fn execute_closure(&mut self) {
        self.sync_current_frame_locals_to_env();
        let frame = self.frames.last_mut().unwrap();
        let fn_idx = frame.chunk.read_u16(frame.ip) as usize;
        frame.ip += 2;
        let func = frame.chunk.functions[fn_idx].clone();
        let closure = VmClosure {
            func,
            env: self.env.clone(),
            source_dir: None,
            module_functions: self
                .frames
                .last()
                .and_then(|frame| frame.module_functions.as_ref().map(Arc::downgrade)),
            // Inherit module state so closures created inside a module function
            // see and mutate the same module-level vars.
            module_state: self
                .frames
                .last()
                .and_then(|frame| frame.module_state.as_ref().map(Arc::downgrade)),
            retained_module_scope: None,
        };
        self.stack.push(VmValue::Closure(Arc::new(closure)));
    }

    pub(super) async fn execute_pipe(&mut self) -> Result<(), VmError> {
        let callable = self.pop()?;
        let args_start = self.stack_arg_start(1)?;
        match callable {
            VmValue::Closure(closure) => {
                self.call_user_closure_from_stack_args(closure, args_start, args_start)
                    .await?;
            }
            VmValue::String(name) => {
                self.call_named_value_from_stack_args(&name, args_start, args_start, None)
                    .await?;
            }
            VmValue::Dict(registry) => {
                let closure = match crate::vm::tool_callable::require_single_harn_tool_handler(
                    &registry,
                    || {
                        format!(
                            "cannot pipe into {}",
                            VmValue::Dict(registry.clone()).type_name()
                        )
                    },
                ) {
                    Ok(closure) => closure,
                    Err(error) => {
                        self.stack.truncate(args_start);
                        return Err(error);
                    }
                };
                self.call_user_closure_from_stack_args(closure, args_start, args_start)
                    .await?;
            }
            VmValue::BuiltinRef(name) => {
                self.call_exact_value_from_stack_args(
                    VmValue::BuiltinRef(name),
                    args_start,
                    args_start,
                )
                .await?;
            }
            VmValue::BuiltinRefId(r) => {
                self.call_exact_value_from_stack_args(
                    VmValue::BuiltinRefId(r),
                    args_start,
                    args_start,
                )
                .await?;
            }
            _ => {
                self.stack.truncate(args_start);
                return Err(VmError::TypeError(format!(
                    "cannot pipe into {}",
                    callable.type_name()
                )));
            }
        }
        Ok(())
    }
}
