use std::future::Future;
use std::sync::Arc;

use crate::value::{ErrorCategory, VmBuiltinFn, VmClosure, VmError, VmValue};
use crate::BuiltinId;

use super::{
    CallArgs, ScopeSpan, Vm, VmBuiltinArity, VmBuiltinDispatch, VmBuiltinEntry, VmBuiltinKind,
    VmBuiltinMetadata,
};

impl Vm {
    fn builtin_span_kind(name: &str) -> Option<crate::tracing::SpanKind> {
        match name {
            "llm_call" | "llm_stream" | "llm_stream_call" | "agent_loop" | "agent_turn" => {
                Some(crate::tracing::SpanKind::LlmCall)
            }
            "mcp_call" => Some(crate::tracing::SpanKind::ToolCall),
            _ => None,
        }
    }

    fn is_runtime_context_builtin(name: &str) -> bool {
        matches!(
            name,
            "runtime_context"
                | "task_current"
                | "runtime_context_values"
                | "runtime_context_get"
                | "runtime_context_set"
                | "runtime_context_clear"
        )
    }

    fn resolve_sync_builtin_id_or_name(
        &self,
        direct_id: Option<BuiltinId>,
        name: &str,
    ) -> Option<Result<VmBuiltinFn, VmError>> {
        if crate::autonomy::needs_async_side_effect_enforcement(name)
            || Self::is_runtime_context_builtin(name)
        {
            return None;
        }

        let dispatch = if let Some(id) = direct_id {
            self.builtins_by_id
                .get(&id)
                .filter(|entry| entry.name.as_ref() == name)
                .map(|entry| entry.dispatch.clone())
        } else {
            None
        }
        .or_else(|| {
            self.builtins
                .get(name)
                .cloned()
                .map(VmBuiltinDispatch::Sync)
        });

        let Some(dispatch) = dispatch else {
            if self.async_builtins.contains_key(name) || self.bridge.is_some() {
                return None;
            }
            let all_builtins = self
                .builtins
                .keys()
                .chain(self.async_builtins.keys())
                .map(|s| s.as_str());
            return Some(
                if let Some(suggestion) = crate::value::closest_match(name, all_builtins) {
                    Err(VmError::Runtime(format!(
                        "Undefined builtin: {name} (did you mean `{suggestion}`?)"
                    )))
                } else {
                    Err(VmError::UndefinedBuiltin(name.to_string()))
                },
            );
        };

        match dispatch {
            VmBuiltinDispatch::Sync(builtin) => Some(Ok(builtin)),
            VmBuiltinDispatch::Async(_) => None,
        }
    }

    fn validate_sync_builtin_args(&self, name: &str, args: &[VmValue]) -> Result<(), VmError> {
        if self.denied_builtins.contains(name) {
            return Err(VmError::CategorizedError {
                message: format!("Tool '{name}' is not permitted."),
                category: ErrorCategory::ToolRejected,
            });
        }
        crate::orchestration::enforce_current_policy_for_builtin(name, args)?;
        crate::typecheck::validate_builtin_call(name, args, None)
    }

    fn index_builtin_id(&mut self, name: &str, dispatch: VmBuiltinDispatch) {
        let id = BuiltinId::from_name(name);
        if self.builtin_id_collisions.contains(&id) {
            return;
        }
        if let Some(existing) = self.builtins_by_id.get(&id) {
            if existing.name.as_ref() != name {
                Arc::make_mut(&mut self.builtins_by_id).remove(&id);
                Arc::make_mut(&mut self.builtin_id_collisions).insert(id);
                return;
            }
        }
        Arc::make_mut(&mut self.builtins_by_id).insert(
            id,
            VmBuiltinEntry {
                name: std::sync::Arc::from(name),
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
                Arc::make_mut(&mut self.builtins_by_id).remove(&id);
            }
        }
    }

    /// Register a sync builtin function.
    pub fn register_builtin<F>(&mut self, name: &str, f: F)
    where
        F: Fn(&[VmValue], &mut String) -> Result<VmValue, VmError> + Send + Sync + 'static,
    {
        Arc::make_mut(&mut self.builtins).insert(name.to_string(), Arc::new(f));
        Arc::make_mut(&mut self.builtin_metadata)
            .insert(name.to_string(), VmBuiltinMetadata::sync(name.to_string()));
        self.refresh_builtin_id(name);
    }

    /// Register a sync builtin function with discoverable metadata.
    pub fn register_builtin_with_metadata<F>(&mut self, metadata: VmBuiltinMetadata, f: F)
    where
        F: Fn(&[VmValue], &mut String) -> Result<VmValue, VmError> + Send + Sync + 'static,
    {
        let name = metadata.name().to_string();
        Arc::make_mut(&mut self.builtins).insert(name.clone(), Arc::new(f));
        Arc::make_mut(&mut self.builtin_metadata)
            .insert(name.clone(), metadata.with_kind(VmBuiltinKind::Sync));
        self.refresh_builtin_id(&name);
    }

    /// Register a `VmBuiltinDef` (the shape emitted by `#[harn_builtin]`).
    /// Registers the primary name plus each declared alias, sharing the
    /// same handler. `runtime_only` defs skip the parser-side publish (the
    /// vm-side registration still happens). `parser_only` defs skip the
    /// vm-side registration entirely (handler is `None`).
    pub fn register_builtin_def(&mut self, def: &'static crate::stdlib::macros::VmBuiltinDef) {
        use crate::stdlib::macros::VmBuiltinHandler;
        if def.parser_only {
            return;
        }
        // Derive arity from the parsed `BuiltinSignature` so the discoverable
        // metadata layer (harn explain, alignment-test metadata check) keeps
        // parity with the pre-macro DSL builder.
        let arity = arity_from_sig(&def.sig);
        let names = std::iter::once(def.sig.name).chain(def.aliases.iter().copied());
        for name in names {
            match def.handler {
                VmBuiltinHandler::Sync(f) => {
                    let meta = builtin_def_metadata(def, name, arity, VmBuiltinKind::Sync);
                    self.register_builtin_with_metadata(meta, f);
                }
                VmBuiltinHandler::Async(f) => {
                    let meta = builtin_def_metadata(def, name, arity, VmBuiltinKind::Async);
                    // Wrap the function pointer that already returns an
                    // AsyncBuiltinFuture so register_async_builtin_with_metadata's
                    // generic handler/future bounds are met.
                    self.register_async_builtin_with_metadata(meta, f);
                }
                VmBuiltinHandler::None => {
                    // Parser-only, but reached here despite parser_only=false.
                    // This is a configuration bug.
                    panic!(
                        "VmBuiltinHandler::None for {name:?} without parser_only=true \
                         on its BuiltinDef"
                    );
                }
            }
        }
    }

    /// Remove a sync builtin (so an async version can take precedence).
    pub fn unregister_builtin(&mut self, name: &str) {
        Arc::make_mut(&mut self.builtins).remove(name);
        if self.async_builtins.contains_key(name) {
            Arc::make_mut(&mut self.builtin_metadata).insert(
                name.to_string(),
                VmBuiltinMetadata::async_builtin(name.to_string()),
            );
        } else {
            Arc::make_mut(&mut self.builtin_metadata).remove(name);
        }
        self.refresh_builtin_id(name);
    }

    /// Register an async builtin function. The handler receives the explicit
    /// [`crate::vm::AsyncBuiltinCtx`] threaded by the dispatch loop.
    pub fn register_async_builtin<F, Fut>(&mut self, name: &str, f: F)
    where
        F: Fn(crate::vm::AsyncBuiltinCtx, Vec<VmValue>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<VmValue, VmError>> + Send + 'static,
    {
        Arc::make_mut(&mut self.async_builtins).insert(
            name.to_string(),
            Arc::new(move |ctx, args| Box::pin(f(ctx, args))),
        );
        Arc::make_mut(&mut self.builtin_metadata).insert(
            name.to_string(),
            VmBuiltinMetadata::async_builtin(name.to_string()),
        );
        self.refresh_builtin_id(name);
    }

    /// Register an async builtin function with discoverable metadata. The
    /// handler receives the explicit [`crate::vm::AsyncBuiltinCtx`].
    pub fn register_async_builtin_with_metadata<F, Fut>(
        &mut self,
        metadata: VmBuiltinMetadata,
        f: F,
    ) where
        F: Fn(crate::vm::AsyncBuiltinCtx, Vec<VmValue>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<VmValue, VmError>> + Send + 'static,
    {
        let name = metadata.name().to_string();
        Arc::make_mut(&mut self.async_builtins).insert(
            name.clone(),
            Arc::new(move |ctx, args| Box::pin(f(ctx, args))),
        );
        Arc::make_mut(&mut self.builtin_metadata)
            .insert(name.clone(), metadata.with_kind(VmBuiltinKind::Async));
        self.refresh_builtin_id(&name);
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

    /// Invoke a closure inline against the existing VM frame stack.
    ///
    /// Dispatch path for every callback-taking method on lists/dicts/sets
    /// (`.map`, `.filter`, `.reduce`, `.each`, `.sort_by`, …) via
    /// [`call_callable_value`]. The closure's frame is pushed onto
    /// `self.frames` using the same machinery as `Op::Call`, and the
    /// shared dispatch loop ([`Vm::drive_until_frame_depth`]) drains the
    /// sub-execution back to the caller's depth.
    ///
    /// This avoids the per-invocation `Pin<Box<dyn Future>>` heap
    /// allocation a recursive `async fn` would require — the recursion
    /// cycle (closure → `.map` → callback → closure) is broken instead at
    /// [`Vm::call_method`], which keeps a single boxed future per
    /// method-call site rather than per callback element.
    ///
    /// Exception handlers are saved and cleared before the sub-execution
    /// so an unhandled throw inside the body propagates as a Rust
    /// `Result::Err` to the caller's dispatch loop. Iterators, deadlines,
    /// and frames are scoped by `CallFrame::saved_iterator_depth` and the
    /// per-frame deadline tags.
    pub(crate) async fn call_closure(
        &mut self,
        closure: &VmClosure,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        self.call_closure_args(closure, CallArgs::Slice(args)).await
    }

    pub(crate) async fn call_closure_args(
        &mut self,
        closure: &VmClosure,
        args: CallArgs<'_>,
    ) -> Result<VmValue, VmError> {
        let saved_handlers = std::mem::take(&mut self.exception_handlers);
        let active_context = (!crate::step_runtime::is_tracked_function(&closure.func.name))
            .then(crate::step_runtime::take_active_context);

        let target_frame_depth = self.frames.len();
        let frame_result = self.push_closure_frame_args(closure, &args);
        drop(args);
        let result = match frame_result {
            Ok(()) => self.drive_until_frame_depth(target_frame_depth).await,
            Err(e) => Err(e),
        };

        self.exception_handlers = saved_handlers;
        if let Some(ctx) = active_context {
            crate::step_runtime::restore_active_context(ctx);
        }

        result
    }

    /// Invoke a value as a callable. Supports `VmValue::Closure` and
    /// `VmValue::BuiltinRef`, so builtin names passed by reference (e.g.
    /// `dict.rekey(snake_to_camel)`) dispatch through the same code path as
    /// user-defined closures.
    pub(crate) async fn call_callable_value(
        &mut self,
        callable: &VmValue,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        self.call_callable_args(callable, CallArgs::Slice(args))
            .await
    }

    pub(crate) async fn call_callable_owned(
        &mut self,
        callable: &VmValue,
        args: Vec<VmValue>,
    ) -> Result<VmValue, VmError> {
        self.call_callable_args(callable, CallArgs::Owned(args))
            .await
    }

    pub(crate) async fn call_callable_zero(
        &mut self,
        callable: &VmValue,
    ) -> Result<VmValue, VmError> {
        self.call_callable_args(callable, CallArgs::Empty).await
    }

    pub(crate) async fn call_callable_one(
        &mut self,
        callable: &VmValue,
        arg: &VmValue,
    ) -> Result<VmValue, VmError> {
        self.call_callable_args(callable, CallArgs::One(arg)).await
    }

    pub(crate) async fn call_callable_two(
        &mut self,
        callable: &VmValue,
        first: &VmValue,
        second: &VmValue,
    ) -> Result<VmValue, VmError> {
        self.call_callable_args(callable, CallArgs::Two(first, second))
            .await
    }

    pub(crate) async fn call_callable_args(
        &mut self,
        callable: &VmValue,
        args: CallArgs<'_>,
    ) -> Result<VmValue, VmError> {
        match callable {
            VmValue::Closure(closure) => self.call_closure_args(closure, args).await,
            VmValue::BuiltinRef(name) => {
                if !crate::autonomy::needs_async_side_effect_enforcement(name) {
                    if let Some(result) = self.call_sync_builtin_by_ref_args(name, &args) {
                        return result;
                    }
                }
                self.call_named_builtin(name, args.into_vec()).await
            }
            VmValue::BuiltinRefId(r) => {
                if let Some(result) =
                    self.try_call_sync_builtin_id_or_name_args(Some(r.id), &r.name, &args)
                {
                    return result;
                }
                self.call_builtin_id_or_name(r.id, &r.name, args.into_vec())
                    .await
            }
            other => Err(VmError::TypeError(format!(
                "expected callable, got {}",
                other.type_name()
            ))),
        }
    }

    fn call_sync_builtin_by_ref_args(
        &mut self,
        name: &str,
        args: &CallArgs<'_>,
    ) -> Option<Result<VmValue, VmError>> {
        self.try_call_sync_builtin_id_or_name_args(None, name, args)
    }

    /// Returns true if `v` is callable via `call_callable_value`.
    pub(crate) fn is_callable_value(v: &VmValue) -> bool {
        matches!(
            v,
            VmValue::Closure(_) | VmValue::BuiltinRef(_) | VmValue::BuiltinRefId(_)
        )
    }

    /// Public wrapper for `call_closure`, used by the MCP server to invoke
    /// tool handler closures from outside the VM execution loop.
    pub async fn call_closure_pub(
        &mut self,
        closure: &VmClosure,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        self.cancel_grace_instructions_remaining = None;
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

    pub(crate) fn try_call_sync_builtin_id_or_name_args(
        &mut self,
        direct_id: Option<BuiltinId>,
        name: &str,
        args: &CallArgs<'_>,
    ) -> Option<Result<VmValue, VmError>> {
        if self.denied_builtins.contains(name) {
            return Some(Err(VmError::CategorizedError {
                message: format!("Tool '{name}' is not permitted."),
                category: ErrorCategory::ToolRejected,
            }));
        }
        let builtin = match self.resolve_sync_builtin_id_or_name(direct_id, name)? {
            Ok(builtin) => builtin,
            Err(error) => return Some(Err(error)),
        };
        let _span =
            Self::builtin_span_kind(name).map(|kind| ScopeSpan::new(kind, name.to_string()));
        if let Err(error) = args.with_slice(|slice| self.validate_sync_builtin_args(name, slice)) {
            return Some(Err(error));
        }

        Some(args.with_slice(|slice| builtin(slice, &mut self.output)))
    }

    pub(crate) fn try_call_sync_builtin_id_or_name_from_stack_args(
        &mut self,
        direct_id: Option<BuiltinId>,
        name: &str,
        args_start: usize,
    ) -> Option<Result<VmValue, VmError>> {
        if self.denied_builtins.contains(name) {
            return Some(Err(VmError::CategorizedError {
                message: format!("Tool '{name}' is not permitted."),
                category: ErrorCategory::ToolRejected,
            }));
        }
        let builtin = match self.resolve_sync_builtin_id_or_name(direct_id, name)? {
            Ok(builtin) => builtin,
            Err(error) => return Some(Err(error)),
        };
        if args_start > self.stack.len() {
            return Some(Err(VmError::Runtime(
                "call argument stack underflow".to_string(),
            )));
        }

        let _span =
            Self::builtin_span_kind(name).map(|kind| ScopeSpan::new(kind, name.to_string()));
        let args = &self.stack[args_start..];
        if let Err(error) = self.validate_sync_builtin_args(name, args) {
            return Some(Err(error));
        }

        Some(builtin(args, &mut self.output))
    }

    async fn call_builtin_impl(
        &mut self,
        name: &str,
        args: Vec<VmValue>,
        direct_id: Option<BuiltinId>,
    ) -> Result<VmValue, VmError> {
        // Auto-trace LLM calls and tool calls.
        let _span =
            Self::builtin_span_kind(name).map(|kind| ScopeSpan::new(kind, name.to_string()));

        // Sandbox check: deny builtins blocked by --deny/--allow flags.
        if self.denied_builtins.contains(name) {
            return Err(VmError::CategorizedError {
                message: format!("Tool '{name}' is not permitted."),
                category: ErrorCategory::ToolRejected,
            });
        }
        let autonomy = if crate::autonomy::needs_async_side_effect_enforcement(name) {
            crate::autonomy::enforce_builtin_side_effect_boxed(name, &args).await?
        } else {
            None
        };
        if let Some(crate::autonomy::AutonomyDecision::Skip(value)) = autonomy {
            return Ok(value);
        }
        if !matches!(
            autonomy,
            Some(crate::autonomy::AutonomyDecision::AllowApproved)
        ) {
            crate::orchestration::enforce_current_policy_for_builtin(name, &args)?;
        }
        crate::typecheck::validate_builtin_call(name, &args, None)?;

        if let Some(result) =
            crate::runtime_context::dispatch_runtime_context_builtin(self, name, &args)
        {
            return result;
        }

        if let Some(id) = direct_id {
            if let Some(entry) = self.builtins_by_id.get(&id).cloned() {
                if entry.name.as_ref() == name {
                    return self.call_builtin_entry(name, entry.dispatch, args).await;
                }
            }
        }

        if let Some(builtin) = self.builtins.get(name).cloned() {
            self.call_builtin_entry(name, VmBuiltinDispatch::Sync(builtin), args)
                .await
        } else if let Some(async_builtin) = self.async_builtins.get(name).cloned() {
            self.call_builtin_entry(name, VmBuiltinDispatch::Async(async_builtin), args)
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
        name: &str,
        dispatch: VmBuiltinDispatch,
        args: Vec<VmValue>,
    ) -> Result<VmValue, VmError> {
        let result = match dispatch {
            VmBuiltinDispatch::Sync(builtin) => builtin(&args, &mut self.output),
            VmBuiltinDispatch::Async(async_builtin) => {
                // Bind a fresh child VM as the async-builtin context for the
                // duration of this future, threading the explicit ctx handle
                // into the handler. Drain any output VM-side closures
                // forwarded into the ctx back to the parent.
                let (result, captured) =
                    crate::vm::run_async_builtin_with(self.child_vm_inline(), |ctx| {
                        async_builtin(ctx, args)
                    })
                    .await;
                if !captured.is_empty() {
                    self.output.push_str(&captured);
                }
                result
            }
        }?;
        if matches!(
            name,
            "sync_mutex_acquire"
                | "sync_semaphore_acquire"
                | "sync_gate_acquire"
                | "sync_rwlock_acquire"
        ) {
            if let VmValue::SyncPermit(permit) = &result {
                self.adopt_sync_permit_for_current_scope(permit.as_ref().clone());
            }
        }
        Ok(result)
    }
}

/// Build the discoverable [`VmBuiltinMetadata`] for one entry of a
/// `#[harn_builtin]`-emitted `VmBuiltinDef`, threading the optional
/// category / doc / signature_text fields without duplicating the chain
/// across the Sync / Async dispatch arms in `register_builtin_def`.
fn builtin_def_metadata(
    def: &'static crate::stdlib::macros::VmBuiltinDef,
    name: &'static str,
    arity: VmBuiltinArity,
    kind: VmBuiltinKind,
) -> VmBuiltinMetadata {
    let mut meta = match kind {
        VmBuiltinKind::Sync => VmBuiltinMetadata::sync_static(name),
        VmBuiltinKind::Async => VmBuiltinMetadata::async_static(name),
    }
    .arity(arity);
    if let Some(category) = def.category {
        meta = meta.category_static(category);
    }
    if let Some(doc) = def.doc {
        meta = meta.doc_static(doc);
    }
    if let Some(sig_text) = def.signature_text {
        meta = meta.signature_static(sig_text);
    } else {
        // Builtins declared via `sig_expr = …` (a canonical
        // `harn_builtin_meta::signatures` const) carry no human-typed `sig`
        // string, so render the parsed signature back through its `Display`
        // impl. `Display` round-trips through the macro sig grammar (enforced
        // by the signature-text drift test), so `harn explain` / LSP hover
        // still surface an accurate, canonical signature.
        meta = meta.signature_owned(format!("{}", def.sig));
    }
    meta
}

/// Derive a [`VmBuiltinArity`] from a parsed [`BuiltinSignature`]. Required
/// params count toward the floor; optional params and `has_rest` widen the
/// ceiling. Returns `Variadic` for `(...args: any)`-shaped sigs that have
/// no required params, matching how the DSL builder previously declared
/// `Variadic` explicitly.
fn arity_from_sig(sig: &harn_builtin_meta::BuiltinSignature) -> VmBuiltinArity {
    let required = sig.params.iter().filter(|p| !p.optional).count();
    let total = sig.params.len();
    if sig.has_rest {
        if required == 0 {
            VmBuiltinArity::Variadic
        } else {
            VmBuiltinArity::Min(required)
        }
    } else if required == total {
        VmBuiltinArity::Exact(total)
    } else {
        VmBuiltinArity::Range {
            min: required,
            max: total,
        }
    }
}
