use std::rc::Rc;

use crate::chunk::Op;
use crate::value::{VmClosure, VmError, VmValue};

use super::super::CallFrame;

impl super::super::Vm {
    pub(super) async fn try_execute_call_op(&mut self, op: u8) -> Result<bool, VmError> {
        if op == Op::Call as u8 {
            let frame = self.frames.last_mut().unwrap();
            let argc = frame.chunk.code[frame.ip] as usize;
            frame.ip += 1;
            // Avoid borrowing frame across the call.
            let functions = frame.chunk.functions.clone();

            let args: Vec<VmValue> = self.stack.split_off(self.stack.len().saturating_sub(argc));
            let callee = self.pop()?;

            match callee {
                VmValue::String(name) => {
                    if name.as_ref() == "await" {
                        let task_id = args.first().and_then(|a| match a {
                            VmValue::TaskHandle(id) => Some(id.clone()),
                            _ => None,
                        });
                        if let Some(id) = task_id {
                            if let Some(handle) = self.spawned_tasks.remove(&id) {
                                let (result, task_output) =
                                    handle.handle.await.map_err(|e| {
                                        VmError::Runtime(format!("Task join error: {e}"))
                                    })??;
                                self.output.push_str(&task_output);
                                self.stack.push(result);
                            } else {
                                self.stack.push(VmValue::Nil);
                            }
                        } else {
                            self.stack
                                .push(args.into_iter().next().unwrap_or(VmValue::Nil));
                        }
                    } else if name.as_ref() == "cancel" {
                        if let Some(VmValue::TaskHandle(id)) = args.first() {
                            if let Some(handle) = self.spawned_tasks.remove(id) {
                                handle.handle.abort();
                            }
                        }
                        self.stack.push(VmValue::Nil);
                    } else if name.as_ref() == "cancel_graceful" {
                        let task_id = args.first().and_then(|a| match a {
                            VmValue::TaskHandle(id) => Some(id.clone()),
                            _ => None,
                        });
                        let timeout_ms = args
                            .get(1)
                            .and_then(|a| match a {
                                VmValue::Int(n) => Some(*n as u64),
                                VmValue::Duration(ms) => Some(*ms),
                                _ => None,
                            })
                            .unwrap_or(5000);
                        if let Some(id) = task_id {
                            if let Some(task) = self.spawned_tasks.remove(&id) {
                                task.cancel_token
                                    .store(true, std::sync::atomic::Ordering::SeqCst);
                                let deadline = tokio::time::Instant::now()
                                    + tokio::time::Duration::from_millis(timeout_ms);
                                match tokio::time::timeout_at(deadline, task.handle).await {
                                    Ok(Ok(Ok((result, output)))) => {
                                        self.output.push_str(&output);
                                        self.stack.push(VmValue::EnumVariant {
                                            enum_name: "Result".into(),
                                            variant: "Ok".into(),
                                            fields: vec![result],
                                        });
                                    }
                                    Ok(Ok(Err(e))) => {
                                        self.stack.push(VmValue::EnumVariant {
                                            enum_name: "Result".into(),
                                            variant: "Err".into(),
                                            fields: vec![VmValue::String(Rc::from(e.to_string()))],
                                        });
                                    }
                                    Ok(Err(e)) => {
                                        self.stack.push(VmValue::EnumVariant {
                                            enum_name: "Result".into(),
                                            variant: "Err".into(),
                                            fields: vec![VmValue::String(Rc::from(format!(
                                                "Task join error: {e}"
                                            )))],
                                        });
                                    }
                                    Err(_) => {
                                        self.stack.push(VmValue::EnumVariant {
                                            enum_name: "Result".into(),
                                            variant: "Err".into(),
                                            fields: vec![VmValue::String(Rc::from(
                                                "cancel_graceful: timeout, task forcefully aborted",
                                            ))],
                                        });
                                    }
                                }
                            } else {
                                self.stack.push(VmValue::EnumVariant {
                                    enum_name: "Result".into(),
                                    variant: "Ok".into(),
                                    fields: vec![VmValue::Nil],
                                });
                            }
                        } else {
                            self.stack.push(VmValue::Nil);
                        }
                    } else if name.as_ref() == "is_cancelled" {
                        let cancelled = self
                            .cancel_token
                            .as_ref()
                            .map(|t| t.load(std::sync::atomic::Ordering::SeqCst))
                            .unwrap_or(false);
                        self.stack.push(VmValue::Bool(cancelled));
                    } else if let Some(closure) = self.resolve_named_closure(&name) {
                        if closure.func.is_generator {
                            let gen = self.create_generator(&closure, &args);
                            self.stack.push(gen);
                        } else {
                            self.push_closure_frame(&closure, &args, &functions)?;
                        }
                    } else {
                        let result = self.call_named_builtin(&name, args).await?;
                        self.stack.push(result);
                    }
                }
                VmValue::Closure(closure) => {
                    if closure.func.is_generator {
                        let gen = self.create_generator(&closure, &args);
                        self.stack.push(gen);
                    } else {
                        self.push_closure_frame(&closure, &args, &functions)?;
                    }
                }
                _ => {
                    return Err(VmError::TypeError(format!(
                        "Cannot call {}",
                        callee.display()
                    )));
                }
            }
        } else if op == Op::CallSpread as u8 {
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
            let functions = self.frames.last().unwrap().chunk.functions.clone();

            match callee {
                VmValue::String(name) => {
                    if name.as_ref() == "await" {
                        let task_id = args.first().and_then(|a| match a {
                            VmValue::TaskHandle(id) => Some(id.clone()),
                            _ => None,
                        });
                        if let Some(id) = task_id {
                            if let Some(handle) = self.spawned_tasks.remove(&id) {
                                let (result, task_output) =
                                    handle.handle.await.map_err(|e| {
                                        VmError::Runtime(format!("Task join error: {e}"))
                                    })??;
                                self.output.push_str(&task_output);
                                self.stack.push(result);
                            } else {
                                self.stack.push(VmValue::Nil);
                            }
                        } else {
                            self.stack
                                .push(args.into_iter().next().unwrap_or(VmValue::Nil));
                        }
                    } else if name.as_ref() == "cancel" {
                        if let Some(VmValue::TaskHandle(id)) = args.first() {
                            if let Some(handle) = self.spawned_tasks.remove(id) {
                                handle.handle.abort();
                            }
                        }
                        self.stack.push(VmValue::Nil);
                    } else if let Some(closure) = self.resolve_named_closure(&name) {
                        if closure.func.is_generator {
                            let gen = self.create_generator(&closure, &args);
                            self.stack.push(gen);
                        } else {
                            self.push_closure_frame(&closure, &args, &functions)?;
                        }
                    } else {
                        let result = self.call_named_builtin(&name, args).await?;
                        self.stack.push(result);
                    }
                }
                VmValue::Closure(closure) => {
                    if closure.func.is_generator {
                        let gen = self.create_generator(&closure, &args);
                        self.stack.push(gen);
                    } else {
                        self.push_closure_frame(&closure, &args, &functions)?;
                    }
                }
                _ => {
                    return Err(VmError::TypeError(format!(
                        "Cannot call {}",
                        callee.display()
                    )));
                }
            }
        } else if op == Op::TailCall as u8 {
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
                if closure.func.is_generator {
                    // Generators cannot be tail-call optimized.
                    let gen = self.create_generator(&closure, &args);
                    return Err(VmError::Return(gen));
                }
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

                // Pass parent env so closure_call_env merges locally-defined
                // recursive fns.
                let mut call_env = Self::closure_call_env(&parent_env, &closure);
                call_env.push_scope();
                let default_start = closure
                    .func
                    .default_start
                    .unwrap_or(closure.func.params.len());
                for (i, param) in closure.func.params.iter().enumerate() {
                    if i < args.len() {
                        call_env.define(param, args[i].clone(), false)?;
                    } else if i < default_start {
                        call_env.define(param, VmValue::Nil, false)?;
                    }
                    // else: has default, preamble will DefLet
                }
                let initial_env = call_env.clone();
                self.env = call_env;

                let argc = args.len();
                self.frames.push(CallFrame {
                    chunk: closure.func.chunk.clone(),
                    ip: 0,
                    stack_base,
                    saved_env: parent_env,
                    initial_env: Some(initial_env),
                    saved_iterator_depth: self.iterators.len(),
                    fn_name: closure.func.name.clone(),
                    argc,
                    saved_source_dir,
                    module_functions: closure.module_functions.clone(),
                    module_state: closure.module_state.clone(),
                });
            } else {
                match callee {
                    VmValue::String(name) => {
                        let result = self.call_named_builtin(&name, args).await?;
                        self.stack.push(result);
                    }
                    _ => {
                        return Err(VmError::TypeError(format!(
                            "Cannot call {}",
                            callee.display()
                        )));
                    }
                }
            }
        } else if op == Op::Return as u8 {
            let val = self.pop().unwrap_or(VmValue::Nil);
            return Err(VmError::Return(val));
        } else if op == Op::Closure as u8 {
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
                // Inherit module state so closures created inside a module
                // function see and mutate the same module-level vars.
                module_state: self
                    .frames
                    .last()
                    .and_then(|frame| frame.module_state.clone()),
            };
            self.stack.push(VmValue::Closure(Rc::new(closure)));
        } else if op == Op::MethodCall as u8 || op == Op::MethodCallOpt as u8 {
            let optional = op == Op::MethodCallOpt as u8;
            let frame = self.frames.last_mut().unwrap();
            let name_idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let argc = frame.chunk.code[frame.ip] as usize;
            frame.ip += 1;
            let method = Self::const_string(&frame.chunk.constants[name_idx])?;
            let functions = frame.chunk.functions.clone();
            let args: Vec<VmValue> = self.stack.split_off(self.stack.len().saturating_sub(argc));
            let obj = self.pop()?;
            if optional && matches!(obj, VmValue::Nil) {
                self.stack.push(VmValue::Nil);
            } else {
                let result = self.call_method(obj, &method, &args, &functions).await?;
                self.stack.push(result);
            }
        } else if op == Op::MethodCallSpread as u8 {
            let frame = self.frames.last_mut().unwrap();
            let name_idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let method = Self::const_string(&frame.chunk.constants[name_idx])?;
            let functions = frame.chunk.functions.clone();
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
            let result = self.call_method(obj, &method, &args, &functions).await?;
            self.stack.push(result);
        } else if op == Op::Pipe as u8 {
            let callable = self.pop()?;
            let value = self.pop()?;
            let functions = self.frames.last().unwrap().chunk.functions.clone();
            match callable {
                VmValue::Closure(closure) => {
                    self.push_closure_frame(&closure, &[value], &functions)?;
                }
                VmValue::String(name) => {
                    if let Some(VmValue::Closure(closure)) = self.env.get(&name) {
                        self.push_closure_frame(&closure, &[value], &functions)?;
                    } else {
                        let result = self.call_named_builtin(&name, vec![value]).await?;
                        self.stack.push(result);
                    }
                }
                _ => {
                    return Err(VmError::TypeError(format!(
                        "cannot pipe into {}",
                        callable.type_name()
                    )));
                }
            }
        } else {
            return Ok(false);
        }
        Ok(true)
    }
}
