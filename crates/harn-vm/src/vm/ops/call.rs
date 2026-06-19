use crate::value::VmDictExt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::chunk::{DirectCallState, DirectCallTarget, InlineCacheEntry, MethodCacheTarget};
use crate::orchestration::HookEvent;
use crate::value::{
    string_char_count, values_equal, DeadlockError, VmClosure, VmError, VmJoinHandle, VmTaskHandle,
    VmValue,
};
use crate::BuiltinId;

use super::super::{CallArgs, CallFrame};

const DIRECT_CALL_QUICKEN_THRESHOLD: u8 = 3;

struct AwaitingTask {
    task: Option<VmTaskHandle>,
}

impl AwaitingTask {
    fn new(task: VmTaskHandle) -> Self {
        Self { task: Some(task) }
    }

    fn handle_mut(&mut self) -> &mut VmJoinHandle {
        &mut self.task.as_mut().expect("awaiting task present").handle
    }

    fn disarm(mut self) {
        self.task = None;
    }
}

impl Drop for AwaitingTask {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.cancel_token
                .store(true, std::sync::atomic::Ordering::SeqCst);
            task.handle.abort();
        }
    }
}

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
        step.put_str("name", step_name);
        step.put_str("function", function_name);
        step.insert(
            "args".to_string(),
            VmValue::List(std::sync::Arc::new(args.to_vec())),
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

    async fn call_user_closure(
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
            let snapshot = crate::step_runtime::take_active_context();
            let result = self
                .call_closure_args(handler, CallArgs::One(&payload))
                .await;
            crate::step_runtime::restore_active_context(snapshot);
            result
        })
    }

    fn try_cached_method(
        cache: Option<(u16, usize, MethodCacheTarget)>,
        name_idx: u16,
        argc: usize,
        obj: &VmValue,
        args: &[VmValue],
    ) -> Option<VmValue> {
        let (cached_name_idx, cached_argc, target) = cache?;
        if cached_name_idx != name_idx || cached_argc != argc {
            return None;
        }

        match (target, obj) {
            (MethodCacheTarget::ListCount, VmValue::List(items)) => {
                Some(VmValue::Int(items.len() as i64))
            }
            (MethodCacheTarget::ListEmpty, VmValue::List(items)) => {
                Some(VmValue::Bool(items.is_empty()))
            }
            (MethodCacheTarget::ListContains, VmValue::List(items)) => {
                let needle = args.first().unwrap_or(&VmValue::Nil);
                Some(VmValue::Bool(items.iter().any(|v| values_equal(v, needle))))
            }
            (MethodCacheTarget::StringCount, VmValue::String(s)) => {
                Some(VmValue::Int(string_char_count(s) as i64))
            }
            (MethodCacheTarget::StringEmpty, VmValue::String(s)) => {
                Some(VmValue::Bool(s.is_empty()))
            }
            (MethodCacheTarget::StringContains, VmValue::String(s)) => Some(VmValue::Bool(
                s.contains(&*args.first().map(|arg| arg.display()).unwrap_or_default()),
            )),
            (MethodCacheTarget::DictCount, VmValue::Dict(map)) => {
                Some(VmValue::Int(map.len() as i64))
            }
            (MethodCacheTarget::DictHas, VmValue::Dict(map)) => Some(VmValue::Bool(
                map.contains_key(&args.first().map(|arg| arg.display()).unwrap_or_default()),
            )),
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
            (MethodCacheTarget::SetContains, VmValue::Set(items)) => {
                let needle = args.first().unwrap_or(&VmValue::Nil);
                Some(VmValue::Bool(items.iter().any(|v| values_equal(v, needle))))
            }
            _ => None,
        }
    }

    fn try_cached_harness_method(
        cache: Option<(u16, usize, MethodCacheTarget)>,
        name_idx: u16,
        argc: usize,
        obj: &VmValue,
    ) -> Option<Arc<crate::harness::VmHarness>> {
        let (cached_name_idx, cached_argc, target) = cache?;
        if cached_name_idx != name_idx || cached_argc != argc {
            return None;
        }
        let MethodCacheTarget::Harness(kind) = target else {
            return None;
        };
        let VmValue::Harness(handle) = obj else {
            return None;
        };
        if handle.kind() == kind {
            Some(Arc::clone(handle))
        } else {
            None
        }
    }

    fn try_harness_method_sync_fast(
        output: &mut String,
        obj: &VmValue,
        method: &str,
        args: &[VmValue],
    ) -> Option<Result<VmValue, VmError>> {
        let VmValue::Harness(handle) = obj else {
            return None;
        };
        Self::call_harness_method_sync_fast(output, handle, method, args)
    }

    fn method_cache_target(obj: &VmValue, method: &str, argc: usize) -> Option<MethodCacheTarget> {
        match obj {
            VmValue::Harness(handle) => Self::harness_sync_method_cache_target(handle, method),
            VmValue::List(_) => match (method, argc) {
                ("count", 0) => Some(MethodCacheTarget::ListCount),
                ("empty", 0) => Some(MethodCacheTarget::ListEmpty),
                ("contains", 1) => Some(MethodCacheTarget::ListContains),
                _ => None,
            },
            VmValue::String(_) => match (method, argc) {
                ("count" | "len", 0) => Some(MethodCacheTarget::StringCount),
                ("empty", 0) => Some(MethodCacheTarget::StringEmpty),
                ("contains", 1) => Some(MethodCacheTarget::StringContains),
                _ => None,
            },
            VmValue::Dict(_) => match (method, argc) {
                ("count", 0) => Some(MethodCacheTarget::DictCount),
                ("has", 1) => Some(MethodCacheTarget::DictHas),
                _ => None,
            },
            VmValue::Range(_) => match (method, argc) {
                ("count", 0) => Some(MethodCacheTarget::RangeCount),
                ("len", 0) => Some(MethodCacheTarget::RangeLen),
                ("empty", 0) => Some(MethodCacheTarget::RangeEmpty),
                ("first", 0) => Some(MethodCacheTarget::RangeFirst),
                ("last", 0) => Some(MethodCacheTarget::RangeLast),
                _ => None,
            },
            VmValue::Set(_) => match (method, argc) {
                ("count", 0) => Some(MethodCacheTarget::SetCount),
                ("len", 0) => Some(MethodCacheTarget::SetLen),
                ("empty", 0) => Some(MethodCacheTarget::SetEmpty),
                ("contains", 1) => Some(MethodCacheTarget::SetContains),
                _ => None,
            },
            _ => None,
        }
    }

    fn harness_sync_method_cache_target(
        handle: &crate::harness::VmHarness,
        method: &str,
    ) -> Option<MethodCacheTarget> {
        let cacheable = match handle.kind() {
            crate::harness::HarnessKind::Stdio => matches!(
                method,
                "print" | "println" | "eprint" | "eprintln" | "read_line" | "prompt"
            ),
            crate::harness::HarnessKind::Term => {
                matches!(method, "width" | "height" | "read_password")
            }
            crate::harness::HarnessKind::Clock => {
                matches!(method, "now_ms" | "timestamp" | "monotonic_ms" | "elapsed")
            }
            crate::harness::HarnessKind::Env => matches!(method, "get" | "get_or"),
            crate::harness::HarnessKind::Random => matches!(
                method,
                "gen_f64"
                    | "f64"
                    | "random"
                    | "gen_u64"
                    | "u64"
                    | "gen_range"
                    | "range"
                    | "random_int"
                    | "int"
                    | "choice"
                    | "random_choice"
                    | "shuffle"
                    | "random_shuffle"
            ),
            crate::harness::HarnessKind::Crypto => matches!(method, "sha256"),
            crate::harness::HarnessKind::Tenant => matches!(method, "id" | "try_id"),
            crate::harness::HarnessKind::Auth => matches!(
                method,
                "is_authenticated"
                    | "subject"
                    | "try_subject"
                    | "scheme"
                    | "try_scheme"
                    | "kind"
                    | "scopes"
                    | "has_scope"
            ),
            crate::harness::HarnessKind::Root
            | crate::harness::HarnessKind::Fs
            | crate::harness::HarnessKind::Net
            | crate::harness::HarnessKind::Process
            | crate::harness::HarnessKind::System
            | crate::harness::HarnessKind::Secrets
            | crate::harness::HarnessKind::Llm
            | crate::harness::HarnessKind::Obs => false,
        };
        cacheable.then_some(MethodCacheTarget::Harness(handle.kind()))
    }

    fn stack_arg_start(&self, argc: usize) -> Result<usize, VmError> {
        self.stack
            .len()
            .checked_sub(argc)
            .ok_or_else(|| VmError::Runtime("call argument stack underflow".to_string()))
    }

    fn take_stack_args_from(&mut self, args_start: usize) -> Result<Vec<VmValue>, VmError> {
        if args_start > self.stack.len() {
            return Err(VmError::Runtime(
                "call argument stack underflow".to_string(),
            ));
        }
        Ok(self.stack.drain(args_start..).collect())
    }

    fn is_special_name(name: &str) -> bool {
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

    async fn try_call_special_name(
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
                    let mut awaiting = AwaitingTask::new(handle);
                    let joined = awaiting
                        .handle_mut()
                        .await
                        .map_err(|e| VmError::Runtime(format!("Task join error: {e}")))??;
                    awaiting.disarm();
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
                if let Some(handle) = self.spawned_tasks.remove(id.as_str()) {
                    handle.handle.abort();
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
                            handle.abort();
                            self.stack.push(VmValue::enum_variant(
                                "Result",
                                "Err",
                                vec![VmValue::String(arcstr::ArcStr::from(
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

    async fn call_named_value_from_stack_args(
        &mut self,
        name: &str,
        args_start: usize,
        stack_truncate_to: usize,
        direct_id: Option<BuiltinId>,
    ) -> Result<(), VmError> {
        if stack_truncate_to > args_start || args_start > self.stack.len() {
            return Err(VmError::Runtime(
                "invalid call argument stack range".to_string(),
            ));
        }

        if Self::is_special_name(name) {
            let args = self.take_stack_args_from(args_start)?;
            self.stack.truncate(stack_truncate_to);
            return self.call_named_value(name, args, direct_id).await;
        }

        if let Some(closure) = self.resolve_named_closure(name) {
            if closure.func.is_generator
                || crate::step_runtime::step_definition_for_function(&closure.func.name).is_some()
            {
                let args = self.take_stack_args_from(args_start)?;
                self.stack.truncate(stack_truncate_to);
                self.call_user_closure(closure, args).await?;
            } else {
                self.push_closure_frame_from_stack_args(&closure, args_start, stack_truncate_to)?;
            }
            return Ok(());
        }

        if let Some(result) =
            self.try_call_sync_builtin_id_or_name_from_stack_args(direct_id, name, args_start)
        {
            self.stack.truncate(stack_truncate_to);
            self.stack.push(result?);
            return Ok(());
        }

        let args = self.take_stack_args_from(args_start)?;
        self.stack.truncate(stack_truncate_to);
        let result = if let Some(id) = direct_id {
            self.call_builtin_id_or_name(id, name, args).await?
        } else {
            self.call_named_builtin(name, args).await?
        };
        self.stack.push(result);
        Ok(())
    }

    /// Sync fast path for `Op::Call`. Peeks the callee before touching
    /// `ip`; regular user closures can enter a new frame directly from the
    /// existing stack argument slice. Anything that needs async handling
    /// leaves `ip` untouched so [`execute_call_async`] can read the operand
    /// exactly once.
    pub(super) fn execute_call_sync(&mut self) -> Option<Result<(), VmError>> {
        let (chunk, argc, cache_slot) = {
            let frame = self.frames.last().unwrap();
            let op_offset = frame.ip.saturating_sub(1);
            let chunk = Arc::clone(&frame.chunk);
            let argc = frame.chunk.code[frame.ip] as usize;
            let cache_slot = frame.chunk.inline_cache_slot(op_offset);
            (chunk, argc, cache_slot)
        };
        let cached_state = cache_slot.and_then(|slot| self.peek_direct_call_state(&chunk, slot));
        let callee_idx = self.stack.len().checked_sub(argc + 1)?;
        let closure = match self.try_cached_direct_call(cached_state.as_ref(), argc, callee_idx) {
            Some(closure) => closure,
            None => match self.stack.get(callee_idx)? {
                VmValue::Closure(c) => Arc::clone(c),
                _ => return None,
            },
        };
        if !Self::direct_call_cacheable(&closure) {
            return None;
        }

        if let Some(slot) = cache_slot {
            let next_entry = Self::next_direct_call_entry(
                cached_state,
                argc,
                DirectCallTarget::Closure(Arc::clone(&closure)),
            );
            self.set_inline_cache_entry(&chunk, slot, next_entry);
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
                if closure.func.is_generator
                    || crate::step_runtime::step_definition_for_function(&closure.func.name)
                        .is_some()
                {
                    let args = self.take_stack_args_from(args_start)?;
                    self.stack.truncate(callee_idx);
                    self.call_user_closure(closure, args).await?;
                } else {
                    self.push_closure_frame_from_stack_args(&closure, args_start, callee_idx)?;
                }
            }
            VmValue::BuiltinRef(name) => {
                self.call_named_value_from_stack_args(&name, args_start, callee_idx, None)
                    .await?;
            }
            VmValue::BuiltinRefId(r) => {
                self.call_named_value_from_stack_args(&r.name, args_start, callee_idx, Some(r.id))
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
            VmValue::BuiltinRef(name) => {
                self.call_named_value(&name, args, None).await?;
            }
            VmValue::BuiltinRefId(r) => {
                self.call_named_value(&r.name, args, Some(r.id)).await?;
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
        let (chunk, name_idx, argc, cache_slot) = {
            let frame = self.frames.last().unwrap();
            let op_offset = frame.ip.saturating_sub(1);
            let chunk = Arc::clone(&frame.chunk);
            let name_idx = frame.chunk.read_u16(frame.ip + 8) as usize;
            let argc = frame.chunk.code[frame.ip + 10] as usize;
            let cache_slot = frame.chunk.inline_cache_slot(op_offset);
            (chunk, name_idx, argc, cache_slot)
        };
        let cached_state = cache_slot.and_then(|slot| self.peek_direct_call_state(&chunk, slot));
        let name = Self::const_str(&chunk.constants[name_idx]).ok()?;

        // Names handled by `try_call_special_name` are runtime constructs
        // (`await`, `cancel`, ...) that must run on the async path even if
        // the user happens to define a function with the same name —
        // matching the async dispatcher's first-check semantics.
        if Self::is_special_name(name) {
            return None;
        }

        let closure = match self.try_cached_named_direct_call(cached_state.as_ref(), name, argc) {
            Some(closure) => closure,
            None => match self.resolve_named_closure(name) {
                Some(closure) => closure,
                // Not a user closure, so this bare call targets a builtin.
                // Most builtins are synchronous; dispatch them right here on
                // the sync path instead of bailing to
                // `execute_call_builtin_async`, which would spin up the async
                // state machine and re-run `resolve_named_closure` (a second
                // local-slot scan + env walk + module mutexes) only to reach
                // the same builtin. Async builtins return `None` and still take
                // the async path. Resolution semantics are unchanged: a user
                // `fn` of the same name is still found by `resolve_named_closure`
                // above and wins over the builtin.
                None => return self.try_dispatch_sync_builtin_inline(name, argc),
            },
        };
        if !Self::direct_call_cacheable(&closure) {
            return None;
        }

        if let Some(slot) = cache_slot {
            let next_entry = Self::next_direct_call_entry(
                cached_state,
                argc,
                DirectCallTarget::Closure(Arc::clone(&closure)),
            );
            self.set_inline_cache_entry(&chunk, slot, next_entry);
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
        let args_len = self.stack.len() - args_start;
        let args = &self.stack[args_start..];
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
        Self::bind_param_slots(
            &mut local_slots,
            &closure.func,
            &self.stack[args_start..],
            false,
        );
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
        self.frames.push(CallFrame {
            chunk: Arc::clone(&closure.func.chunk),
            ip: 0,
            stack_base,
            saved_env: parent_env,
            initial_env,
            initial_local_slots,
            saved_iterator_depth,
            fn_name: closure.func.name.clone(),
            argc: args_len,
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
                _ => {
                    let message = format!("Cannot call {}", callee.display());
                    self.stack.truncate(callee_idx);
                    return Err(VmError::TypeError(message));
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
        };
        self.stack.push(VmValue::Closure(Arc::new(closure)));
    }

    pub(super) async fn execute_method_call(&mut self, optional: bool) -> Result<(), VmError> {
        let (chunk, name_idx, argc, cache_slot) = {
            let frame = self.frames.last_mut().unwrap();
            let op_offset = frame.ip.saturating_sub(1);
            let chunk = Arc::clone(&frame.chunk);
            let name_idx = frame.chunk.read_u16(frame.ip);
            frame.ip += 2;
            let argc = frame.chunk.code[frame.ip] as usize;
            frame.ip += 1;
            let cache_slot = frame.chunk.inline_cache_slot(op_offset);
            (chunk, name_idx, argc, cache_slot)
        };
        let cached_method = cache_slot.and_then(|slot| self.peek_method_cache(&chunk, slot));
        let args_start = self.stack_arg_start(argc)?;
        let obj_idx = args_start
            .checked_sub(1)
            .ok_or_else(|| VmError::Runtime("method receiver stack underflow".to_string()))?;
        let obj = self
            .stack
            .get(obj_idx)
            .cloned()
            .ok_or_else(|| VmError::Runtime("method receiver stack underflow".to_string()))?;
        if optional && matches!(obj, VmValue::Nil) {
            self.stack.truncate(obj_idx);
            self.stack.push(VmValue::Nil);
        } else if let Some(result) = Self::try_cached_method(
            cached_method,
            name_idx,
            argc,
            &obj,
            &self.stack[args_start..],
        ) {
            self.stack.truncate(obj_idx);
            self.stack.push(result);
        } else if let Some(handle) =
            Self::try_cached_harness_method(cached_method, name_idx, argc, &obj)
        {
            let method = Self::const_str(&chunk.constants[name_idx as usize])?;
            let args = self.take_stack_args_from(args_start)?;
            self.stack.truncate(obj_idx);
            let result = if let Some(result) =
                Self::call_harness_method_sync_fast(&mut self.output, &handle, method, &args)
            {
                result?
            } else {
                self.call_method_async(obj, method, &args).await?
            };
            self.stack.push(result);
        } else {
            let method = Self::const_str(&chunk.constants[name_idx as usize])?;
            let cache_target = Self::method_cache_target(&obj, method, argc);
            let args = &self.stack[args_start..];
            let result = if let Some(result) =
                Self::try_harness_method_sync_fast(&mut self.output, &obj, method, args)
            {
                self.stack.truncate(obj_idx);
                result?
            } else if let Some(result) = Self::call_method_sync(&obj, method, args) {
                self.stack.truncate(obj_idx);
                result?
            } else {
                let args = self.take_stack_args_from(args_start)?;
                self.stack.truncate(obj_idx);
                self.call_method_async(obj, method, &args).await?
            };
            if let (Some(slot), Some(target)) = (cache_slot, cache_target) {
                self.set_inline_cache_entry(
                    &chunk,
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

    /// Completes method calls that do not need to suspend. Returning `None`
    /// leaves the frame and operand stack untouched for the async fallback.
    pub(super) fn execute_method_call_sync(
        &mut self,
        optional: bool,
    ) -> Option<Result<(), VmError>> {
        let (chunk, name_idx, argc, cache_slot) = {
            let frame = self.frames.last().unwrap();
            let op_offset = frame.ip.saturating_sub(1);
            let chunk = Arc::clone(&frame.chunk);
            let name_idx = frame.chunk.read_u16(frame.ip);
            let argc = frame.chunk.code[frame.ip + 2] as usize;
            let cache_slot = frame.chunk.inline_cache_slot(op_offset);
            (chunk, name_idx, argc, cache_slot)
        };
        let cached_method = cache_slot.and_then(|slot| self.peek_method_cache(&chunk, slot));

        let args_start = match self.stack.len().checked_sub(argc) {
            Some(args_start) => args_start,
            None => return Some(Err(VmError::StackUnderflow)),
        };
        let obj_idx = match args_start.checked_sub(1) {
            Some(obj_idx) => obj_idx,
            None => return Some(Err(VmError::StackUnderflow)),
        };
        let obj = &self.stack[obj_idx];

        if optional && matches!(obj, VmValue::Nil) {
            return Some(self.finish_method_call_sync(
                &chunk,
                argc,
                Ok(VmValue::Nil),
                name_idx,
                None,
                None,
            ));
        }

        if let Some(result) = Self::try_cached_method(
            cached_method,
            name_idx,
            argc,
            obj,
            &self.stack[args_start..],
        ) {
            return Some(self.finish_method_call_sync(
                &chunk,
                argc,
                Ok(result),
                name_idx,
                None,
                None,
            ));
        }

        if let Some(handle) = Self::try_cached_harness_method(cached_method, name_idx, argc, obj) {
            let method = match Self::const_str(&chunk.constants[name_idx as usize]) {
                Ok(method) => method,
                Err(err) => {
                    return Some(self.finish_method_call_sync(
                        &chunk,
                        argc,
                        Err(err),
                        name_idx,
                        None,
                        None,
                    ))
                }
            };
            if let Some(result) = Self::call_harness_method_sync_fast(
                &mut self.output,
                &handle,
                method,
                &self.stack[args_start..],
            ) {
                return Some(
                    self.finish_method_call_sync(&chunk, argc, result, name_idx, None, None),
                );
            }
        }

        let (result, cache_target) = {
            let method = match Self::const_str(&chunk.constants[name_idx as usize]) {
                Ok(method) => method,
                Err(err) => {
                    return Some(self.finish_method_call_sync(
                        &chunk,
                        argc,
                        Err(err),
                        name_idx,
                        None,
                        None,
                    ))
                }
            };
            let args = &self.stack[args_start..];
            let result = if let Some(result) =
                Self::try_harness_method_sync_fast(&mut self.output, obj, method, args)
            {
                result
            } else {
                Self::call_method_sync(obj, method, args)?
            };
            let cache_target = Self::method_cache_target(obj, method, argc);
            (result, cache_target)
        };

        Some(self.finish_method_call_sync(&chunk, argc, result, name_idx, cache_slot, cache_target))
    }

    fn finish_method_call_sync(
        &mut self,
        chunk: &crate::chunk::ChunkRef,
        argc: usize,
        result: Result<VmValue, VmError>,
        name_idx: u16,
        cache_slot: Option<usize>,
        cache_target: Option<MethodCacheTarget>,
    ) -> Result<(), VmError> {
        let frame = self.frames.last_mut().unwrap();
        frame.ip += 3;

        let obj_idx = self
            .stack
            .len()
            .checked_sub(argc + 1)
            .ok_or(VmError::StackUnderflow)?;
        self.stack.truncate(obj_idx);

        let result = result?;
        if let (Some(slot), Some(target)) = (cache_slot, cache_target) {
            self.set_inline_cache_entry(
                chunk,
                slot,
                InlineCacheEntry::Method {
                    name_idx,
                    argc,
                    target,
                },
            );
        }
        self.stack.push(result);
        Ok(())
    }

    pub(super) async fn execute_method_call_spread(&mut self) -> Result<(), VmError> {
        let (chunk, name_idx, cache_slot) = {
            let frame = self.frames.last_mut().unwrap();
            let op_offset = frame.ip.saturating_sub(1);
            let chunk = Arc::clone(&frame.chunk);
            let name_idx = frame.chunk.read_u16(frame.ip);
            frame.ip += 2;
            let cache_slot = frame.chunk.inline_cache_slot(op_offset);
            (chunk, name_idx, cache_slot)
        };
        let cached_method = cache_slot.and_then(|slot| self.peek_method_cache(&chunk, slot));
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
        if let Some(result) =
            Self::try_cached_method(cached_method, name_idx, args.len(), &obj, &args)
        {
            self.stack.push(result);
        } else if let Some(handle) =
            Self::try_cached_harness_method(cached_method, name_idx, args.len(), &obj)
        {
            let method = Self::const_str(&chunk.constants[name_idx as usize])?;
            let result = if let Some(result) =
                Self::call_harness_method_sync_fast(&mut self.output, &handle, method, &args)
            {
                result?
            } else {
                self.call_method_async(obj, method, &args).await?
            };
            self.stack.push(result);
        } else {
            let method = Self::const_str(&chunk.constants[name_idx as usize])?;
            let cache_target = Self::method_cache_target(&obj, method, args.len());
            let result = if let Some(result) =
                Self::try_harness_method_sync_fast(&mut self.output, &obj, method, &args)
            {
                result?
            } else if let Some(result) = Self::call_method_sync(&obj, method, &args) {
                result?
            } else {
                self.call_method_async(obj, method, &args).await?
            };
            if let (Some(slot), Some(target)) = (cache_slot, cache_target) {
                self.set_inline_cache_entry(
                    &chunk,
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
        let args_start = self.stack_arg_start(1)?;
        match callable {
            VmValue::Closure(closure) => {
                if closure.func.is_generator
                    || crate::step_runtime::step_definition_for_function(&closure.func.name)
                        .is_some()
                {
                    let args = self.take_stack_args_from(args_start)?;
                    self.call_user_closure(closure, args).await?;
                } else {
                    self.push_closure_frame_from_stack_args(&closure, args_start, args_start)?;
                }
            }
            VmValue::String(name) => {
                self.call_named_value_from_stack_args(&name, args_start, args_start, None)
                    .await?;
            }
            VmValue::BuiltinRef(name) => {
                self.call_named_value_from_stack_args(&name, args_start, args_start, None)
                    .await?;
            }
            VmValue::BuiltinRefId(r) => {
                self.call_named_value_from_stack_args(&r.name, args_start, args_start, Some(r.id))
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
