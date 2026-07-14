use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::chunk::{Chunk, ChunkRef, Op};
use crate::value::{ModuleFunctionRegistry, VmError, VmValue};

use super::state::ExecutionDeadlineState;
use super::{CallFrame, LocalSlot, Vm};

const CANCEL_GRACE_ASYNC_OP: Duration = Duration::from_millis(250);

pub(super) fn new_execution_deadline_state(
    deadline: Option<Instant>,
) -> Arc<ExecutionDeadlineState> {
    ExecutionDeadlineState::new(Instant::now(), deadline)
}

#[cfg(test)]
thread_local! {
    static SCOPE_INTERRUPT_ASYNC_DISPATCHES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(super) fn reset_scope_interrupt_async_dispatches() {
    SCOPE_INTERRUPT_ASYNC_DISPATCHES.set(0);
}

#[cfg(test)]
pub(super) fn scope_interrupt_async_dispatches() -> u64 {
    SCOPE_INTERRUPT_ASYNC_DISPATCHES.get()
}

#[derive(Clone, Copy)]
enum DeadlineKind {
    Execution,
    Scope,
    InterruptHandler,
}

impl Vm {
    /// Returns true when no scope-level async machinery is armed. The hot
    /// interpreter loop uses this to skip both the `pending_scope_interrupt`
    /// future and the `execute_op_with_scope_interrupts` `tokio::select!`
    /// wrapper on every dispatch — both are necessary for cancellable /
    /// deadlined VMs but pure overhead in the common case (benchmarks,
    /// background script execution, etc.).
    #[inline]
    pub(crate) fn scope_interrupts_clean(&self) -> bool {
        self.cancel_token.is_none()
            && self.interrupt_signal_token.is_none()
            && self.pending_interrupt_signal.is_none()
            && self.interrupt_handler_deadline.is_none()
            && !self.execution_deadline.is_active()
            && self.deadlines.is_empty()
    }

    /// Execute a compiled chunk.
    ///
    /// Convenience entry point for callers that hold a borrowed [`Chunk`] and
    /// run it once (tests, one-shot CLI invocations). It clones the chunk once
    /// to obtain the owned [`ChunkRef`] the call frame requires. Callers that
    /// re-run the same compiled chunk (servers, record filters, triggers)
    /// should hold a [`ChunkRef`] and call [`Vm::execute_arc`] to skip the
    /// per-execution deep copy of the bytecode + constant pool.
    pub async fn execute(&mut self, chunk: &Chunk) -> Result<VmValue, VmError> {
        self.execute_arc(Arc::new(chunk.clone())).await
    }

    /// Execute a compiled chunk under an uncatchable host wall-clock limit.
    ///
    /// This is distinct from Harn's catchable `deadline` expression: test
    /// runners and embedding hosts must be able to stop CPU-bound user code
    /// even when it never yields to the async runtime.
    pub async fn execute_with_timeout(
        &mut self,
        chunk: &Chunk,
        timeout: Duration,
    ) -> Result<VmValue, VmError> {
        let deadline = Instant::now().checked_add(timeout).ok_or_else(|| {
            VmError::Runtime("execution timeout exceeds the platform clock range".to_string())
        })?;
        let _deadline_guard = self.execution_deadline.install(deadline);
        self.execute(chunk).await
    }

    /// Execute a shared compiled chunk without cloning its bytecode.
    ///
    /// Threads the existing [`ChunkRef`] straight into the call frame, so
    /// re-running the same chunk is a refcount bump rather than an
    /// `O(code + constants)` copy.
    pub async fn execute_arc(&mut self, chunk: ChunkRef) -> Result<VmValue, VmError> {
        let registry = self.pool_registry.clone();
        crate::stdlib::pool::with_pool_registry_scope(registry, async {
            self.execute_scoped(chunk).await
        })
        .await
    }

    async fn execute_scoped(&mut self, chunk: ChunkRef) -> Result<VmValue, VmError> {
        let _execution_activity = self
            .wait_for_graph
            .register_task(self.runtime_context.task_id.clone());
        let span_id = crate::tracing::span_start(crate::tracing::SpanKind::Pipeline, "main".into());
        let result = self.run_chunk(chunk).await;
        let result = match result {
            Ok(value) => self.run_pipeline_finish_lifecycle(value).await,
            Err(error) => {
                crate::orchestration::clear_pipeline_on_finish();
                Err(error)
            }
        };
        crate::tracing::span_end(span_id);
        result
    }

    /// Run the pipeline-finish lifecycle: `PreFinish`, optional
    /// `OnUnsettledDetected`, the `on_finish` callback, `PostFinish`. The
    /// callback (if registered) may transform the return value; everything
    /// else is advisory.
    ///
    /// Tracked: <https://github.com/burin-labs/harn/issues/1854>.
    async fn run_pipeline_finish_lifecycle(&mut self, value: VmValue) -> Result<VmValue, VmError> {
        use crate::orchestration::{
            take_pipeline_on_finish, unsettled_state_snapshot_async, HookEvent,
        };
        let _tape_phase =
            crate::testbench::tape::enter_phase(crate::testbench::tape::TapePhase::RuntimeFinalize);

        let on_finish = take_pipeline_on_finish();
        let unsettled = unsettled_state_snapshot_async().await;

        let pre_payload = serde_json::json!({
            "event": HookEvent::PreFinish.as_str(),
            "return_value": crate::llm::vm_value_to_json(&value),
            "unsettled": unsettled.to_json(),
            "has_on_finish": on_finish.is_some(),
        });
        self.fire_finish_lifecycle_event(HookEvent::PreFinish, &pre_payload)
            .await?;

        if !unsettled.is_empty() {
            let payload = serde_json::json!({
                "event": HookEvent::OnUnsettledDetected.as_str(),
                "unsettled": unsettled.to_json(),
            });
            self.fire_finish_lifecycle_event(HookEvent::OnUnsettledDetected, &payload)
                .await?;
        }

        let final_value = if let Some(closure) = on_finish {
            let harness_value = crate::harness::Harness::real().into_vm_value();
            self.call_closure_pub(&closure, &[harness_value, value])
                .await?
        } else {
            value
        };

        let post_payload = serde_json::json!({
            "event": HookEvent::PostFinish.as_str(),
            "return_value": crate::llm::vm_value_to_json(&final_value),
            "unsettled": unsettled.to_json(),
        });
        self.fire_finish_lifecycle_event(HookEvent::PostFinish, &post_payload)
            .await?;

        Ok(final_value)
    }

    /// Dispatch a pipeline-finish lifecycle event by invoking matching
    /// hook closures directly on `self`. The shared `run_lifecycle_hooks`
    /// path clones a fresh child VM per call and discards its stdout —
    /// fine for the agent-loop boundaries where hooks are advisory side-
    /// channels, but the pipeline-finish boundary is the script's last
    /// chance to print before `vm.output()` is captured, so the closures
    /// run on `self` to keep their output visible.
    ///
    /// Honors the lifecycle control contract (harn#1859):
    ///   * `PreFinish` rejects `Block` outright — surfaces a runtime
    ///     error pointing the user at `OnFinish.block_until_settled`.
    ///     `PostFinish` ignores any control return (advisory only).
    ///   * `OnUnsettledDetected` honors `Block` to abort the finish
    ///     lifecycle until the unsettled work clears.
    ///   * Modify returns are recorded but not consumed at this boundary
    ///     (the dispatcher already replays subsequent hooks with the
    ///     post-modify payload via `run_lifecycle_hooks_with_control`).
    async fn fire_finish_lifecycle_event(
        &mut self,
        event: crate::orchestration::HookEvent,
        payload: &serde_json::Value,
    ) -> Result<(), VmError> {
        use crate::orchestration::{HookControl, HookEvent};
        let invocations = crate::orchestration::matching_vm_lifecycle_hooks(event, payload);
        if invocations.is_empty() {
            return Ok(());
        }
        let mut current_payload = payload.clone();
        for invocation in invocations {
            let arg = crate::stdlib::json_to_vm_value(&current_payload);
            let closure = invocation.resolve(self).await?;
            let raw = self.call_closure_pub(&closure, &[arg]).await?;
            let (action, effects) = crate::orchestration::collect_hook_effects_and_action(
                event,
                raw,
                crate::value::VmValue::Nil,
            )?;
            crate::orchestration::inject_hook_effects_into_current_session(effects)?;
            let control = crate::orchestration::parse_hook_control_for_finish(event, &action)?;
            match control {
                HookControl::Allow => {}
                HookControl::Block { reason } => {
                    if matches!(event, HookEvent::PreFinish) {
                        return Err(VmError::Runtime(format!(
                            "PreFinish hook returned block, which is not a valid control: {reason}. \
                             To delay pipeline finish until unsettled work clears, use \
                             OnFinish.block_until_settled (std/lifecycle) or return Modify/Allow \
                             from PreFinish."
                        )));
                    }
                    if matches!(event, HookEvent::PostFinish) {
                        // Advisory only; ignore block returns from PostFinish.
                        continue;
                    }
                    // OnUnsettledDetected: block aborts the finish lifecycle.
                    return Err(VmError::Runtime(format!(
                        "{} hook blocked pipeline finish: {reason}",
                        event.as_str()
                    )));
                }
                HookControl::Modify { payload: modified } => {
                    current_payload = modified;
                }
                HookControl::Decision { .. } => {}
            }
        }
        Ok(())
    }

    /// Convert a VmError into either a handled exception (returning Ok) or a propagated error.
    pub(crate) fn handle_error(&mut self, error: VmError) -> Result<Option<VmValue>, VmError> {
        if matches!(error, VmError::ExecutionDeadlineExceeded) {
            return Err(error);
        }
        let thrown_value = error.thrown_value();

        if let Some(handler) = self.exception_handlers.pop() {
            if let Some(error_type) = handler.error_type.as_deref() {
                // Typed catch: only match when the thrown enum's type equals the declared type.
                let matches = match &thrown_value {
                    VmValue::EnumVariant(enum_variant) => enum_variant.has_enum_name(error_type),
                    _ => false,
                };
                if !matches {
                    return self.handle_error(error);
                }
            }

            self.release_sync_guards_after_unwind(handler.frame_depth, handler.env_scope_depth);

            while self.frames.len() > handler.frame_depth {
                if let Some(frame) = self.frames.pop() {
                    if let Some(ref dir) = frame.saved_source_dir {
                        crate::stdlib::set_thread_source_dir(dir);
                    }
                    self.iterators.truncate(frame.saved_iterator_depth);
                    self.env = frame.saved_env;
                }
            }
            crate::step_runtime::prune_below_frame(self.frames.len());

            // Drop deadlines that belonged to unwound frames.
            while self
                .deadlines
                .last()
                .is_some_and(|d| d.1 > handler.frame_depth)
            {
                self.deadlines.pop();
            }

            self.env.truncate_scopes(handler.env_scope_depth);

            self.stack.truncate(handler.stack_depth);
            self.stack.push(thrown_value);

            if let Some(frame) = self.frames.last_mut() {
                frame.ip = handler.catch_ip;
            }

            Ok(None)
        } else {
            Err(error)
        }
    }

    pub(crate) async fn run_chunk(&mut self, chunk: ChunkRef) -> Result<VmValue, VmError> {
        self.run_chunk_ref(chunk, 0, None, None, None, None).await
    }

    pub(crate) async fn run_chunk_ref(
        &mut self,
        chunk: ChunkRef,
        argc: usize,
        saved_source_dir: Option<std::path::PathBuf>,
        module_functions: Option<ModuleFunctionRegistry>,
        module_state: Option<crate::value::ModuleState>,
        local_slots: Option<Vec<LocalSlot>>,
    ) -> Result<VmValue, VmError> {
        let debugger = self.debugger_attached();
        let local_slots = local_slots.unwrap_or_else(|| Self::fresh_local_slots(&chunk));
        let initial_env = if debugger {
            Some(self.env.clone())
        } else {
            None
        };
        let initial_local_slots = if debugger {
            Some(local_slots.clone())
        } else {
            None
        };
        let inline_cache_set = self.inline_cache_set_index_for_chunk(&chunk);
        self.frames.push(CallFrame {
            chunk,
            inline_cache_set,
            ip: 0,
            stack_base: self.stack.len(),
            saved_env: self.env.clone(),
            initial_env,
            initial_local_slots,
            saved_iterator_depth: self.iterators.len(),
            fn_name: String::new(),
            argc,
            saved_source_dir,
            module_functions,
            module_state,
            local_slots,
            local_scope_base: self.env.scope_depth().saturating_sub(1),
            local_scope_depth: 0,
        });

        self.drive_dispatch_loop(0, false).await
    }

    /// Sub-execution entrypoint used by [`Vm::call_closure`]: runs the
    /// dispatch loop until the topmost frame pops back to `target_depth`,
    /// restoring env/iterators/stack on that final pop so the caller's
    /// state is intact. Distinct from the entrypoint-mode call in
    /// [`Vm::run_chunk_ref`] (which preserves the script's top-level scope
    /// for the module-init capture in `modules.rs`).
    pub(crate) async fn drive_until_frame_depth(
        &mut self,
        target_depth: usize,
    ) -> Result<VmValue, VmError> {
        self.drive_dispatch_loop(target_depth, true).await
    }

    /// Dispatch loop body, parameterized on a target frame depth at which
    /// the loop should return and whether to restore the caller's
    /// env/iterators/stack on the final pop.
    ///
    /// `restore_on_final_pop = false` is the entrypoint mode used by
    /// `run_chunk_ref` (leaves the script's top-level state in place so the
    /// caller can capture it — see `modules.rs`).
    ///
    /// `restore_on_final_pop = true` is the sub-execution mode used by
    /// `call_closure`: the closure's frame is pushed onto the caller's
    /// frame stack and the loop drains it back to `target_depth`, so the
    /// per-invocation `Box::pin` heap allocation a recursive async
    /// `call_closure` would require is avoided.
    async fn drive_dispatch_loop(
        &mut self,
        target_depth: usize,
        restore_on_final_pop: bool,
    ) -> Result<VmValue, VmError> {
        let _task_activity = self
            .wait_for_graph
            .register_task(self.runtime_context.task_id.clone());
        loop {
            // Slow path only: the interrupt-handler future, deadline check,
            // and host-signal poll inside `pending_scope_interrupt` are all
            // no-ops when no cancel/interrupt/deadline machinery is armed
            // (the common case for unsupervised execution), so guard them
            // with a sync check that avoids the per-iteration future
            // state-machine allocation.
            if !self.scope_interrupts_clean() {
                if let Some(err) = self.pending_scope_interrupt().await {
                    match self.handle_error(err) {
                        Ok(None) => continue,
                        Ok(Some(val)) => return Ok(val),
                        Err(e) => {
                            self.unwind_frames_to_depth(target_depth);
                            return Err(e);
                        }
                    }
                }
            }

            let frame = match self.frames.last_mut() {
                Some(f) => f,
                None => return Ok(self.stack.pop().unwrap_or(VmValue::Nil)),
            };

            if frame.ip >= frame.chunk.code.len() {
                let val = self.stack.pop().unwrap_or(VmValue::Nil);
                let val = self.run_step_post_hooks_for_current_frame(val).await?;
                self.release_sync_guards_for_frame(self.frames.len());
                let popped_frame = self.frames.pop().unwrap();
                if let Some(ref dir) = popped_frame.saved_source_dir {
                    crate::stdlib::set_thread_source_dir(dir);
                }
                let current_depth = self.frames.len();
                crate::step_runtime::prune_below_frame(current_depth);
                // Drop any deadlines owned by the popped frame so the
                // caller doesn't inherit them (an early `return` from
                // inside `deadline(d) { ... }` would otherwise leave the
                // deadline live across the function boundary).
                while self.deadlines.last().is_some_and(|d| d.1 > current_depth) {
                    self.deadlines.pop();
                }

                let reached_target = current_depth <= target_depth;
                if reached_target && !restore_on_final_pop {
                    // Entrypoint mode: leave env / iterators / stack in place
                    // so the caller can observe the script's top-level scope.
                    return Ok(val);
                }
                self.iterators.truncate(popped_frame.saved_iterator_depth);
                self.env = popped_frame.saved_env;
                self.stack.truncate(popped_frame.stack_base);
                if reached_target {
                    return Ok(val);
                }
                self.stack.push(val);
                continue;
            }

            let op_byte = frame.chunk.code[frame.ip];
            // Line-coverage hit. `self.coverage` is `None` unless a coverage
            // session is active, so this is a single predictable branch on the
            // hot path (and a disjoint-field borrow from `frame`, which holds
            // `self.frames`). `frame.ip` is still the index of the instruction
            // we just read, before the increment below.
            if let Some(coverage) = self.coverage.as_mut() {
                coverage.record(&frame.chunk, frame.ip);
            }
            frame.ip += 1;

            // Sync/async split dispatch: sync opcodes stay on the direct hot
            // path even while a host deadline is armed. The instruction-boundary
            // check above makes CPU-bound code interruptible without paying for
            // a future and `tokio::select!` on every arithmetic/local opcode.
            let op = match Op::from_byte(op_byte) {
                Some(op) => op,
                None => return Err(VmError::InvalidInstruction(op_byte)),
            };
            let op_result: Result<(), VmError> = if let Some(result) = self.execute_op_sync(op) {
                result
            } else if self.scope_interrupts_clean() {
                self.execute_op_async(op).await
            } else {
                match self.execute_op_with_scope_interrupts(op_byte).await {
                    Ok(Some(val)) => return Ok(val),
                    Ok(None) => Ok(()),
                    Err(e) => Err(e),
                }
            };

            match op_result {
                Ok(()) => continue,
                Err(VmError::Return(val)) => {
                    let val = self.run_step_post_hooks_for_current_frame(val).await?;
                    if let Some(popped_frame) = self.frames.pop() {
                        self.release_sync_guards_for_frame(self.frames.len() + 1);
                        if let Some(ref dir) = popped_frame.saved_source_dir {
                            crate::stdlib::set_thread_source_dir(dir);
                        }
                        let current_depth = self.frames.len();
                        self.exception_handlers
                            .retain(|h| h.frame_depth <= current_depth);
                        crate::step_runtime::prune_below_frame(current_depth);
                        while self.deadlines.last().is_some_and(|d| d.1 > current_depth) {
                            self.deadlines.pop();
                        }

                        let reached_target = current_depth <= target_depth;
                        if reached_target && !restore_on_final_pop {
                            return Ok(val);
                        }
                        self.iterators.truncate(popped_frame.saved_iterator_depth);
                        self.env = popped_frame.saved_env;
                        self.stack.truncate(popped_frame.stack_base);
                        if reached_target {
                            return Ok(val);
                        }
                        self.stack.push(val);
                    } else {
                        return Ok(val);
                    }
                }
                Err(e) => {
                    // Capture stack trace before error handling unwinds frames.
                    if self.error_stack_trace.is_empty() {
                        self.error_stack_trace = self.capture_stack_trace();
                    }
                    // Honor `@step(error_boundary: ...)` if a step-budget
                    // exhaustion error is propagating out of the step's
                    // own frame. `continue` swaps the throw for a Nil
                    // return; `escalate` re-tags the error as a handoff
                    // escalation and lets the existing exception
                    // handlers route it.
                    let e = match self.apply_step_error_boundary(e) {
                        StepBoundaryOutcome::Returned(val) => {
                            self.error_stack_trace.clear();
                            if self.frames.len() <= target_depth {
                                return Ok(val);
                            }
                            self.stack.push(val);
                            continue;
                        }
                        StepBoundaryOutcome::Throw(err) => err,
                    };
                    match self.handle_error(e) {
                        Ok(None) => {
                            self.error_stack_trace.clear();
                            continue;
                        }
                        Ok(Some(val)) => return Ok(val),
                        Err(e) => {
                            self.unwind_frames_to_depth(target_depth);
                            return Err(self.enrich_error_with_line(e));
                        }
                    }
                }
            }
        }
    }

    /// Pop frames until `self.frames.len() <= target_depth`, restoring env,
    /// iterators, stack, source-dir thread-locals, and releasing per-frame
    /// sync guards for each popped frame. Used by [`drive_until_frame_depth`]
    /// on the error path so a closure sub-execution leaves caller-visible
    /// state at the same depth it found when an unhandled error propagates
    /// out.
    fn unwind_frames_to_depth(&mut self, target_depth: usize) {
        while self.frames.len() > target_depth {
            let frame_depth = self.frames.len();
            if let Some(frame) = self.frames.pop() {
                self.release_sync_guards_for_frame(frame_depth);
                if let Some(ref dir) = frame.saved_source_dir {
                    crate::stdlib::set_thread_source_dir(dir);
                }
                self.iterators.truncate(frame.saved_iterator_depth);
                self.env = frame.saved_env;
                self.stack.truncate(frame.stack_base);
            }
        }
        let current_depth = self.frames.len();
        crate::step_runtime::prune_below_frame(current_depth);
        while self.deadlines.last().is_some_and(|d| d.1 > current_depth) {
            self.deadlines.pop();
        }
    }

    /// Inspect a thrown error against the topmost active step's
    /// `error_boundary`. Called from the main step loop before
    /// `handle_error` so that a step's own budget-exhaustion error can be
    /// short-circuited (`continue`) or annotated (`escalate`) before the
    /// generic try/catch machinery sees it.
    pub(crate) fn apply_step_error_boundary(&mut self, error: VmError) -> StepBoundaryOutcome {
        use crate::step_runtime;
        if !step_runtime::is_step_budget_exhausted(&error) {
            return StepBoundaryOutcome::Throw(error);
        }
        let Some(step_depth) = step_runtime::active_step_frame_depth() else {
            return StepBoundaryOutcome::Throw(error);
        };
        // The step's frame is the topmost on the call stack iff its
        // recorded frame_depth equals `frames.len()`. If the throw is
        // coming from a deeper frame we let it bubble up — the boundary
        // still applies later when the step's own frame is reached.
        if step_depth != self.frames.len() {
            return StepBoundaryOutcome::Throw(error);
        }
        let boundary = step_runtime::with_active_step(|step| step.definition.boundary())
            .unwrap_or(step_runtime::StepErrorBoundary::Fail);
        match boundary {
            step_runtime::StepErrorBoundary::Continue => {
                // Mimic VmError::Return(Nil) for the step's frame: pop
                // the frame, restore its env/iterators/stack, and feed a
                // Nil return value back to the caller.
                if let Some(popped) = self.frames.pop() {
                    self.release_sync_guards_for_frame(self.frames.len() + 1);
                    if let Some(ref dir) = popped.saved_source_dir {
                        crate::stdlib::set_thread_source_dir(dir);
                    }
                    let current_depth = self.frames.len();
                    self.exception_handlers
                        .retain(|h| h.frame_depth <= current_depth);
                    step_runtime::pop_and_record(
                        current_depth + 1,
                        "skipped",
                        Some(step_runtime_error_message(&error)),
                    );
                    if self.frames.is_empty() {
                        return StepBoundaryOutcome::Returned(VmValue::Nil);
                    }
                    self.iterators.truncate(popped.saved_iterator_depth);
                    self.env = popped.saved_env;
                    self.stack.truncate(popped.stack_base);
                }
                StepBoundaryOutcome::Returned(VmValue::Nil)
            }
            step_runtime::StepErrorBoundary::Escalate => {
                let identity = step_runtime::with_active_step(|step| {
                    (
                        step.definition.name.clone(),
                        step.definition.function.clone(),
                    )
                });
                step_runtime::pop_and_record(
                    step_depth,
                    "escalated",
                    Some(step_runtime_error_message(&error)),
                );
                let (step_name, function) = identity.unzip();
                StepBoundaryOutcome::Throw(step_runtime::mark_escalated(
                    error,
                    step_name.as_deref(),
                    function.as_deref(),
                ))
            }
            step_runtime::StepErrorBoundary::Fail => {
                step_runtime::pop_and_record(
                    step_depth,
                    "failed",
                    Some(step_runtime_error_message(&error)),
                );
                StepBoundaryOutcome::Throw(error)
            }
        }
    }
}

fn next_deadline(
    execution_deadline: Option<Instant>,
    scope_deadline: Option<Instant>,
    interrupt_handler_deadline: Option<Instant>,
) -> (Option<Instant>, Option<DeadlineKind>) {
    [
        (execution_deadline, DeadlineKind::Execution),
        (scope_deadline, DeadlineKind::Scope),
        (interrupt_handler_deadline, DeadlineKind::InterruptHandler),
    ]
    .into_iter()
    .filter_map(|(deadline, kind)| deadline.map(|deadline| (deadline, kind)))
    .min_by_key(|(deadline, _)| *deadline)
    .map_or((None, None), |(deadline, kind)| {
        (Some(deadline), Some(kind))
    })
}

fn step_runtime_error_message(error: &VmError) -> String {
    match error {
        VmError::Thrown(VmValue::Dict(dict)) => dict
            .get("message")
            .map(|v| v.display())
            .unwrap_or_else(|| error.to_string()),
        _ => error.to_string(),
    }
}

pub(crate) enum StepBoundaryOutcome {
    Returned(VmValue),
    Throw(VmError),
}

impl crate::vm::Vm {
    pub(crate) async fn execute_one_cycle(&mut self) -> Result<Option<(VmValue, bool)>, VmError> {
        if let Some(err) = self.pending_scope_interrupt().await {
            match self.handle_error(err) {
                Ok(None) => return Ok(None),
                Ok(Some(val)) => return Ok(Some((val, false))),
                Err(e) => return Err(e),
            }
        }

        let frame = match self.frames.last_mut() {
            Some(f) => f,
            None => {
                let val = self.stack.pop().unwrap_or(VmValue::Nil);
                return Ok(Some((val, false)));
            }
        };

        if frame.ip >= frame.chunk.code.len() {
            let val = self.stack.pop().unwrap_or(VmValue::Nil);
            self.release_sync_guards_for_frame(self.frames.len());
            let popped_frame = self.frames.pop().unwrap();
            if self.frames.is_empty() {
                return Ok(Some((val, false)));
            }
            self.iterators.truncate(popped_frame.saved_iterator_depth);
            self.env = popped_frame.saved_env;
            self.stack.truncate(popped_frame.stack_base);
            self.stack.push(val);
            return Ok(None);
        }

        let op = frame.chunk.code[frame.ip];
        frame.ip += 1;

        match self.execute_op_with_scope_interrupts(op).await {
            Ok(Some(val)) => Ok(Some((val, false))),
            Ok(None) => Ok(None),
            Err(VmError::Return(val)) => {
                if let Some(popped_frame) = self.frames.pop() {
                    self.release_sync_guards_for_frame(self.frames.len() + 1);
                    if let Some(ref dir) = popped_frame.saved_source_dir {
                        crate::stdlib::set_thread_source_dir(dir);
                    }
                    let current_depth = self.frames.len();
                    self.exception_handlers
                        .retain(|h| h.frame_depth <= current_depth);
                    if self.frames.is_empty() {
                        return Ok(Some((val, false)));
                    }
                    self.iterators.truncate(popped_frame.saved_iterator_depth);
                    self.env = popped_frame.saved_env;
                    self.stack.truncate(popped_frame.stack_base);
                    self.stack.push(val);
                    Ok(None)
                } else {
                    Ok(Some((val, false)))
                }
            }
            Err(e) => {
                if self.error_stack_trace.is_empty() {
                    self.error_stack_trace = self.capture_stack_trace();
                }
                match self.handle_error(e) {
                    Ok(None) => {
                        self.error_stack_trace.clear();
                        Ok(None)
                    }
                    Ok(Some(val)) => Ok(Some((val, false))),
                    Err(e) => Err(self.enrich_error_with_line(e)),
                }
            }
        }
    }

    async fn execute_op_with_scope_interrupts(
        &mut self,
        op: u8,
    ) -> Result<Option<VmValue>, VmError> {
        #[cfg(test)]
        SCOPE_INTERRUPT_ASYNC_DISPATCHES
            .set(SCOPE_INTERRUPT_ASYNC_DISPATCHES.get().saturating_add(1));

        enum ScopeInterruptResult {
            Op(Result<Option<VmValue>, VmError>),
            Deadline(DeadlineKind),
            CancelTimedOut,
        }

        let (deadline, deadline_kind) = next_deadline(
            self.execution_deadline.current(),
            self.deadlines.last().map(|(deadline, _)| *deadline),
            self.interrupt_handler_deadline,
        );
        let cancel_token = self.cancel_token.clone();

        if deadline.is_none() && cancel_token.is_none() {
            return self.execute_op(op).await;
        }

        let has_deadline = deadline.is_some();
        let cancel_requested_at_start = cancel_token
            .as_ref()
            .is_some_and(|token| token.load(std::sync::atomic::Ordering::SeqCst));
        let has_cancel = cancel_token.is_some() && !cancel_requested_at_start;
        let deadline_sleep = async move {
            if let Some(deadline) = deadline {
                tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
            } else {
                std::future::pending::<()>().await;
            }
        };
        let cancel_sleep = async move {
            if let Some(token) = cancel_token {
                while !token.load(std::sync::atomic::Ordering::SeqCst) {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            } else {
                std::future::pending::<()>().await;
            }
        };

        let result = {
            let op_future = self.execute_op(op);
            tokio::pin!(op_future);
            tokio::select! {
                result = &mut op_future => ScopeInterruptResult::Op(result),
                _ = deadline_sleep, if has_deadline => {
                    ScopeInterruptResult::Deadline(deadline_kind.unwrap_or(DeadlineKind::Scope))
                },
                _ = cancel_sleep, if has_cancel => {
                    let grace = tokio::time::sleep(CANCEL_GRACE_ASYNC_OP);
                    tokio::pin!(grace);
                    tokio::select! {
                        result = &mut op_future => ScopeInterruptResult::Op(result),
                        _ = &mut grace => ScopeInterruptResult::CancelTimedOut,
                    }
                }
            }
        };

        match result {
            ScopeInterruptResult::Op(result) => result,
            ScopeInterruptResult::Deadline(DeadlineKind::Execution) => {
                self.cancel_spawned_tasks();
                Err(VmError::ExecutionDeadlineExceeded)
            }
            ScopeInterruptResult::Deadline(DeadlineKind::Scope) => {
                self.deadlines.pop();
                self.cancel_spawned_tasks();
                Err(Self::deadline_exceeded_error())
            }
            ScopeInterruptResult::Deadline(DeadlineKind::InterruptHandler) => {
                Err(Self::interrupt_handler_timeout_error())
            }
            ScopeInterruptResult::CancelTimedOut => {
                self.cancel_spawned_tasks();
                let signal = self
                    .take_host_interrupt_signal()
                    .unwrap_or_else(|| "SIGINT".to_string());
                if self.has_interrupt_handler_for(&signal) {
                    self.dispatch_interrupt_handlers(&signal).await?;
                }
                Err(Self::cancelled_error())
            }
        }
    }

    pub(crate) fn deadline_exceeded_error() -> VmError {
        VmError::Thrown(VmValue::String(arcstr::ArcStr::from("Deadline exceeded")))
    }

    pub(crate) fn cancelled_error() -> VmError {
        VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "kind:cancelled:VM cancelled by host",
        )))
    }

    /// Capture the current call stack as (fn_name, line, col, source_file) tuples.
    pub(crate) fn capture_stack_trace(&self) -> Vec<(String, usize, usize, Option<String>)> {
        self.frames
            .iter()
            .map(|f| {
                let idx = if f.ip > 0 { f.ip - 1 } else { 0 };
                let line = f.chunk.lines.get(idx).copied().unwrap_or(0) as usize;
                let col = f.chunk.columns.get(idx).copied().unwrap_or(0) as usize;
                (f.fn_name.clone(), line, col, f.chunk.source_file.clone())
            })
            .collect()
    }

    /// Enrich a VmError with source line information from the captured stack
    /// trace. Appends ` (line N)` to error variants whose messages don't
    /// already carry location context.
    pub(crate) fn enrich_error_with_line(&self, error: VmError) -> VmError {
        // Determine the line AND source file from the captured stack trace
        // (innermost frame) so the error names the exact `.harn` it crashed in.
        // A bare `(line N)` is ambiguous across 100+ stdlib files and forces a
        // manual hunt; `(stall.harn:497)` pinpoints it immediately.
        let (line, file) = self
            .error_stack_trace
            .last()
            .map(|(_, l, _, f)| (*l, f.clone()))
            .unwrap_or_else(|| (self.current_line(), None));
        if line == 0 {
            return error;
        }
        let suffix = match file.as_deref() {
            Some(path) => {
                let name = std::path::Path::new(path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(path);
                format!(" ({name}:{line})")
            }
            None => format!(" (line {line})"),
        };
        match error {
            VmError::Runtime(msg) => VmError::Runtime(format!("{msg}{suffix}")),
            VmError::TypeError(msg) => VmError::TypeError(format!("{msg}{suffix}")),
            VmError::DivisionByZero => VmError::Runtime(format!("Division by zero{suffix}")),
            VmError::UndefinedVariable(name) => {
                VmError::Runtime(format!("Undefined variable: {name}{suffix}"))
            }
            VmError::UndefinedBuiltin(name) => {
                VmError::Runtime(format!("Undefined builtin: {name}{suffix}"))
            }
            VmError::ImmutableAssignment(name) => VmError::Runtime(format!(
                "Cannot assign to immutable binding: {name}{suffix}"
            )),
            VmError::StackOverflow => {
                VmError::Runtime(format!("Stack overflow: too many nested calls{suffix}"))
            }
            // Leave these untouched:
            // - Thrown: user-thrown errors should not be silently modified
            // - CategorizedError: structured errors for agent orchestration
            // - Return: control flow, not a real error
            // - StackUnderflow / InvalidInstruction: internal VM bugs
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::Compiler;
    use crate::stdlib::register_vm_stdlib;
    use harn_lexer::Lexer;
    use harn_parser::Parser;

    fn compile_harn(source: &str) -> Chunk {
        let mut lexer = Lexer::new(source);
        let tokens = lexer.tokenize().unwrap();
        let mut parser = Parser::new(tokens);
        let program = parser.parse().unwrap();
        Compiler::new().compile(&program).unwrap()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dropping_timed_execution_restores_host_deadline() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let slow = compile_harn("pipeline default() { sleep(5s) }");
                let quick = compile_harn("pipeline default() { return 42 }");
                let mut vm = Vm::new();
                register_vm_stdlib(&mut vm);

                let mut execution =
                    Box::pin(vm.execute_with_timeout(&slow, Duration::from_secs(30)));
                tokio::select! {
                    result = &mut execution => panic!("slow execution unexpectedly finished: {result:?}"),
                    () = tokio::task::yield_now() => {}
                }
                drop(execution);

                assert!(!vm.execution_deadline.is_active());
                let value = vm.execute(&quick).await.unwrap();
                assert!(matches!(value, VmValue::Int(42)));
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn timed_finite_loop_keeps_sync_opcodes_on_direct_dispatch() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let chunk = compile_harn(
                    r"
pipeline default() {
  let total = 0
  for i in 0 to 10000 {
    total = total + i
  }
  return total
}
",
                );
                let mut vm = Vm::new();
                register_vm_stdlib(&mut vm);
                reset_scope_interrupt_async_dispatches();

                let value = vm
                    .execute_with_timeout(&chunk, Duration::from_secs(1))
                    .await
                    .unwrap();

                assert!(matches!(value, VmValue::Int(_)));
                assert!(
                    scope_interrupt_async_dispatches() <= 4,
                    "finite sync loop fell back to per-op async dispatch"
                );
            })
            .await;
    }
}
