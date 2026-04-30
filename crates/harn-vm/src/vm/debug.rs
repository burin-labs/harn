use std::rc::Rc;

use crate::chunk::{Chunk, Constant};
use crate::value::{VmError, VmValue};

use super::{CallFrame, Vm};

/// Debug action returned by the debug hook.
#[derive(Debug, Clone, PartialEq)]
pub enum DebugAction {
    /// Continue execution normally.
    Continue,
    /// Stop (breakpoint hit, step complete).
    Stop,
}

/// Information about current execution state for the debugger.
#[derive(Debug, Clone)]
pub struct DebugState {
    pub line: usize,
    pub variables: std::collections::BTreeMap<String, VmValue>,
    pub frame_name: String,
    pub frame_depth: usize,
}

pub(super) type DebugHook = dyn FnMut(&DebugState) -> DebugAction;

impl Vm {
    /// Replace breakpoints for a single source file. Pass an empty string
    /// (or call `set_breakpoints` for the wildcard equivalent) to install
    /// breakpoints that match every file — useful for ad-hoc CLI runs
    /// where the embedder doesn't track per-file source paths.
    pub fn set_breakpoints_for_file(&mut self, file: &str, lines: Vec<usize>) {
        if lines.is_empty() {
            self.breakpoints.remove(file);
            return;
        }
        self.breakpoints
            .insert(file.to_string(), lines.into_iter().collect());
    }

    /// Backwards-compatible wildcard form. Stores all lines under the
    /// empty-string key, which matches *any* source file at the check
    /// site. Existing embedders that don't track file scoping still work.
    pub fn set_breakpoints(&mut self, lines: Vec<usize>) {
        self.set_breakpoints_for_file("", lines);
    }

    /// Replace the function-breakpoint set. Every subsequent closure
    /// call whose name matches one of the provided strings will pause
    /// on entry. Empty vec clears the set.
    pub fn set_function_breakpoints(&mut self, names: Vec<String>) {
        self.function_breakpoints = names.into_iter().collect();
        // Clear any pending latch so a stale entry from the previous
        // configuration doesn't fire once.
        self.pending_function_bp = None;
    }

    /// Returns the current function-breakpoint name set. Used by the
    /// DAP adapter to build the `setFunctionBreakpoints` response with
    /// verified=true per registered name.
    pub fn function_breakpoint_names(&self) -> Vec<String> {
        self.function_breakpoints.iter().cloned().collect()
    }

    /// Drain any pending function-breakpoint name latched by the most
    /// recent closure entry. Returns `Some(name)` exactly once per hit
    /// so the caller can emit a single `stopped` event.
    pub fn take_pending_function_bp(&mut self) -> Option<String> {
        self.pending_function_bp.take()
    }

    /// Source file path of the currently executing frame, if known.
    pub(crate) fn current_source_file(&self) -> Option<&str> {
        self.frames
            .last()
            .and_then(|f| f.chunk.source_file.as_deref())
    }

    /// True when a breakpoint at `line` is set for the current frame's
    /// source file (or the wildcard set covers it).
    pub(crate) fn breakpoint_matches(&self, line: usize) -> bool {
        if let Some(wild) = self.breakpoints.get("") {
            if wild.contains(&line) {
                return true;
            }
        }
        if let Some(file) = self.current_source_file() {
            if let Some(set) = self.breakpoints.get(file) {
                if set.contains(&line) {
                    return true;
                }
            }
            // Some callers send a relative or differently-prefixed path
            // than the chunk records; fall back to suffix comparison so
            // foo.harn matches /abs/path/foo.harn and vice-versa.
            for (key, set) in &self.breakpoints {
                if key.is_empty() {
                    continue;
                }
                if (file.ends_with(key.as_str()) || key.ends_with(file)) && set.contains(&line) {
                    return true;
                }
            }
        }
        false
    }

    /// Enable step mode (stop at the next source line regardless of
    /// frame depth — i.e. step-in semantics, descending into calls).
    pub fn set_step_mode(&mut self, step: bool) {
        self.step_mode = step;
        self.step_frame_depth = usize::MAX;
    }

    /// Enable step-over mode (stop at the next source line in the current
    /// frame or a shallower one, skipping past any nested calls).
    pub fn set_step_over(&mut self) {
        self.step_mode = true;
        self.step_frame_depth = self.frames.len();
    }

    /// Register a debug hook invoked whenever execution advances to a new source line.
    pub fn set_debug_hook<F>(&mut self, hook: F)
    where
        F: FnMut(&DebugState) -> DebugAction + 'static,
    {
        self.debug_hook = Some(Box::new(hook));
    }

    /// Clear the current debug hook.
    pub fn clear_debug_hook(&mut self) {
        self.debug_hook = None;
    }

    /// Enable step-out mode (stop at the next source line *after* the
    /// current frame has returned — strictly shallower than where the
    /// user requested the step-out).
    pub fn set_step_out(&mut self) {
        self.step_mode = true;
        // Condition site compares `frames.len() <= step_frame_depth`, so
        // storing N-1 makes the stop fire only after the current frame
        // pops (frames.len() drops from N to N-1 or less). Clamp to 0 for
        // the top frame — caller handles that via the usize::MAX sentinel
        // if they wanted step-in semantics.
        self.step_frame_depth = self.frames.len().saturating_sub(1);
    }

    /// Check if the VM is stopped at a debug point.
    pub fn is_stopped(&self) -> bool {
        self.stopped
    }

    /// Get the current debug state (variables, line, etc.).
    pub fn debug_state(&self) -> DebugState {
        let line = self.current_line();
        let variables = self.visible_variables();
        let frame_name = if self.frames.len() > 1 {
            format!("frame_{}", self.frames.len() - 1)
        } else {
            "pipeline".to_string()
        };
        DebugState {
            line,
            variables,
            frame_name,
            frame_depth: self.frames.len(),
        }
    }

    /// Call sites (name + ip) on `line` within the current frame's
    /// chunk — drives DAP `stepInTargets` (#112). Walks the chunk's
    /// parallel lines array, surfaces every Call / MethodCall /
    /// CallSpread and pairs it with the name of the constant or
    /// identifier preceding the call when we can derive it cheaply.
    pub fn call_sites_on_line(&self, line: u32) -> Vec<(u32, String)> {
        let Some(frame) = self.frames.last() else {
            return Vec::new();
        };
        let chunk = &frame.chunk;
        let mut out = Vec::new();
        let code = &chunk.code;
        let lines = &chunk.lines;
        let mut ip: usize = 0;
        while ip < code.len() {
            let op = code[ip];
            if ip < lines.len() && lines[ip] == line {
                // 0x00 .. 0x99 covers the opcode space the compiler
                // emits for calls. Rather than decode every op, we
                // pattern-match on the Call-family opcodes via
                // their numeric tag — stable because harn-vm locks
                // opcodes with pin tests.
                if matches!(op, 0x40..=0x44) {
                    // Best-effort label: take the most recent
                    // LoadConst / LoadGlobal constant value.
                    let label = Self::label_preceding_call(chunk, ip);
                    out.push((ip as u32, label));
                }
            }
            ip += 1;
        }
        out
    }

    fn label_preceding_call(chunk: &Chunk, call_ip: usize) -> String {
        // Walk backwards a few instructions to find a LoadConst that
        // resolves to a string (the callee name). Good enough for
        // the IDE menu; deep callee resolution can land later if
        // needed.
        let mut back = call_ip.saturating_sub(6);
        while back < call_ip {
            let op = chunk.code[back];
            // LoadConst opcodes (range covers the two-byte tag) —
            // fall back to "call" when none found.
            if (op == 0x01 || op == 0x02) && back + 2 < chunk.code.len() {
                let idx = (u16::from(chunk.code[back + 1]) << 8) | u16::from(chunk.code[back + 2]);
                if let Some(Constant::String(s)) = chunk.constants.get(idx as usize) {
                    return s.clone();
                }
            }
            back += 1;
        }
        "call".to_string()
    }

    /// Install (or replace) the cooperative cancellation token on
    /// this VM. Callers (DAP adapter, embedded host) flip the
    /// wrapped AtomicBool to request graceful shutdown; the step
    /// loop checks `is_cancel_requested()` at every instruction and
    /// exits with `VmError::Cancelled` when set.
    pub fn install_cancel_token(&mut self, token: std::sync::Arc<std::sync::atomic::AtomicBool>) {
        self.cancel_token = Some(token);
        self.cancel_grace_instructions_remaining = None;
    }

    /// Signal cooperative cancellation on this VM — the step loop
    /// unwinds on its next instruction check. Lazily allocates a
    /// fresh token when none is installed so hosts don't need to
    /// pre-plumb it on every launch. Returns the Arc so the caller
    /// can hold onto it and re-signal later if needed.
    pub fn signal_cancel(&mut self) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
        let token = self.cancel_token.clone().unwrap_or_else(|| {
            let t = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            self.cancel_token = Some(t.clone());
            t
        });
        token.store(true, std::sync::atomic::Ordering::SeqCst);
        token
    }

    /// True when cooperative cancellation has been requested.
    pub fn is_cancel_requested(&self) -> bool {
        self.cancel_token
            .as_ref()
            .map(|t| t.load(std::sync::atomic::Ordering::SeqCst))
            .unwrap_or(false)
    }

    /// Identifiers visible at the given frame's scope — locals plus
    /// every registered builtin + async builtin. Drives DAP
    /// `completions` (#109) so the REPL autocomplete surfaces
    /// everything the unified evaluator can reach.
    pub fn identifiers_in_scope(&self, _frame_id: usize) -> Vec<String> {
        let mut out: Vec<String> = self.visible_variables().keys().cloned().collect();
        out.extend(self.builtins.keys().cloned());
        out.extend(self.async_builtins.keys().cloned());
        out.sort();
        out.dedup();
        out
    }

    /// Get all stack frames for the debugger.
    pub fn debug_stack_frames(&self) -> Vec<(String, usize)> {
        self.debug_stack_frames_with_sources()
            .into_iter()
            .map(|(name, line, _source)| (name, line))
            .collect()
    }

    /// Get all stack frames plus their source keys for debugger clients that
    /// can retrieve synthetic sources through DAP `source`.
    pub fn debug_stack_frames_with_sources(&self) -> Vec<(String, usize, Option<String>)> {
        let mut frames = Vec::new();
        for (i, frame) in self.frames.iter().enumerate() {
            let line = if frame.ip > 0 && frame.ip - 1 < frame.chunk.lines.len() {
                frame.chunk.lines[frame.ip - 1] as usize
            } else {
                0
            };
            let name = if frame.fn_name.is_empty() {
                if i == 0 {
                    "pipeline".to_string()
                } else {
                    format!("fn_{}", i)
                }
            } else {
                frame.fn_name.clone()
            };
            frames.push((name, line, frame.chunk.source_file.clone()));
        }
        frames
    }

    /// Return cached source text by debugger source key. This covers entry
    /// programs, real imports that have already been read, and synthetic
    /// sources such as stdlib modules or generated in-memory modules.
    pub fn debug_source_for_path(&self, path: &str) -> Option<String> {
        if self.source_file.as_deref() == Some(path) {
            if let Some(source) = &self.source_text {
                return Some(source.clone());
            }
        }

        let key = std::path::PathBuf::from(path);
        if let Some(source) = self.source_cache.get(&key) {
            return Some(source.clone());
        }

        if let Some(module) = path
            .strip_prefix("<stdlib>/")
            .and_then(|s| s.strip_suffix(".harn"))
        {
            return crate::stdlib_modules::get_stdlib_source(module).map(str::to_string);
        }

        None
    }

    /// Get the current source line.
    pub(crate) fn current_line(&self) -> usize {
        if let Some(frame) = self.frames.last() {
            let ip = if frame.ip > 0 { frame.ip - 1 } else { 0 };
            if ip < frame.chunk.lines.len() {
                return frame.chunk.lines[ip] as usize;
            }
        }
        0
    }

    /// Execute one instruction, returning whether to stop (breakpoint/step).
    /// Returns Ok(None) to continue, Ok(Some(val)) on program end, Err on error.
    ///
    /// Line-change detection reads the line of the instruction we're
    /// *about to execute* (`lines[ip]`) rather than the byte before
    /// `ip`. After a jump, `ip-1` still points into the skipped region,
    /// which previously reported phantom stops on the tail of a
    /// not-taken branch (e.g. `host_metadata_save()` highlighted even
    /// though `any_stale` was false). Using `lines[ip]` — combined with
    /// cleanup ops emitted at line 0 after branch/loop exits — keeps
    /// the debugger aligned with what's actually going to run.
    pub async fn step_execute(&mut self) -> Result<Option<(VmValue, bool)>, VmError> {
        // Cooperative cancellation (#108): the DAP adapter flips the
        // shared flag when the IDE presses the Stop pill. Check here
        // before any instruction work so the loop unwinds promptly
        // on the next tick.
        if self.is_cancel_requested() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "kind:cancelled:VM cancelled by host",
            ))));
        }
        let current_line = self.upcoming_line();
        let line_changed = current_line != self.last_line && current_line > 0;

        if line_changed {
            self.last_line = current_line;

            let state = self.debug_state();
            if let Some(hook) = self.debug_hook.as_mut() {
                if matches!(hook(&state), DebugAction::Stop) {
                    self.stopped = true;
                    return Ok(Some((VmValue::Nil, true)));
                }
            }

            if self.breakpoint_matches(current_line) {
                self.stopped = true;
                return Ok(Some((VmValue::Nil, true)));
            }

            // Function-breakpoint latch: set by push_closure_frame when
            // the callee's name is in `function_breakpoints`. Stop with
            // the same shape as a line BP so the DAP adapter's
            // classify_breakpoint_hit emits a standard stopped event.
            if self.pending_function_bp.is_some() {
                self.stopped = true;
                return Ok(Some((VmValue::Nil, true)));
            }

            // step_frame_depth is the deepest frame count at which a stop
            // is acceptable. set_step_mode uses usize::MAX (any depth,
            // step-in), set_step_over uses N (same frame or shallower),
            // set_step_out uses N-1 (strictly shallower than where the
            // step-out was requested).
            if self.step_mode && self.frames.len() <= self.step_frame_depth {
                self.step_mode = false;
                self.stopped = true;
                return Ok(Some((VmValue::Nil, true)));
            }
        }

        self.stopped = false;
        self.execute_one_cycle().await
    }

    /// Line of the instruction *about to execute* — used by the
    /// debugger for line-change detection so the first cycle after a
    /// jump doesn't report a stale line from the skipped region.
    pub(crate) fn upcoming_line(&self) -> usize {
        if let Some(frame) = self.frames.last() {
            if frame.ip < frame.chunk.lines.len() {
                return frame.chunk.lines[frame.ip] as usize;
            }
        }
        0
    }

    /// Number of live call frames. Used by the DAP adapter to
    /// translate stackTrace ids (1-based, innermost first) back to
    /// the VM's 0-based outermost-first index when processing
    /// `restartFrame`.
    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    /// Rewind the given frame to its entry state so stepping resumes
    /// from the first instruction of the function with the original
    /// arguments re-bound. Higher frames above `frame_id` are dropped.
    /// Returns an error if the frame has no captured `initial_env`
    /// (scratch / evaluator frames don't) or if the id is out of range.
    ///
    /// Side effects already performed by the restarted frame (tool
    /// calls, file writes, host_call round-trips) are *not* rolled
    /// back — DAP leaves that to the adapter's discretion. The IDE
    /// should warn on frames whose source text contains obvious
    /// side-effectful calls before invoking restartFrame.
    pub fn restart_frame(&mut self, frame_id: usize) -> Result<(), VmError> {
        if frame_id >= self.frames.len() {
            return Err(VmError::Runtime(format!(
                "restartFrame: frame id {frame_id} out of range (have {} frames)",
                self.frames.len()
            )));
        }
        let Some(initial_env) = self.frames[frame_id].initial_env.clone() else {
            return Err(VmError::Runtime(
                "restartFrame: target frame was not captured for restart (scratch / evaluator frame)"
                    .into(),
            ));
        };
        let initial_local_slots = self.frames[frame_id].initial_local_slots.clone();
        // Drop every frame above the target. Each pop restores its
        // saved_iterator_depth into `self.iterators` so iterator state
        // unwinds consistently.
        while self.frames.len() > frame_id + 1 {
            let popped = self.frames.pop().expect("bounds checked above");
            self.iterators.truncate(popped.saved_iterator_depth);
        }
        // Rewind the target frame.
        let frame = self
            .frames
            .last_mut()
            .expect("frame_id within bounds guarantees a frame");
        frame.ip = 0;
        let stack_base = frame.stack_base;
        let saved_iter_depth = frame.saved_iterator_depth;
        self.stack.truncate(stack_base);
        self.iterators.truncate(saved_iter_depth);
        if let Some(initial_local_slots) = initial_local_slots {
            frame.local_slots = initial_local_slots;
            frame.local_scope_depth = 0;
        }
        self.env = initial_env;
        self.last_line = 0;
        self.stopped = false;
        Ok(())
    }

    /// Assign a new value to a named binding in the paused VM's env.
    /// Returns the value that was actually stored (after coercion, if
    /// the VM performed any) so the caller can echo it back to the
    /// DAP client. Fails if the name does not resolve to a mutable
    /// binding in any live scope.
    ///
    /// The provided `value_expr` goes through the unified evaluator so
    /// callers can type expressions like `plan.tasks.len() + 1` in the
    /// Locals inline-edit field, not just literals.
    pub async fn set_variable_in_frame(
        &mut self,
        name: &str,
        value_expr: &str,
        frame_id: usize,
    ) -> Result<VmValue, VmError> {
        let value = self.evaluate_in_frame(value_expr, frame_id).await?;
        // Debug-specific assign: bypasses the `let` immutability gate
        // because the user is explicitly editing in the IDE, and
        // almost every pipeline binding is `let`. The underlying
        // binding's mutability flag is preserved so runtime behavior
        // after the override is unchanged.
        if !self.assign_active_local_slot(name, value.clone(), true)? {
            self.env
                .assign_debug(name, value.clone())
                .map_err(|e| match e {
                    VmError::UndefinedVariable(n) => {
                        VmError::Runtime(format!("setVariable: '{n}' is not in the current scope"))
                    }
                    other => other,
                })?;
        }
        Ok(value)
    }

    /// Evaluate a Harn expression against the currently paused frame's
    /// scope and return its value. This is the single evaluation path
    /// used by hover tips, watch expressions, conditional breakpoints,
    /// logpoint interpolation, and `setVariable` / `setExpression`
    /// before we had a unified evaluator there were four separate
    /// mini-parsers, each with its own rough edges (see burin-code #85).
    ///
    /// The expression is wrapped as `let __r = (<expr>)` so arbitrary
    /// infix chains, ternaries, and access paths parse uniformly. A
    /// scratch `CallFrame` runs the wrapped bytecode with `saved_env`
    /// pointing at the caller's env, so the compiled expression sees
    /// every local in scope. When the scratch frame pops, the caller's
    /// env is automatically restored.
    ///
    /// A fixed instruction budget guards against runaway expressions
    /// (infinite loops, accidental recursion) wedging the debugger.
    /// Side effects — including `llm_call`, `host_*`, and file mutators
    /// — are not blocked here; callers that invoke this for read-only
    /// surfaces (hover, watch) should reject obviously-side-effectful
    /// expressions before calling.
    pub async fn evaluate_in_frame(
        &mut self,
        expr: &str,
        _frame_id: usize,
    ) -> Result<VmValue, VmError> {
        let trimmed = expr.trim();
        if trimmed.is_empty() {
            return Err(VmError::Runtime("evaluate: empty expression".into()));
        }

        // Wrap as a pipeline whose body *returns* the expression. The
        // explicit `return` compiles to `push value + Op::Return`, and
        // Op::Return's frame-exit path pushes that value onto the
        // caller's stack — which is where we read it from below.
        // Avoids the script-mode compile path that trails a Pop+Nil
        // sequence after every expression statement, which would
        // clobber the result before we could capture it.
        let wrapped = format!("pipeline default() {{\n  return ({trimmed})\n}}\n");
        let program = harn_parser::check_source_strict(&wrapped)
            .map_err(|e| VmError::Runtime(format!("evaluate: parse error: {e}")))?;
        let mut chunk = crate::compiler::Compiler::new()
            .compile(&program)
            .map_err(|e| VmError::Runtime(format!("evaluate: compile error: {e}")))?;
        // Inherit the current frame's source file so any runtime error
        // enriched with `(line N)` attributes cleanly.
        if let Some(current) = self.frames.last() {
            chunk.source_file = current.chunk.source_file.clone();
        }

        // Snapshot every piece of VM state the scratch frame could
        // perturb. Evaluation MUST be transparent: step state, scope
        // depth, iterator depth, and the line-change baseline all
        // restore on exit so the paused session continues exactly as
        // before the user typed an expression into the REPL.
        self.sync_current_frame_locals_to_env();
        let saved_stack_len = self.stack.len();
        let saved_frame_count = self.frames.len();
        let saved_iter_depth = self.iterators.len();
        let saved_scope_depth = self.env.scope_depth();
        let saved_last_line = self.last_line;
        let saved_step_mode = self.step_mode;
        let saved_step_frame_depth = self.step_frame_depth;
        let saved_stopped = self.stopped;
        let saved_env = self.env.clone();

        // Disable stepping during evaluation; otherwise the debug hook
        // would fire on every synthetic line and block the pause UI.
        self.step_mode = false;
        self.stopped = false;

        let local_slots = Self::fresh_local_slots(&chunk);
        self.frames.push(CallFrame {
            chunk: Rc::new(chunk),
            ip: 0,
            stack_base: saved_stack_len,
            saved_env,
            // Scratch evaluator frames never accept restartFrame — the
            // REPL/watch user expects read-only inspection semantics,
            // not replay — so skip the clone.
            initial_env: None,
            initial_local_slots: None,
            saved_iterator_depth: saved_iter_depth,
            fn_name: "<eval>".to_string(),
            argc: 0,
            saved_source_dir: self.source_dir.clone(),
            module_functions: None,
            module_state: None,
            local_slots,
            local_scope_base: self.env.scope_depth().saturating_sub(1),
            local_scope_depth: 0,
        });

        // Drive one op at a time with a fixed budget. A pure expression
        // is typically < 20 instructions; 10k gives plenty of headroom
        // for e.g. a list comprehension without letting a bad loop
        // hang the debugger forever.
        const MAX_EVAL_STEPS: usize = 10_000;
        let mut err: Option<VmError> = None;
        for _ in 0..MAX_EVAL_STEPS {
            if self.frames.len() <= saved_frame_count {
                break;
            }
            match self.execute_one_cycle().await {
                Ok(_) => {
                    if self.frames.len() <= saved_frame_count {
                        break;
                    }
                }
                Err(e) => {
                    err = Some(e);
                    break;
                }
            }
        }

        // Read the result before restoring the stack — frame exit
        // pushes the last-computed value onto the caller's stack, so
        // it sits at `saved_stack_len` if execution completed cleanly.
        let result = if self.stack.len() > saved_stack_len {
            Some(self.stack[saved_stack_len].clone())
        } else {
            None
        };

        // Unconditional cleanup so a mid-execution error doesn't leak
        // scratch state into the live session.
        self.frames.truncate(saved_frame_count);
        self.stack.truncate(saved_stack_len);
        self.iterators.truncate(saved_iter_depth);
        self.env.truncate_scopes(saved_scope_depth);
        self.last_line = saved_last_line;
        self.step_mode = saved_step_mode;
        self.step_frame_depth = saved_step_frame_depth;
        self.stopped = saved_stopped;

        if let Some(e) = err {
            return Err(e);
        }
        result.ok_or_else(|| {
            VmError::Runtime(
                "evaluate: step budget exceeded before the expression produced a value".into(),
            )
        })
    }
}
