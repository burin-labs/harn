use crate::cancellation::{cancelled_error, HandlerDispatch};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::chunk::{Chunk, ChunkRef, Op};
use crate::value::{ModuleFunctionRegistry, VmError, VmValue};

use super::callable_entry::TopLevelEntry;
use super::state::ExecutionDeadlineState;
use super::{CallFrame, LocalSlot, Vm};

const CANCEL_GRACE_ASYNC_OP: Duration = Duration::from_millis(250);

pub(super) fn new_execution_deadline_state(
    deadline: Option<Instant>,
) -> Arc<ExecutionDeadlineState> {
    ExecutionDeadlineState::new(Instant::now(), deadline)
}

#[cfg(any(test, feature = "vm-test-support"))]
thread_local! {
    static SCOPE_INTERRUPT_ASYNC_DISPATCHES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static SCOPE_INTERRUPT_POLLS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(feature = "vm-test-support")]
pub fn reset_scope_interrupt_async_dispatches() {
    SCOPE_INTERRUPT_ASYNC_DISPATCHES.set(0);
}

#[cfg(feature = "vm-test-support")]
pub fn scope_interrupt_async_dispatches() -> u64 {
    SCOPE_INTERRUPT_ASYNC_DISPATCHES.get()
}

#[derive(Clone, Copy)]
enum DeadlineKind {
    Execution,
    Scope,
    InterruptHandler,
}

impl Vm {
    /// Returns true when no scope-level async machinery is armed. Async
    /// opcodes use this to skip the `execute_op_with_scope_interrupts`
    /// `tokio::select!` wrapper, which an armed cancel token or deadline
    /// needs so it can stop an in-flight await.
    #[inline]
    pub(crate) fn scope_interrupts_clean(&self) -> bool {
        !self.process_exit_requested()
            && self.cancel_token.is_none()
            && self.interrupt_signal_token.is_none()
            && self.pending_interrupt_signal.is_none()
            && self.interrupt_handler_deadline.is_none()
            && !self.execution_deadline.is_active()
            && self.deadlines.is_empty()
    }

    /// Returns true when `pending_scope_interrupt` could report something at
    /// this instruction boundary. Every production host (`harn run`, serve,
    /// ACP, trigger dispatch) arms a cancel token, so this checks whether a
    /// request is pending rather than whether the machinery is armed. An armed
    /// but unset token costs one atomic load per instruction instead of the
    /// full poll, which takes the host signal mutex.
    ///
    /// The host signal token is covered by the cancel flag because hosts write
    /// the signal before flipping cancellation. Deadlines are time-based, so
    /// an armed one still polls the clock at every boundary. A leftover cancel
    /// grace counter polls once more so it resets before the next request.
    #[inline]
    fn scope_interrupt_poll_due(&self) -> bool {
        self.process_exit_requested()
            || self.is_cancel_requested()
            || self.pending_interrupt_signal.is_some()
            || self.cancel_grace_instructions_remaining.is_some()
            || self.interrupt_handler_deadline.is_some()
            || self.execution_deadline.is_active()
            || !self.deadlines.is_empty()
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
    ///
    /// Dropping the returned future after polling restores the caller's ambient
    /// execution context and abandons spans opened by this execution. The VM
    /// itself remains poisoned, so every later execution returns
    /// [`VmError::AbandonedExecution`], and dropping it aborts spawned children.
    pub async fn execute_with_timeout(
        &mut self,
        chunk: &Chunk,
        timeout: Duration,
    ) -> Result<VmValue, VmError> {
        self.execute_top_level_with_timeout(TopLevelEntry::Chunk(Arc::new(chunk.clone())), timeout)
            .await
    }

    pub(super) async fn execute_top_level_with_timeout(
        &mut self,
        entry: TopLevelEntry,
        timeout: Duration,
    ) -> Result<VmValue, VmError> {
        let deadline = Instant::now().checked_add(timeout).ok_or_else(|| {
            VmError::Runtime("execution timeout exceeds the platform clock range".to_string())
        })?;
        let runtime = Arc::clone(&self.runtime);
        runtime.execute_timed(self, entry, deadline).await
    }

    pub fn install_execution_deadline(
        &self,
        deadline: Instant,
    ) -> super::execution_deadline::ExecutionDeadlineGuard {
        self.execution_deadline.install(deadline)
    }

    /// Execute a shared compiled chunk without cloning its bytecode.
    ///
    /// Threads the existing [`ChunkRef`] straight into the call frame, so
    /// re-running the same chunk is a refcount bump rather than an
    /// `O(code + constants)` copy.
    pub async fn execute_arc(&mut self, chunk: ChunkRef) -> Result<VmValue, VmError> {
        self.execute_top_level(TopLevelEntry::Chunk(chunk)).await
    }

    /// Convert a VmError into either a handled exception (returning Ok) or a propagated error.
    pub(crate) fn handle_error(&mut self, error: VmError) -> Result<Option<VmValue>, VmError> {
        if let Some(code) = error.process_exit_code() {
            self.request_process_exit(code);
        }
        if error.is_uncatchable_control_flow() {
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
                        self.runtime.set_source_dir(dir);
                    }
                    self.iterators.truncate(frame.saved_iterator_depth);
                    self.env = frame.saved_env;
                }
            }
            self.runtime.prune_frames(self.frames.len());

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
            let caught = super::CaughtError {
                value: thrown_value.clone(),
                error,
                stack_trace: std::mem::take(&mut self.error_stack_trace),
            };
            if handler.preserve {
                self.stack
                    .push(VmValue::resource(crate::value::VmResourceHandle::new(
                        "caught_exception",
                        caught,
                    )));
            } else {
                self.last_caught_error = Some(Box::new(caught));
                self.stack.push(thrown_value);
            }

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
        self.ensure_execution_available()?;
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
            fn_name: crate::value::HarnStr::new(),
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
        self.ensure_execution_available()?;
        let _task_activity = self
            .wait_for_graph
            .register_task(self.runtime_context.task_id.clone());
        loop {
            // Slow path only: the interrupt-handler future, deadline check,
            // and host-signal poll inside `pending_scope_interrupt` are all
            // no-ops until a request is pending or a deadline is armed, so
            // guard them with a sync check that avoids the per-iteration
            // future state machine.
            if self.scope_interrupt_poll_due() {
                #[cfg(test)]
                SCOPE_INTERRUPT_POLLS.set(SCOPE_INTERRUPT_POLLS.get().saturating_add(1));
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

            let frame_depth = self.frames.len();
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
                    self.runtime.set_source_dir(dir);
                }
                let current_depth = self.frames.len();
                self.runtime.prune_frames(current_depth);
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
            if let Some(recorder) = self.recorders.as_ref().and_then(|r| r.work.as_ref()) {
                recorder.record_step();
            }
            if let Some(recorder) = self.flight_recorder.as_ref() {
                recorder.record_instruction(
                    &self.runtime_context.task_id,
                    frame_depth,
                    frame.fn_name.as_str(),
                    &frame.chunk,
                    self.source_file.as_deref(),
                    frame.ip - 1,
                    op,
                );
            }
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
                            self.runtime.set_source_dir(dir);
                        }
                        let current_depth = self.frames.len();
                        self.exception_handlers
                            .retain(|h| h.frame_depth <= current_depth);
                        self.runtime.prune_frames(current_depth);
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
                    self.runtime.set_source_dir(dir);
                }
                self.iterators.truncate(frame.saved_iterator_depth);
                self.env = frame.saved_env;
                self.stack.truncate(frame.stack_base);
            }
        }
        let current_depth = self.frames.len();
        self.runtime.prune_frames(current_depth);
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
        use super::step_context::StepBoundaryDecision;
        match self.runtime.step_error_boundary(error, self.frames.len()) {
            StepBoundaryDecision::Continue { message } => {
                // Mimic VmError::Return(Nil) for the step's frame: pop
                // the frame, restore its env/iterators/stack, and feed a
                // Nil return value back to the caller.
                if let Some(popped) = self.frames.pop() {
                    self.release_sync_guards_for_frame(self.frames.len() + 1);
                    if let Some(ref dir) = popped.saved_source_dir {
                        self.runtime.set_source_dir(dir);
                    }
                    let current_depth = self.frames.len();
                    self.exception_handlers
                        .retain(|h| h.frame_depth <= current_depth);
                    self.runtime.finish_skipped_step(current_depth + 1, message);
                    if self.frames.is_empty() {
                        return StepBoundaryOutcome::Returned(VmValue::Nil);
                    }
                    self.iterators.truncate(popped.saved_iterator_depth);
                    self.env = popped.saved_env;
                    self.stack.truncate(popped.stack_base);
                }
                StepBoundaryOutcome::Returned(VmValue::Nil)
            }
            StepBoundaryDecision::Throw(error) => StepBoundaryOutcome::Throw(error),
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

        let frame_depth = self.frames.len();
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

        let op_offset = frame.ip;
        let op = frame.chunk.code[op_offset];
        frame.ip += 1;

        if let Some(recorder) = self.recorders.as_ref().and_then(|r| r.work.as_ref()) {
            if Op::from_byte(op).is_some() {
                recorder.record_step();
            }
        }
        if let (Some(recorder), Some(decoded)) = (self.flight_recorder.as_ref(), Op::from_byte(op))
        {
            recorder.record_instruction(
                &self.runtime_context.task_id,
                frame_depth,
                frame.fn_name.as_str(),
                &frame.chunk,
                self.source_file.as_deref(),
                op_offset,
                decoded,
            );
        }

        match self.execute_op_with_scope_interrupts(op).await {
            Ok(Some(val)) => Ok(Some((val, false))),
            Ok(None) => Ok(None),
            Err(VmError::Return(val)) => {
                if let Some(popped_frame) = self.frames.pop() {
                    self.release_sync_guards_for_frame(self.frames.len() + 1);
                    if let Some(ref dir) = popped_frame.saved_source_dir {
                        self.runtime.set_source_dir(dir);
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
        #[cfg(any(test, feature = "vm-test-support"))]
        SCOPE_INTERRUPT_ASYNC_DISPATCHES
            .set(SCOPE_INTERRUPT_ASYNC_DISPATCHES.get().saturating_add(1));

        enum ScopeInterruptResult {
            Op(Result<Option<VmValue>, VmError>),
            Deadline(DeadlineKind),
            CancelTimedOut,
        }

        // Import execution temporarily replaces frame-, environment-, and
        // module-graph state. Dropping that future from this outer selector
        // would strand the replacement state in the caller VM. The imported
        // module's own bytecode still runs through this interrupt-aware loop,
        // so let the import transaction observe and unwind the interrupt
        // instead of cancelling the transaction future itself.
        if matches!(
            Op::from_byte(op),
            Some(
                Op::Import | Op::SelectiveImport | Op::NamespaceImport | Op::NamespaceImportMembers
            )
        ) {
            return self.execute_op(op).await;
        }

        let execution_deadline = Arc::clone(&self.execution_deadline);
        let scope_deadline = self.deadlines.last().map(|(deadline, _)| *deadline);
        let interrupt_handler_deadline = self.interrupt_handler_deadline;
        let cancel_token = self.cancel_token.clone();

        let has_deadline = execution_deadline.is_active()
            || scope_deadline.is_some()
            || interrupt_handler_deadline.is_some();
        if !has_deadline && cancel_token.is_none() {
            return self.execute_op(op).await;
        }

        let cancel_requested_at_start = cancel_token
            .as_ref()
            .is_some_and(|token| token.load(std::sync::atomic::Ordering::SeqCst));
        let has_cancel = cancel_token.is_some() && !cancel_requested_at_start;
        let deadline_sleep = async move {
            loop {
                // Register before reading state so a pause/resume between the
                // read and await cannot strand the deadline waiter.
                let changed = execution_deadline.changed();
                let (deadline, kind) = next_deadline(
                    execution_deadline.current(),
                    scope_deadline,
                    interrupt_handler_deadline,
                );
                if let Some(deadline) = deadline {
                    tokio::select! {
                        _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => {
                            return kind.unwrap_or(DeadlineKind::Scope);
                        }
                        _ = changed => {}
                    }
                } else {
                    changed.await;
                }
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
                kind = deadline_sleep, if has_deadline => {
                    ScopeInterruptResult::Deadline(kind)
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
                self.dispatch_handlers_for_observed_cancel().await?;
                // The owner was consulted on the line above, so this
                // cancellation has already had its chance to run handlers.
                Err(cancelled_error(HandlerDispatch::Dispatched))
            }
        }
    }

    pub(crate) fn deadline_exceeded_error() -> VmError {
        VmError::Thrown(VmValue::string("Deadline exceeded"))
    }

    /// Capture the current call stack as (fn_name, line, col, source_file) tuples.
    pub(crate) fn capture_stack_trace(&self) -> Vec<(String, usize, usize, Option<String>)> {
        self.frames
            .iter()
            .map(|f| {
                let idx = if f.ip > 0 { f.ip - 1 } else { 0 };
                let line = f.chunk.lines.get(idx).copied().unwrap_or(0) as usize;
                let col = f.chunk.columns.get(idx).copied().unwrap_or(0) as usize;
                (
                    f.fn_name.to_string(),
                    line,
                    col,
                    f.chunk.source_file.clone(),
                )
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
            // - DivisionByZero, ValueByteLimit, ArgTypeMismatch: their variant
            //   is the diagnostic code hosts and the portable differential
            //   match on; the stack trace already carries the location
            // - Thrown: user-thrown errors should not be silently modified
            // - CategorizedError: structured errors for agent orchestration
            // - Return / ProcessExit: control flow, not a real error
            // - StackUnderflow / InvalidInstruction: internal VM bugs
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::Compiler;
    use harn_lexer::Lexer;
    use harn_parser::Parser;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn compile_harn(source: &str) -> Chunk {
        let mut lexer = Lexer::new(source);
        let tokens = lexer.tokenize().unwrap();
        let mut parser = Parser::new(tokens);
        let program = parser.parse().unwrap();
        Compiler::new().compile(&program).unwrap()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn armed_cancel_token_polls_only_once_requested() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut vm = crate::vm::test_runtime::new_vm();
                let cancel = Arc::new(AtomicBool::new(false));
                vm.install_cancel_token(Arc::clone(&cancel));
                vm.install_interrupt_signal_token(Arc::new(std::sync::Mutex::new(None)));
                let flip = Arc::clone(&cancel);
                vm.register_builtin("request_cancel", move |_, _| {
                    flip.store(true, Ordering::SeqCst);
                    Ok(VmValue::Nil)
                });

                SCOPE_INTERRUPT_POLLS.set(0);
                let finite = compile_harn(
                    r"
pipeline default(harness: Harness) {
  let total = 0
  let i = 0
  while i <= 10000 {
    total = total + i
    i = i + 1
  }
  return total
}
",
                );
                let value = vm.execute(&finite).await.unwrap();
                assert!(matches!(value, VmValue::Int(_)));
                assert_eq!(
                    SCOPE_INTERRUPT_POLLS.get(),
                    0,
                    "an armed but unset cancel token polled the instruction boundary"
                );

                // Negative control: the same armed token still stops a loop
                // made only of sync opcodes once a request is pending.
                let spinning = compile_harn(
                    "pipeline default(harness: Harness) { request_cancel()\n while true {} }",
                );
                let error = vm.execute(&spinning).await.unwrap_err();
                assert!(
                    error.to_string().contains("cancelled"),
                    "expected cancellation, got {error:?}"
                );
                assert!(SCOPE_INTERRUPT_POLLS.get() > 0);
            })
            .await;
    }
}
