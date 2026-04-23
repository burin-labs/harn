use std::rc::Rc;
use std::time::{Duration, Instant};

use crate::chunk::{Chunk, ChunkRef};
use crate::value::{ModuleFunctionRegistry, VmError, VmValue};

use super::{CallFrame, LocalSlot, Vm};

const CANCEL_GRACE_INSTRUCTIONS: usize = 1024;

impl Vm {
    /// Execute a compiled chunk.
    pub async fn execute(&mut self, chunk: &Chunk) -> Result<VmValue, VmError> {
        let span_id = crate::tracing::span_start(crate::tracing::SpanKind::Pipeline, "main".into());
        let result = self.run_chunk(chunk).await;
        crate::tracing::span_end(span_id);
        result
    }

    /// Convert a VmError into either a handled exception (returning Ok) or a propagated error.
    pub(crate) fn handle_error(&mut self, error: VmError) -> Result<Option<VmValue>, VmError> {
        let thrown_value = match &error {
            VmError::Thrown(v) => v.clone(),
            other => VmValue::String(Rc::from(other.to_string())),
        };

        if let Some(handler) = self.exception_handlers.pop() {
            if !handler.error_type.is_empty() {
                // Typed catch: only match when the thrown enum's type equals the declared type.
                let matches = match &thrown_value {
                    VmValue::EnumVariant { enum_name, .. } => {
                        enum_name.as_ref() == handler.error_type
                    }
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

    pub(crate) async fn run_chunk(&mut self, chunk: &Chunk) -> Result<VmValue, VmError> {
        self.run_chunk_entry(chunk, 0, None, None, None, None).await
    }

    pub(crate) async fn run_chunk_entry(
        &mut self,
        chunk: &Chunk,
        argc: usize,
        saved_source_dir: Option<std::path::PathBuf>,
        module_functions: Option<ModuleFunctionRegistry>,
        module_state: Option<crate::value::ModuleState>,
        local_slots: Option<Vec<LocalSlot>>,
    ) -> Result<VmValue, VmError> {
        self.run_chunk_ref(
            Rc::new(chunk.clone()),
            argc,
            saved_source_dir,
            module_functions,
            module_state,
            local_slots,
        )
        .await
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
        let initial_env = self.env.clone();
        let local_slots = local_slots.unwrap_or_else(|| Self::fresh_local_slots(&chunk));
        let initial_local_slots = local_slots.clone();
        self.frames.push(CallFrame {
            chunk,
            ip: 0,
            stack_base: self.stack.len(),
            saved_env: self.env.clone(),
            initial_env: Some(initial_env),
            initial_local_slots: Some(initial_local_slots),
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

        loop {
            if let Some(err) = self.pending_scope_interrupt() {
                match self.handle_error(err) {
                    Ok(None) => continue,
                    Ok(Some(val)) => return Ok(val),
                    Err(e) => return Err(e),
                }
            }

            let frame = match self.frames.last_mut() {
                Some(f) => f,
                None => return Ok(self.stack.pop().unwrap_or(VmValue::Nil)),
            };

            if frame.ip >= frame.chunk.code.len() {
                let val = self.stack.pop().unwrap_or(VmValue::Nil);
                self.release_sync_guards_for_frame(self.frames.len());
                let popped_frame = self.frames.pop().unwrap();
                if let Some(ref dir) = popped_frame.saved_source_dir {
                    crate::stdlib::set_thread_source_dir(dir);
                }

                if self.frames.is_empty() {
                    return Ok(val);
                } else {
                    self.iterators.truncate(popped_frame.saved_iterator_depth);
                    self.env = popped_frame.saved_env;
                    self.stack.truncate(popped_frame.stack_base);
                    self.stack.push(val);
                    continue;
                }
            }

            let op = frame.chunk.code[frame.ip];
            frame.ip += 1;

            match self.execute_op_with_scope_interrupts(op).await {
                Ok(Some(val)) => return Ok(val),
                Ok(None) => continue,
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
                            return Ok(val);
                        }
                        self.iterators.truncate(popped_frame.saved_iterator_depth);
                        self.env = popped_frame.saved_env;
                        self.stack.truncate(popped_frame.stack_base);
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
                    match self.handle_error(e) {
                        Ok(None) => {
                            self.error_stack_trace.clear();
                            continue;
                        }
                        Ok(Some(val)) => return Ok(val),
                        Err(e) => return Err(self.enrich_error_with_line(e)),
                    }
                }
            }
        }
    }

    pub(crate) async fn execute_one_cycle(&mut self) -> Result<Option<(VmValue, bool)>, VmError> {
        if let Some(err) = self.pending_scope_interrupt() {
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
            } else {
                self.iterators.truncate(popped_frame.saved_iterator_depth);
                self.env = popped_frame.saved_env;
                self.stack.truncate(popped_frame.stack_base);
                self.stack.push(val);
                return Ok(None);
            }
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

    fn pending_scope_interrupt(&mut self) -> Option<VmError> {
        if self.is_cancel_requested() {
            match self.cancel_grace_instructions_remaining.as_mut() {
                Some(0) => {
                    self.cancel_spawned_tasks();
                    return Some(Self::cancelled_error());
                }
                Some(remaining) => *remaining -= 1,
                None => self.cancel_grace_instructions_remaining = Some(CANCEL_GRACE_INSTRUCTIONS),
            }
        } else {
            self.cancel_grace_instructions_remaining = None;
        }
        if let Some(&(deadline, _)) = self.deadlines.last() {
            if Instant::now() >= deadline {
                self.deadlines.pop();
                return Some(Self::deadline_exceeded_error());
            }
        }
        None
    }

    async fn execute_op_with_scope_interrupts(
        &mut self,
        op: u8,
    ) -> Result<Option<VmValue>, VmError> {
        let deadline = self.deadlines.last().map(|(deadline, _)| *deadline);
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

        tokio::select! {
            result = self.execute_op(op) => result,
            _ = deadline_sleep, if has_deadline => {
                self.deadlines.pop();
                self.cancel_spawned_tasks();
                Err(Self::deadline_exceeded_error())
            }
            _ = cancel_sleep, if has_cancel => {
                self.cancel_spawned_tasks();
                Err(Self::cancelled_error())
            }
        }
    }

    pub(crate) fn deadline_exceeded_error() -> VmError {
        VmError::Thrown(VmValue::String(Rc::from("Deadline exceeded")))
    }

    pub(crate) fn cancelled_error() -> VmError {
        VmError::Thrown(VmValue::String(Rc::from(
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
        // Determine the line from the captured stack trace (innermost frame).
        let line = self
            .error_stack_trace
            .last()
            .map(|(_, l, _, _)| *l)
            .unwrap_or_else(|| self.current_line());
        if line == 0 {
            return error;
        }
        let suffix = format!(" (line {line})");
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
