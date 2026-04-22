use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;

use crate::value::{ErrorCategory, VmClosure, VmError, VmValue};
use crate::BuiltinId;

use super::async_builtin::CURRENT_ASYNC_BUILTIN_CHILD_VM;
use super::{ScopeSpan, Vm, VmBuiltinDispatch, VmBuiltinEntry};

impl Vm {
    fn index_builtin_id(&mut self, name: &str, dispatch: VmBuiltinDispatch) {
        let id = BuiltinId::from_name(name);
        if self.builtin_id_collisions.contains(&id) {
            return;
        }
        if let Some(existing) = self.builtins_by_id.get(&id) {
            if existing.name.as_ref() != name {
                self.builtins_by_id.remove(&id);
                self.builtin_id_collisions.insert(id);
                return;
            }
        }
        self.builtins_by_id.insert(
            id,
            VmBuiltinEntry {
                name: Rc::from(name),
                dispatch,
            },
        );
    }

    fn refresh_builtin_id(&mut self, name: &str) {
        if let Some(builtin) = self.builtins.get(name).cloned() {
            self.index_builtin_id(name, VmBuiltinDispatch::Sync(builtin));
        } else if let Some(async_builtin) = self.async_builtins.get(name).cloned() {
            self.index_builtin_id(name, VmBuiltinDispatch::Async(async_builtin));
        } else {
            let id = BuiltinId::from_name(name);
            if self
                .builtins_by_id
                .get(&id)
                .is_some_and(|entry| entry.name.as_ref() == name)
            {
                self.builtins_by_id.remove(&id);
            }
        }
    }

    /// Register a sync builtin function.
    pub fn register_builtin<F>(&mut self, name: &str, f: F)
    where
        F: Fn(&[VmValue], &mut String) -> Result<VmValue, VmError> + 'static,
    {
        self.builtins.insert(name.to_string(), Rc::new(f));
        self.refresh_builtin_id(name);
    }

    /// Remove a sync builtin (so an async version can take precedence).
    pub fn unregister_builtin(&mut self, name: &str) {
        self.builtins.remove(name);
        self.refresh_builtin_id(name);
    }

    /// Register an async builtin function.
    pub fn register_async_builtin<F, Fut>(&mut self, name: &str, f: F)
    where
        F: Fn(Vec<VmValue>) -> Fut + 'static,
        Fut: Future<Output = Result<VmValue, VmError>> + 'static,
    {
        self.async_builtins
            .insert(name.to_string(), Rc::new(move |args| Box::pin(f(args))));
        self.refresh_builtin_id(name);
    }

    pub(crate) fn registered_builtin_id(&self, name: &str) -> Option<BuiltinId> {
        let id = BuiltinId::from_name(name);
        if self
            .builtins_by_id
            .get(&id)
            .is_some_and(|entry| entry.name.as_ref() == name)
        {
            Some(id)
        } else {
            None
        }
    }

    /// Call a closure (used by method calls like .map/.filter etc.)
    /// Uses recursive execution for simplicity in method dispatch.
    pub(crate) fn call_closure<'a>(
        &'a mut self,
        closure: &'a VmClosure,
        args: &'a [VmValue],
    ) -> Pin<Box<dyn Future<Output = Result<VmValue, VmError>> + 'a>> {
        Box::pin(async move {
            let saved_env = self.env.clone();
            let mut call_env = self.closure_call_env_for_current_frame(closure);
            let saved_frames = std::mem::take(&mut self.frames);
            let saved_handlers = std::mem::take(&mut self.exception_handlers);
            let saved_iterators = std::mem::take(&mut self.iterators);
            let saved_deadlines = std::mem::take(&mut self.deadlines);

            call_env.push_scope();

            self.env = call_env;
            let argc = args.len();
            let mut local_slots = Self::fresh_local_slots(&closure.func.chunk);
            Self::bind_param_slots(&mut local_slots, &closure.func, args, false);
            let saved_source_dir = if let Some(ref dir) = closure.source_dir {
                let prev = crate::stdlib::process::VM_SOURCE_DIR.with(|sd| sd.borrow().clone());
                crate::stdlib::set_thread_source_dir(dir);
                prev
            } else {
                None
            };
            let result = self
                .run_chunk_ref(
                    Rc::clone(&closure.func.chunk),
                    argc,
                    saved_source_dir,
                    closure.module_functions.clone(),
                    closure.module_state.clone(),
                    Some(local_slots),
                )
                .await;

            self.env = saved_env;
            self.frames = saved_frames;
            self.exception_handlers = saved_handlers;
            self.iterators = saved_iterators;
            self.deadlines = saved_deadlines;

            result
        })
    }

    /// Invoke a value as a callable. Supports `VmValue::Closure` and
    /// `VmValue::BuiltinRef`, so builtin names passed by reference (e.g.
    /// `dict.rekey(snake_to_camel)`) dispatch through the same code path as
    /// user-defined closures.
    #[allow(clippy::manual_async_fn)]
    pub(crate) fn call_callable_value<'a>(
        &'a mut self,
        callable: &'a VmValue,
        args: &'a [VmValue],
    ) -> Pin<Box<dyn Future<Output = Result<VmValue, VmError>> + 'a>> {
        Box::pin(async move {
            match callable {
                VmValue::Closure(closure) => self.call_closure(closure, args).await,
                VmValue::BuiltinRef(name) => {
                    if let Some(result) = self.call_sync_builtin_by_ref(name, args) {
                        result
                    } else {
                        self.call_named_builtin(name, args.to_vec()).await
                    }
                }
                VmValue::BuiltinRefId { id, name } => {
                    self.call_builtin_id_or_name(*id, name, args.to_vec()).await
                }
                other => Err(VmError::TypeError(format!(
                    "expected callable, got {}",
                    other.type_name()
                ))),
            }
        })
    }

    fn call_sync_builtin_by_ref(
        &mut self,
        name: &str,
        args: &[VmValue],
    ) -> Option<Result<VmValue, VmError>> {
        let builtin = self.builtins.get(name).cloned()?;

        let span_kind = match name {
            "llm_call" | "llm_stream" | "agent_loop" => Some(crate::tracing::SpanKind::LlmCall),
            "mcp_call" => Some(crate::tracing::SpanKind::ToolCall),
            _ => None,
        };
        let _span = span_kind.map(|kind| ScopeSpan::new(kind, name.to_string()));

        if self.denied_builtins.contains(name) {
            return Some(Err(VmError::CategorizedError {
                message: format!("Tool '{}' is not permitted.", name),
                category: ErrorCategory::ToolRejected,
            }));
        }
        if let Err(err) = crate::orchestration::enforce_current_policy_for_builtin(name, args) {
            return Some(Err(err));
        }

        Some(builtin(args, &mut self.output))
    }

    /// Returns true if `v` is callable via `call_callable_value`.
    pub(crate) fn is_callable_value(v: &VmValue) -> bool {
        matches!(
            v,
            VmValue::Closure(_) | VmValue::BuiltinRef(_) | VmValue::BuiltinRefId { .. }
        )
    }

    /// Public wrapper for `call_closure`, used by the MCP server to invoke
    /// tool handler closures from outside the VM execution loop.
    pub async fn call_closure_pub(
        &mut self,
        closure: &VmClosure,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        self.call_closure(closure, args).await
    }

    /// Resolve a named builtin: sync builtins → async builtins → bridge → error.
    /// Used by Call, TailCall, and Pipe handlers to avoid duplicating this lookup.
    pub(crate) async fn call_named_builtin(
        &mut self,
        name: &str,
        args: Vec<VmValue>,
    ) -> Result<VmValue, VmError> {
        self.call_builtin_impl(name, args, None).await
    }

    pub(crate) async fn call_builtin_id_or_name(
        &mut self,
        id: BuiltinId,
        name: &str,
        args: Vec<VmValue>,
    ) -> Result<VmValue, VmError> {
        self.call_builtin_impl(name, args, Some(id)).await
    }

    async fn call_builtin_impl(
        &mut self,
        name: &str,
        args: Vec<VmValue>,
        direct_id: Option<BuiltinId>,
    ) -> Result<VmValue, VmError> {
        // Auto-trace LLM calls and tool calls.
        let span_kind = match name {
            "llm_call" | "llm_stream" | "agent_loop" => Some(crate::tracing::SpanKind::LlmCall),
            "mcp_call" => Some(crate::tracing::SpanKind::ToolCall),
            _ => None,
        };
        let _span = span_kind.map(|kind| ScopeSpan::new(kind, name.to_string()));

        // Sandbox check: deny builtins blocked by --deny/--allow flags.
        if self.denied_builtins.contains(name) {
            return Err(VmError::CategorizedError {
                message: format!("Tool '{}' is not permitted.", name),
                category: ErrorCategory::ToolRejected,
            });
        }
        crate::orchestration::enforce_current_policy_for_builtin(name, &args)?;

        if let Some(result) =
            crate::runtime_context::dispatch_runtime_context_builtin(self, name, &args)
        {
            return result;
        }

        if let Some(id) = direct_id {
            if let Some(entry) = self.builtins_by_id.get(&id).cloned() {
                if entry.name.as_ref() == name {
                    return self.call_builtin_entry(entry.dispatch, args).await;
                }
            }
        }

        if let Some(builtin) = self.builtins.get(name).cloned() {
            self.call_builtin_entry(VmBuiltinDispatch::Sync(builtin), args)
                .await
        } else if let Some(async_builtin) = self.async_builtins.get(name).cloned() {
            self.call_builtin_entry(VmBuiltinDispatch::Async(async_builtin), args)
                .await
        } else if let Some(bridge) = &self.bridge {
            crate::orchestration::enforce_current_policy_for_bridge_builtin(name)?;
            let args_json: Vec<serde_json::Value> =
                args.iter().map(crate::llm::vm_value_to_json).collect();
            let result = bridge
                .call(
                    "builtin_call",
                    serde_json::json!({"name": name, "args": args_json}),
                )
                .await?;
            Ok(crate::bridge::json_result_to_vm_value(&result))
        } else {
            let all_builtins = self
                .builtins
                .keys()
                .chain(self.async_builtins.keys())
                .map(|s| s.as_str());
            if let Some(suggestion) = crate::value::closest_match(name, all_builtins) {
                return Err(VmError::Runtime(format!(
                    "Undefined builtin: {name} (did you mean `{suggestion}`?)"
                )));
            }
            Err(VmError::UndefinedBuiltin(name.to_string()))
        }
    }

    async fn call_builtin_entry(
        &mut self,
        dispatch: VmBuiltinDispatch,
        args: Vec<VmValue>,
    ) -> Result<VmValue, VmError> {
        match dispatch {
            VmBuiltinDispatch::Sync(builtin) => builtin(&args, &mut self.output),
            VmBuiltinDispatch::Async(async_builtin) => {
                CURRENT_ASYNC_BUILTIN_CHILD_VM.with(|slot| {
                    slot.borrow_mut().push(self.child_vm());
                });
                let result = async_builtin(args).await;
                CURRENT_ASYNC_BUILTIN_CHILD_VM.with(|slot| {
                    slot.borrow_mut().pop();
                });
                result
            }
        }
    }
}
