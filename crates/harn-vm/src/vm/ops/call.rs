use std::rc::Rc;

use crate::chunk::{InlineCacheEntry, MethodCacheTarget, Op};
use crate::value::{VmClosure, VmError, VmValue};
use crate::BuiltinId;

use super::super::CallFrame;

impl super::super::Vm {
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
            if let Some(VmValue::TaskHandle(id)) = args.first() {
                if let Some(handle) = self.spawned_tasks.remove(id) {
                    handle.handle.abort();
                }
            }
            self.stack.push(VmValue::Nil);
            return Ok(true);
        }

        if name == "cancel_graceful" {
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
            return Ok(true);
        }

        if name == "is_cancelled" {
            let cancelled = self
                .cancel_token
                .as_ref()
                .map(|t| t.load(std::sync::atomic::Ordering::SeqCst))
                .unwrap_or(false);
            self.stack.push(VmValue::Bool(cancelled));
            return Ok(true);
        }

        Ok(false)
    }

    async fn call_named_value(
        &mut self,
        name: &str,
        args: Vec<VmValue>,
        functions: &[crate::chunk::CompiledFunction],
        direct_id: Option<BuiltinId>,
    ) -> Result<(), VmError> {
        if self.try_call_special_name(name, &args).await? {
            return Ok(());
        }
        if let Some(closure) = self.resolve_named_closure(name) {
            if closure.func.is_generator {
                let gen = self.create_generator(&closure, &args);
                self.stack.push(gen);
            } else {
                self.push_closure_frame(&closure, &args, functions)?;
            }
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
                    self.call_named_value(&name, args, &functions, None).await?;
                }
                VmValue::Closure(closure) => {
                    if closure.func.is_generator {
                        let gen = self.create_generator(&closure, &args);
                        self.stack.push(gen);
                    } else {
                        self.push_closure_frame(&closure, &args, &functions)?;
                    }
                }
                VmValue::BuiltinRef(name) => {
                    self.call_named_value(&name, args, &functions, None).await?;
                }
                VmValue::BuiltinRefId { id, name } => {
                    self.call_named_value(&name, args, &functions, Some(id))
                        .await?;
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
                    self.call_named_value(&name, args, &functions, None).await?;
                }
                VmValue::Closure(closure) => {
                    if closure.func.is_generator {
                        let gen = self.create_generator(&closure, &args);
                        self.stack.push(gen);
                    } else {
                        self.push_closure_frame(&closure, &args, &functions)?;
                    }
                }
                VmValue::BuiltinRef(name) => {
                    self.call_named_value(&name, args, &functions, None).await?;
                }
                VmValue::BuiltinRefId { id, name } => {
                    self.call_named_value(&name, args, &functions, Some(id))
                        .await?;
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
        } else if op == Op::CallBuiltin as u8 {
            let frame = self.frames.last_mut().unwrap();
            let id = BuiltinId::from_raw(frame.chunk.read_u64(frame.ip));
            frame.ip += 8;
            let name_idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let argc = frame.chunk.code[frame.ip] as usize;
            frame.ip += 1;
            let name = Self::const_string(&frame.chunk.constants[name_idx])?;
            let functions = frame.chunk.functions.clone();
            let args: Vec<VmValue> = self.stack.split_off(self.stack.len().saturating_sub(argc));
            self.call_named_value(&name, args, &functions, Some(id))
                .await?;
        } else if op == Op::CallBuiltinSpread as u8 {
            let frame = self.frames.last_mut().unwrap();
            let id = BuiltinId::from_raw(frame.chunk.read_u64(frame.ip));
            frame.ip += 8;
            let name_idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let name = Self::const_string(&frame.chunk.constants[name_idx])?;
            let functions = frame.chunk.functions.clone();
            let args_val = self.pop()?;
            let args = match args_val {
                VmValue::List(items) => (*items).clone(),
                _ => {
                    return Err(VmError::TypeError(
                        "spread call requires list arguments".into(),
                    ))
                }
            };
            self.call_named_value(&name, args, &functions, Some(id))
                .await?;
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
            } else if let Some(result) = Self::try_cached_method(&cache_entry, name_idx, argc, &obj)
            {
                self.stack.push(result);
            } else {
                let method = {
                    let frame = self.frames.last().unwrap();
                    Self::const_string(&frame.chunk.constants[name_idx as usize])?
                };
                let cache_target = Self::method_cache_target(&obj, &method, args.len());
                let functions = self.frames.last().unwrap().chunk.functions.clone();
                let result = self.call_method(obj, &method, &args, &functions).await?;
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
        } else if op == Op::MethodCallSpread as u8 {
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
            if let Some(result) = Self::try_cached_method(&cache_entry, name_idx, args.len(), &obj)
            {
                self.stack.push(result);
            } else {
                let method = {
                    let frame = self.frames.last().unwrap();
                    Self::const_string(&frame.chunk.constants[name_idx as usize])?
                };
                let cache_target = Self::method_cache_target(&obj, &method, args.len());
                let functions = self.frames.last().unwrap().chunk.functions.clone();
                let result = self.call_method(obj, &method, &args, &functions).await?;
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
        } else if op == Op::Pipe as u8 {
            let callable = self.pop()?;
            let value = self.pop()?;
            let functions = self.frames.last().unwrap().chunk.functions.clone();
            match callable {
                VmValue::Closure(closure) => {
                    self.push_closure_frame(&closure, &[value], &functions)?;
                }
                VmValue::String(name) => {
                    self.call_named_value(&name, vec![value], &functions, None)
                        .await?;
                }
                VmValue::BuiltinRef(name) => {
                    self.call_named_value(&name, vec![value], &functions, None)
                        .await?;
                }
                VmValue::BuiltinRefId { id, name } => {
                    self.call_named_value(&name, vec![value], &functions, Some(id))
                        .await?;
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
