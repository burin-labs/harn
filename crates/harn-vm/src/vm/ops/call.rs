use std::rc::Rc;

use crate::chunk::{InlineCacheEntry, MethodCacheTarget};
use crate::orchestration::HookEvent;
use crate::value::{VmClosure, VmError, VmValue};
use crate::BuiltinId;

use super::super::CallFrame;

enum StepPreHookAction {
    Allow(Vec<VmValue>),
    Deny(String),
}

impl super::super::Vm {
    fn step_hook_payload(
        event: HookEvent,
        persona: Option<&str>,
        step_name: &str,
        function_name: &str,
        args: &[VmValue],
        output: Option<VmValue>,
    ) -> VmValue {
        let mut step = std::collections::BTreeMap::new();
        step.insert("name".to_string(), VmValue::String(Rc::from(step_name)));
        step.insert(
            "function".to_string(),
            VmValue::String(Rc::from(function_name)),
        );
        step.insert("args".to_string(), VmValue::List(Rc::new(args.to_vec())));
        let mut payload = std::collections::BTreeMap::new();
        payload.insert(
            "event".to_string(),
            VmValue::String(Rc::from(event.as_str())),
        );
        payload.insert(
            "target".to_string(),
            VmValue::String(Rc::from(match persona {
                Some(persona) if !persona.is_empty() => format!("{persona}.{step_name}"),
                _ => step_name.to_string(),
            })),
        );
        payload.insert(
            "persona".to_string(),
            VmValue::String(Rc::from(persona.unwrap_or(""))),
        );
        payload.insert("step".to_string(), VmValue::Dict(Rc::new(step)));
        if let Some(output) = output {
            payload.insert("output".to_string(), output);
        }
        VmValue::Dict(Rc::new(payload))
    }

    fn parse_step_pre_hook_result(
        value: VmValue,
        current_args: Vec<VmValue>,
    ) -> Result<StepPreHookAction, VmError> {
        match value {
            VmValue::Nil => Ok(StepPreHookAction::Allow(current_args)),
            VmValue::String(text) if text.as_ref() == "Allow" => {
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
                    return Ok(StepPreHookAction::Allow((**args).clone()));
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
            VmValue::String(text) if text.as_ref() == "Pass" => Ok(current_output),
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
            let raw = self.call_lifecycle_hook(&hook.closure, payload).await?;
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
        VmError::Thrown(VmValue::Dict(Rc::new(std::collections::BTreeMap::from([
            (
                "category".to_string(),
                VmValue::String(Rc::from("hook_denied")),
            ),
            ("event".to_string(), VmValue::String(Rc::from("PreStep"))),
            ("reason".to_string(), VmValue::String(Rc::from(reason))),
            (
                "step".to_string(),
                VmValue::String(Rc::from(step_name.to_string())),
            ),
        ]))))
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
                let raw = self.call_lifecycle_hook(&hook.closure, payload).await?;
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

    async fn call_user_closure(
        &mut self,
        closure: Rc<VmClosure>,
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

    async fn call_lifecycle_hook(
        &mut self,
        handler: &Rc<VmClosure>,
        payload: VmValue,
    ) -> Result<VmValue, VmError> {
        let snapshot = crate::step_runtime::take_active_context();
        let result = self.call_closure(handler, &[payload]).await;
        crate::step_runtime::restore_active_context(snapshot);
        result
    }

    fn try_cached_method(
        cache: &InlineCacheEntry,
        name_idx: u16,
        argc: usize,
        obj: &VmValue,
    ) -> Option<VmValue> {
        let InlineCacheEntry::Method {
            name_idx: cached_name_idx,
            argc: cached_argc,
            target,
        } = cache
        else {
            return None;
        };
        if *cached_name_idx != name_idx || *cached_argc != argc {
            return None;
        }

        match (target, obj) {
            (MethodCacheTarget::ListCount, VmValue::List(items)) => {
                Some(VmValue::Int(items.len() as i64))
            }
            (MethodCacheTarget::ListEmpty, VmValue::List(items)) => {
                Some(VmValue::Bool(items.is_empty()))
            }
            (MethodCacheTarget::StringCount, VmValue::String(s)) => {
                Some(VmValue::Int(s.chars().count() as i64))
            }
            (MethodCacheTarget::StringEmpty, VmValue::String(s)) => {
                Some(VmValue::Bool(s.is_empty()))
            }
            (MethodCacheTarget::DictCount, VmValue::Dict(map)) => {
                Some(VmValue::Int(map.len() as i64))
            }
            (MethodCacheTarget::RangeCount | MethodCacheTarget::RangeLen, VmValue::Range(r)) => {
                Some(VmValue::Int(r.len()))
            }
            (MethodCacheTarget::RangeEmpty, VmValue::Range(r)) => Some(VmValue::Bool(r.is_empty())),
            (MethodCacheTarget::RangeFirst, VmValue::Range(r)) => {
                Some(r.first().map(VmValue::Int).unwrap_or(VmValue::Nil))
            }
            (MethodCacheTarget::RangeLast, VmValue::Range(r)) => {
                Some(r.last().map(VmValue::Int).unwrap_or(VmValue::Nil))
            }
            (MethodCacheTarget::SetCount | MethodCacheTarget::SetLen, VmValue::Set(items)) => {
                Some(VmValue::Int(items.len() as i64))
            }
            (MethodCacheTarget::SetEmpty, VmValue::Set(items)) => {
                Some(VmValue::Bool(items.is_empty()))
            }
            _ => None,
        }
    }

    fn method_cache_target(obj: &VmValue, method: &str, argc: usize) -> Option<MethodCacheTarget> {
        if argc != 0 {
            return None;
        }
        match obj {
            VmValue::List(_) => match method {
                "count" => Some(MethodCacheTarget::ListCount),
                "empty" => Some(MethodCacheTarget::ListEmpty),
                _ => None,
            },
            VmValue::String(_) => match method {
                "count" | "len" => Some(MethodCacheTarget::StringCount),
                "empty" => Some(MethodCacheTarget::StringEmpty),
                _ => None,
            },
            VmValue::Dict(_) => match method {
                "count" => Some(MethodCacheTarget::DictCount),
                _ => None,
            },
            VmValue::Range(_) => match method {
                "count" => Some(MethodCacheTarget::RangeCount),
                "len" => Some(MethodCacheTarget::RangeLen),
                "empty" => Some(MethodCacheTarget::RangeEmpty),
                "first" => Some(MethodCacheTarget::RangeFirst),
                "last" => Some(MethodCacheTarget::RangeLast),
                _ => None,
            },
            VmValue::Set(_) => match method {
                "count" => Some(MethodCacheTarget::SetCount),
                "len" => Some(MethodCacheTarget::SetLen),
                "empty" => Some(MethodCacheTarget::SetEmpty),
                _ => None,
            },
            _ => None,
        }
    }

    async fn try_call_special_name(
        &mut self,
        name: &str,
        args: &[VmValue],
    ) -> Result<bool, VmError> {
        if name == "await" {
            crate::typecheck::validate_builtin_call(name, args, None)?;
            let task_id = args.first().and_then(|a| match a {
                VmValue::TaskHandle(id) => Some(id.clone()),
                _ => None,
            });
            if let Some(id) = task_id {
                if let Some(handle) = self.spawned_tasks.remove(&id) {
                    let (result, task_output) = handle
                        .handle
                        .await
                        .map_err(|e| VmError::Runtime(format!("Task join error: {e}")))??;
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
                if let Some(handle) = self.spawned_tasks.remove(id) {
                    handle.handle.abort();
                }
            }
            self.stack.push(VmValue::Nil);
            return Ok(true);
        }

        if name == "cancel_graceful" {
            crate::typecheck::validate_builtin_call(name, args, None)?;
            let task_id = args.first().and_then(|a| match a {
                VmValue::TaskHandle(id) => Some(id.clone()),
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
                                        vec![VmValue::String(Rc::from(e.to_string()))],
                                    ));
                                }
                                Err(e) => {
                                    self.stack.push(VmValue::enum_variant(
                                        "Result",
                                        "Err",
                                        vec![VmValue::String(Rc::from(format!("Task join error: {e}")))],
                                    ));
                                }
                            }
                        }
                        _ = &mut timeout => {
                            handle.abort();
                            self.stack.push(VmValue::enum_variant(
                                "Result",
                                "Err",
                                vec![VmValue::String(Rc::from(
                                    "cancel_graceful: timeout, task forcefully aborted",
                                ))],
                            ));
                        }
                    }
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

    async fn call_named_value(
        &mut self,
        name: &str,
        args: Vec<VmValue>,
        direct_id: Option<BuiltinId>,
    ) -> Result<(), VmError> {
        if self.try_call_special_name(name, &args).await? {
            return Ok(());
        }
        if let Some(closure) = self.resolve_named_closure(name) {
            self.call_user_closure(closure, args).await?;
        } else {
            let result = if let Some(id) = direct_id {
                self.call_builtin_id_or_name(id, name, args).await?
            } else {
                self.call_named_builtin(name, args).await?
            };
            self.stack.push(result);
        }
        Ok(())
    }

    pub(super) async fn execute_call(&mut self) -> Result<(), VmError> {
        let frame = self.frames.last_mut().unwrap();
        let argc = frame.chunk.code[frame.ip] as usize;
        frame.ip += 1;

        let args: Vec<VmValue> = self.stack.split_off(self.stack.len().saturating_sub(argc));
        let callee = self.pop()?;

        match callee {
            VmValue::String(name) => {
                self.call_named_value(&name, args, None).await?;
            }
            VmValue::Closure(closure) => {
                self.call_user_closure(closure, args).await?;
            }
            VmValue::BuiltinRef(name) => {
                self.call_named_value(&name, args, None).await?;
            }
            VmValue::BuiltinRefId { id, name } => {
                self.call_named_value(&name, args, Some(id)).await?;
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
            VmValue::BuiltinRef(name) => {
                self.call_named_value(&name, args, None).await?;
            }
            VmValue::BuiltinRefId { id, name } => {
                self.call_named_value(&name, args, Some(id)).await?;
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

    pub(super) async fn execute_call_builtin(&mut self) -> Result<(), VmError> {
        let frame = self.frames.last_mut().unwrap();
        let id = BuiltinId::from_raw(frame.chunk.read_u64(frame.ip));
        frame.ip += 8;
        let name_idx = frame.chunk.read_u16(frame.ip) as usize;
        frame.ip += 2;
        let argc = frame.chunk.code[frame.ip] as usize;
        frame.ip += 1;
        let name = Self::const_string(&frame.chunk.constants[name_idx])?;
        let args: Vec<VmValue> = self.stack.split_off(self.stack.len().saturating_sub(argc));
        self.call_named_value(&name, args, Some(id)).await
    }

    pub(super) async fn execute_call_builtin_spread(&mut self) -> Result<(), VmError> {
        let frame = self.frames.last_mut().unwrap();
        let id = BuiltinId::from_raw(frame.chunk.read_u64(frame.ip));
        frame.ip += 8;
        let name_idx = frame.chunk.read_u16(frame.ip) as usize;
        frame.ip += 2;
        let name = Self::const_string(&frame.chunk.constants[name_idx])?;
        let args_val = self.pop()?;
        let args = match args_val {
            VmValue::List(items) => (*items).clone(),
            _ => {
                return Err(VmError::TypeError(
                    "spread call requires list arguments".into(),
                ))
            }
        };
        self.call_named_value(&name, args, Some(id)).await
    }

    pub(super) async fn execute_tail_call(&mut self) -> Result<(), VmError> {
        let frame = self.frames.last_mut().unwrap();
        let argc = frame.chunk.code[frame.ip] as usize;
        frame.ip += 1;

        let args: Vec<VmValue> = self.stack.split_off(self.stack.len().saturating_sub(argc));
        let callee = self.pop()?;

        let resolved_closure = match &callee {
            VmValue::Closure(cl) => Some(Rc::clone(cl)),
            VmValue::String(name) => self.resolve_named_closure(name),
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
            {
                // Persona/step lifecycle state is frame-owned. Keep those
                // frames explicit so PreStep/PostStep hooks see the same
                // boundaries as a non-tail call.
                self.call_user_closure(closure, args).await?;
                return Ok(());
            }

            crate::typecheck::validate_user_call(&closure.func, &args, None)?;
            if closure.func.is_generator {
                // Generators cannot be tail-call optimized.
                let gen = self.create_generator(&closure, &args);
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

            self.stack.truncate(stack_base);

            let saved_source_dir = if let Some(ref dir) = closure.source_dir {
                let prev = crate::stdlib::process::VM_SOURCE_DIR.with(|sd| sd.borrow().clone());
                crate::stdlib::set_thread_source_dir(dir);
                prev
            } else {
                None
            };

            call_env.push_scope();
            let debugger = self.debugger_attached();
            let initial_env = if debugger {
                Some(call_env.clone())
            } else {
                None
            };
            self.env = call_env;
            let mut local_slots = Self::fresh_local_slots(&closure.func.chunk);
            Self::bind_param_slots(&mut local_slots, &closure.func, &args, false);
            let initial_local_slots = if debugger {
                Some(local_slots.clone())
            } else {
                None
            };

            // Inherit the popped frame's iterator depth so iterators
            // pushed by for-loops inside the caller (`return f(...)` from
            // inside `for x in xs { ... }`) get torn down when the
            // tail-called callee eventually returns, instead of leaking
            // into the caller's caller.
            let saved_iterator_depth = popped.saved_iterator_depth;
            self.iterators.truncate(saved_iterator_depth);
            let argc = args.len();
            self.frames.push(CallFrame {
                chunk: Rc::clone(&closure.func.chunk),
                ip: 0,
                stack_base,
                saved_env: parent_env,
                initial_env,
                initial_local_slots,
                saved_iterator_depth,
                fn_name: closure.func.name.clone(),
                argc,
                saved_source_dir,
                module_functions: closure.module_functions.clone(),
                module_state: closure.module_state.clone(),
                local_slots,
                local_scope_base: self.env.scope_depth().saturating_sub(1),
                local_scope_depth: 0,
            });
        } else {
            match callee {
                VmValue::String(name) => {
                    self.call_named_value(&name, args, None).await?;
                }
                _ => {
                    return Err(VmError::TypeError(format!(
                        "Cannot call {}",
                        callee.display()
                    )))
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
                .and_then(|frame| frame.module_functions.clone()),
            // Inherit module state so closures created inside a module function
            // see and mutate the same module-level vars.
            module_state: self
                .frames
                .last()
                .and_then(|frame| frame.module_state.clone()),
        };
        self.stack.push(VmValue::Closure(Rc::new(closure)));
    }

    pub(super) async fn execute_method_call(&mut self, optional: bool) -> Result<(), VmError> {
        let (name_idx, argc, cache_slot, cache_entry) = {
            let frame = self.frames.last_mut().unwrap();
            let op_offset = frame.ip.saturating_sub(1);
            let name_idx = frame.chunk.read_u16(frame.ip);
            frame.ip += 2;
            let argc = frame.chunk.code[frame.ip] as usize;
            frame.ip += 1;
            let cache_slot = frame.chunk.inline_cache_slot(op_offset);
            let cache_entry = cache_slot
                .map(|slot| frame.chunk.inline_cache_entry(slot))
                .unwrap_or(InlineCacheEntry::Empty);
            (name_idx, argc, cache_slot, cache_entry)
        };
        let args: Vec<VmValue> = self.stack.split_off(self.stack.len().saturating_sub(argc));
        let obj = self.pop()?;
        if optional && matches!(obj, VmValue::Nil) {
            self.stack.push(VmValue::Nil);
        } else if let Some(result) = Self::try_cached_method(&cache_entry, name_idx, argc, &obj) {
            self.stack.push(result);
        } else {
            let method = {
                let frame = self.frames.last().unwrap();
                Self::const_string(&frame.chunk.constants[name_idx as usize])?
            };
            let cache_target = Self::method_cache_target(&obj, &method, args.len());
            let result = self.call_method(obj, &method, &args).await?;
            if let (Some(slot), Some(target)) = (cache_slot, cache_target) {
                let frame = self.frames.last().unwrap();
                frame.chunk.set_inline_cache_entry(
                    slot,
                    InlineCacheEntry::Method {
                        name_idx,
                        argc,
                        target,
                    },
                );
            }
            self.stack.push(result);
        }
        Ok(())
    }

    pub(super) async fn execute_method_call_spread(&mut self) -> Result<(), VmError> {
        let (name_idx, cache_slot, cache_entry) = {
            let frame = self.frames.last_mut().unwrap();
            let op_offset = frame.ip.saturating_sub(1);
            let name_idx = frame.chunk.read_u16(frame.ip);
            frame.ip += 2;
            let cache_slot = frame.chunk.inline_cache_slot(op_offset);
            let cache_entry = cache_slot
                .map(|slot| frame.chunk.inline_cache_entry(slot))
                .unwrap_or(InlineCacheEntry::Empty);
            (name_idx, cache_slot, cache_entry)
        };
        let args_val = self.pop()?;
        let obj = self.pop()?;
        let args = match args_val {
            VmValue::List(items) => (*items).clone(),
            _ => {
                return Err(VmError::TypeError(
                    "spread method call requires list arguments".into(),
                ))
            }
        };
        if let Some(result) = Self::try_cached_method(&cache_entry, name_idx, args.len(), &obj) {
            self.stack.push(result);
        } else {
            let method = {
                let frame = self.frames.last().unwrap();
                Self::const_string(&frame.chunk.constants[name_idx as usize])?
            };
            let cache_target = Self::method_cache_target(&obj, &method, args.len());
            let result = self.call_method(obj, &method, &args).await?;
            if let (Some(slot), Some(target)) = (cache_slot, cache_target) {
                let frame = self.frames.last().unwrap();
                frame.chunk.set_inline_cache_entry(
                    slot,
                    InlineCacheEntry::Method {
                        name_idx,
                        argc: args.len(),
                        target,
                    },
                );
            }
            self.stack.push(result);
        }
        Ok(())
    }

    pub(super) async fn execute_pipe(&mut self) -> Result<(), VmError> {
        let callable = self.pop()?;
        let value = self.pop()?;
        match callable {
            VmValue::Closure(closure) => {
                self.call_user_closure(closure, vec![value]).await?;
            }
            VmValue::String(name) => {
                self.call_named_value(&name, vec![value], None).await?;
            }
            VmValue::BuiltinRef(name) => {
                self.call_named_value(&name, vec![value], None).await?;
            }
            VmValue::BuiltinRefId { id, name } => {
                self.call_named_value(&name, vec![value], Some(id)).await?;
            }
            _ => {
                return Err(VmError::TypeError(format!(
                    "cannot pipe into {}",
                    callable.type_name()
                )));
            }
        }
        Ok(())
    }
}
