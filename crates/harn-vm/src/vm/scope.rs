use std::rc::Rc;

use crate::value::{VmClosure, VmEnv, VmError, VmValue};

use super::{CallFrame, Vm};

impl Vm {
    pub(crate) const MAX_FRAMES: usize = 512;

    /// Build the call-time env for a closure invocation.
    ///
    /// Harn is **lexically scoped for data**: a closure sees exactly the
    /// data names it captured at creation time, plus its parameters,
    /// plus names from its originating module's `module_state`, plus
    /// the module-function registry. The caller's *data* locals are
    /// intentionally not visible — that would be dynamic scoping, which
    /// is neither what Harn's TS-flavored surface suggests to users nor
    /// something real stdlib code relies on.
    ///
    /// **Exception: closure-typed bindings.** Function *names* are
    /// late-bound, Python-`LOAD_GLOBAL`-style. When a local recursive
    /// fn is declared in a pipeline body (or inside another function),
    /// the closure is created BEFORE its own name is defined in the
    /// enclosing scope, so `closure.env` captures a snapshot that is
    /// missing the self-reference. To make `fn fact(n) { fact(n-1) }`
    /// work without a letrec trick, we merge closure-typed entries
    /// from the caller's scope stack — but only closure-typed ones.
    /// Data locals are never leaked across call boundaries, so the
    /// surprising "caller's variable magically visible in callee"
    /// semantic is ruled out.
    ///
    /// Imported module closures have `module_state` set, at which
    /// point the full lexical environment is already available via
    /// `closure.env` + `module_state`, and we skip the closure merge
    /// entirely as a fast path. This is the hot path for context-
    /// builder workloads (~65% of VM CPU before this optimization).
    pub(crate) fn closure_call_env(caller_env: &VmEnv, closure: &VmClosure) -> VmEnv {
        if closure.module_state.is_some() {
            return closure.env.clone();
        }
        let mut call_env = closure.env.clone();
        // Late-bind only closure-typed names from the caller — enough
        // for local recursive / mutually-recursive fns to self-reference
        // without leaking caller-local data into the callee.
        for scope in &caller_env.scopes {
            for (name, (val, mutable)) in &scope.vars {
                if matches!(val, VmValue::Closure(_)) && call_env.get(name).is_none() {
                    let _ = call_env.define(name, val.clone(), *mutable);
                }
            }
        }
        call_env
    }

    pub(crate) fn resolve_named_closure(&self, name: &str) -> Option<Rc<VmClosure>> {
        if let Some(VmValue::Closure(closure)) = self.env.get(name) {
            return Some(closure);
        }
        self.frames
            .last()
            .and_then(|frame| frame.module_functions.as_ref())
            .and_then(|registry| registry.borrow().get(name).cloned())
    }

    /// Push a new call frame for a closure invocation.
    pub(crate) fn push_closure_frame(
        &mut self,
        closure: &VmClosure,
        args: &[VmValue],
    ) -> Result<(), VmError> {
        if self.frames.len() >= Self::MAX_FRAMES {
            return Err(VmError::StackOverflow);
        }
        let saved_env = self.env.clone();

        // If this closure originated from an imported module, switch
        // the thread-local source dir so that render() and other
        // source-relative builtins resolve relative to the module.
        let saved_source_dir = if let Some(ref dir) = closure.source_dir {
            let prev = crate::stdlib::process::VM_SOURCE_DIR.with(|sd| sd.borrow().clone());
            crate::stdlib::set_thread_source_dir(dir);
            prev
        } else {
            None
        };

        let mut call_env = Self::closure_call_env(&saved_env, closure);
        call_env.push_scope();

        let default_start = closure
            .func
            .default_start
            .unwrap_or(closure.func.params.len());
        let param_count = closure.func.params.len();
        for (i, param) in closure.func.params.iter().enumerate() {
            if closure.func.has_rest_param && i == param_count - 1 {
                // Rest parameter: collect remaining args into a list
                let rest_args = if i < args.len() {
                    args[i..].to_vec()
                } else {
                    Vec::new()
                };
                let _ = call_env.define(param, VmValue::List(std::rc::Rc::new(rest_args)), false);
            } else if i < args.len() {
                let _ = call_env.define(param, args[i].clone(), false);
            } else if i < default_start {
                let _ = call_env.define(param, VmValue::Nil, false);
            }
        }

        // Snapshot the env *after* argument binding so restartFrame
        // can rewind this function to its entry state with the same
        // args re-applied. Cheap relative to the call itself.
        let initial_env = call_env.clone();
        self.env = call_env;

        // Function-name breakpoint latch: record the name so the step
        // loop can raise a single "function breakpoint" stop on the
        // next cycle. We latch instead of stopping inline because
        // push_closure_frame is called from deep inside the call
        // dispatcher — the cleanest place for the debugger to observe
        // a consistent state is at the next line-change check.
        if self.function_breakpoints.contains(&closure.func.name) {
            self.pending_function_bp = Some(closure.func.name.clone());
        }

        self.frames.push(CallFrame {
            chunk: Rc::clone(&closure.func.chunk),
            ip: 0,
            stack_base: self.stack.len(),
            saved_env,
            initial_env: Some(initial_env),
            saved_iterator_depth: self.iterators.len(),
            fn_name: closure.func.name.clone(),
            argc: args.len(),
            saved_source_dir,
            module_functions: closure.module_functions.clone(),
            module_state: closure.module_state.clone(),
        });

        Ok(())
    }

    /// Create a generator value by spawning the closure body as an async task.
    /// The generator body communicates yielded values through an mpsc channel.
    pub(crate) fn create_generator(&self, closure: &VmClosure, args: &[VmValue]) -> VmValue {
        use crate::value::VmGenerator;

        // Buffer size of 1: the generator produces one value at a time.
        let (tx, rx) = tokio::sync::mpsc::channel::<VmValue>(1);

        let mut child = self.child_vm();
        child.yield_sender = Some(tx);

        // Set up the environment for the generator body. The generator
        // body runs in its own child VM; closure_call_env walks the
        // current (parent) env so locally-defined generator closures
        // can self-reference via the narrow closure-only merge. See
        // `Vm::closure_call_env`.
        let parent_env = self.env.clone();
        let mut call_env = Self::closure_call_env(&parent_env, closure);
        call_env.push_scope();

        let default_start = closure
            .func
            .default_start
            .unwrap_or(closure.func.params.len());
        let param_count = closure.func.params.len();
        for (i, param) in closure.func.params.iter().enumerate() {
            if closure.func.has_rest_param && i == param_count - 1 {
                let rest_args = if i < args.len() {
                    args[i..].to_vec()
                } else {
                    Vec::new()
                };
                let _ = call_env.define(param, VmValue::List(std::rc::Rc::new(rest_args)), false);
            } else if i < args.len() {
                let _ = call_env.define(param, args[i].clone(), false);
            } else if i < default_start {
                let _ = call_env.define(param, VmValue::Nil, false);
            }
        }
        child.env = call_env;

        let chunk = Rc::clone(&closure.func.chunk);
        let saved_source_dir = if let Some(ref dir) = closure.source_dir {
            let prev = crate::stdlib::process::VM_SOURCE_DIR.with(|sd| sd.borrow().clone());
            crate::stdlib::set_thread_source_dir(dir);
            prev
        } else {
            None
        };
        let module_functions = closure.module_functions.clone();
        let module_state = closure.module_state.clone();
        let argc = args.len();
        // Spawn the generator body as an async task.
        // The task will execute until return, sending yielded values through the channel.
        tokio::task::spawn_local(async move {
            let _ = child
                .run_chunk_ref(
                    chunk,
                    argc,
                    saved_source_dir,
                    module_functions,
                    module_state,
                )
                .await;
            // When the generator body finishes (return or fall-through),
            // the sender is dropped, signaling completion to the receiver.
        });

        VmValue::Generator(VmGenerator {
            done: Rc::new(std::cell::Cell::new(false)),
            receiver: Rc::new(tokio::sync::Mutex::new(rx)),
        })
    }
}
