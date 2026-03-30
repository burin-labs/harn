use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use crate::value::VmTaskHandle;

use crate::chunk::{Constant, Op};
use crate::value::{compare_values, values_equal, VmClosure, VmError, VmValue};

use super::{CallFrame, ExceptionHandler};

impl super::Vm {
    /// Execute a single opcode. Returns:
    /// - Ok(None): continue execution
    /// - Ok(Some(val)): return this value (top-level exit)
    /// - Err(e): error occurred
    pub(super) async fn execute_op(&mut self, op: u8) -> Result<Option<VmValue>, VmError> {
        // We need to borrow frame fields, but we also need &mut self for other ops.
        // Strategy: read what we need from the frame first, then do the work.

        if op == Op::Constant as u8 {
            let frame = self.frames.last_mut().unwrap();
            let idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let val = match &frame.chunk.constants[idx] {
                Constant::Int(n) => VmValue::Int(*n),
                Constant::Float(n) => VmValue::Float(*n),
                Constant::String(s) => VmValue::String(Rc::from(s.as_str())),
                Constant::Bool(b) => VmValue::Bool(*b),
                Constant::Nil => VmValue::Nil,
                Constant::Duration(ms) => VmValue::Duration(*ms),
            };
            self.stack.push(val);
        } else if op == Op::Nil as u8 {
            self.stack.push(VmValue::Nil);
        } else if op == Op::True as u8 {
            self.stack.push(VmValue::Bool(true));
        } else if op == Op::False as u8 {
            self.stack.push(VmValue::Bool(false));
        } else if op == Op::GetVar as u8 {
            let frame = self.frames.last_mut().unwrap();
            let idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let name = match &frame.chunk.constants[idx] {
                Constant::String(s) => s.clone(),
                _ => return Err(VmError::TypeError("expected string constant".into())),
            };
            if let Some(val) = self.env.get(&name) {
                self.stack.push(val);
            } else if let Some(val) = self.globals.get(&name) {
                self.stack.push(val.clone());
            } else {
                let mut all_vars = self.env.all_variables();
                for (k, v) in &self.globals {
                    all_vars.entry(k.clone()).or_insert_with(|| v.clone());
                }
                if let Some(suggestion) =
                    crate::value::closest_match(&name, all_vars.keys().map(|s| s.as_str()))
                {
                    return Err(VmError::Runtime(format!(
                        "Undefined variable: {name} (did you mean `{suggestion}`?)"
                    )));
                }
                return Err(VmError::UndefinedVariable(name));
            }
        } else if op == Op::DefLet as u8 {
            let frame = self.frames.last_mut().unwrap();
            let idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let name = Self::const_string(&frame.chunk.constants[idx])?;
            let val = self.pop()?;
            self.env.define(&name, val, false)?;
        } else if op == Op::DefVar as u8 {
            let frame = self.frames.last_mut().unwrap();
            let idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let name = Self::const_string(&frame.chunk.constants[idx])?;
            let val = self.pop()?;
            self.env.define(&name, val, true)?;
        } else if op == Op::SetVar as u8 {
            let frame = self.frames.last_mut().unwrap();
            let idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let name = Self::const_string(&frame.chunk.constants[idx])?;
            let val = self.pop()?;
            self.env.assign(&name, val)?;
        } else if op == Op::Add as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            self.stack.push(self.add(a, b));
        } else if op == Op::Sub as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            self.stack.push(self.sub(a, b));
        } else if op == Op::Mul as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            self.stack.push(self.mul(a, b));
        } else if op == Op::Div as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            self.stack.push(self.div(a, b)?);
        } else if op == Op::Mod as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            self.stack.push(self.modulo(a, b)?);
        } else if op == Op::Negate as u8 {
            let v = self.pop()?;
            self.stack.push(match v {
                VmValue::Int(n) => VmValue::Int(n.wrapping_neg()),
                VmValue::Float(n) => VmValue::Float(-n),
                _ => {
                    return Err(VmError::Runtime(format!(
                        "Cannot negate value of type {}",
                        v.type_name()
                    )))
                }
            });
        } else if op == Op::Equal as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            self.stack.push(VmValue::Bool(values_equal(&a, &b)));
        } else if op == Op::NotEqual as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            self.stack.push(VmValue::Bool(!values_equal(&a, &b)));
        } else if op == Op::Less as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            self.stack.push(VmValue::Bool(compare_values(&a, &b) < 0));
        } else if op == Op::Greater as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            self.stack.push(VmValue::Bool(compare_values(&a, &b) > 0));
        } else if op == Op::LessEqual as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            self.stack.push(VmValue::Bool(compare_values(&a, &b) <= 0));
        } else if op == Op::GreaterEqual as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            self.stack.push(VmValue::Bool(compare_values(&a, &b) >= 0));
        } else if op == Op::Contains as u8 {
            let collection = self.pop()?;
            let item = self.pop()?;
            let result = match &collection {
                VmValue::List(items) => items.iter().any(|v| values_equal(v, &item)),
                VmValue::Dict(map) => {
                    let key = item.display();
                    map.contains_key(&key)
                }
                VmValue::Set(items) => items.iter().any(|v| values_equal(v, &item)),
                VmValue::String(s) => {
                    if let VmValue::String(substr) = &item {
                        s.contains(&**substr)
                    } else {
                        let substr = item.display();
                        s.contains(&substr)
                    }
                }
                _ => false,
            };
            self.stack.push(VmValue::Bool(result));
        } else if op == Op::Not as u8 {
            let v = self.pop()?;
            self.stack.push(VmValue::Bool(!v.is_truthy()));
        } else if op == Op::Jump as u8 {
            let frame = self.frames.last_mut().unwrap();
            let target = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip = target;
        } else if op == Op::JumpIfFalse as u8 {
            let frame = self.frames.last_mut().unwrap();
            let target = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let val = self.peek()?;
            if !val.is_truthy() {
                let frame = self.frames.last_mut().unwrap();
                frame.ip = target;
            }
        } else if op == Op::JumpIfTrue as u8 {
            let frame = self.frames.last_mut().unwrap();
            let target = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let val = self.peek()?;
            if val.is_truthy() {
                let frame = self.frames.last_mut().unwrap();
                frame.ip = target;
            }
        } else if op == Op::Pop as u8 {
            self.pop()?;
        } else if op == Op::Call as u8 {
            let frame = self.frames.last_mut().unwrap();
            let argc = frame.chunk.code[frame.ip] as usize;
            frame.ip += 1;
            // Clone the functions list so we don't borrow frame across call
            let functions = frame.chunk.functions.clone();

            // Arguments are on stack above the function name/value
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
                        // Signal cancellation and wait for task to finish (with optional timeout)
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
                                // Signal cancellation
                                task.cancel_token
                                    .store(true, std::sync::atomic::Ordering::SeqCst);
                                // Wait with timeout
                                let deadline = tokio::time::Instant::now()
                                    + tokio::time::Duration::from_millis(timeout_ms);
                                match tokio::time::timeout_at(deadline, task.handle).await {
                                    Ok(Ok(Ok((result, output)))) => {
                                        self.output.push_str(&output);
                                        // Return Result.Ok(value)
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
                                        // Timeout: force abort
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
                        // Check if the current task has been signaled for cancellation.
                        let cancelled = self
                            .cancel_token
                            .as_ref()
                            .map(|t| t.load(std::sync::atomic::Ordering::SeqCst))
                            .unwrap_or(false);
                        self.stack.push(VmValue::Bool(cancelled));
                    } else if let Some(VmValue::Closure(closure)) = self.env.get(&name) {
                        // Check closures in env
                        if closure.func.is_generator {
                            let gen = self.create_generator(&closure, &args);
                            self.stack.push(gen);
                        } else {
                            self.push_closure_frame(&closure, &args, &functions)?;
                        }
                        // Don't push result - frame will handle it on return
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
                    } else if let Some(VmValue::Closure(closure)) = self.env.get(&name) {
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

            // Resolve the callee to a closure (or fall through to builtin)
            let resolved_closure = match &callee {
                VmValue::Closure(cl) => Some(Rc::clone(cl)),
                VmValue::String(name) => {
                    if let Some(VmValue::Closure(cl)) = self.env.get(name) {
                        Some(cl)
                    } else {
                        None
                    }
                }
                _ => None,
            };

            if let Some(closure) = resolved_closure {
                if closure.func.is_generator {
                    // Generator functions cannot be tail-call optimized; return the generator.
                    let gen = self.create_generator(&closure, &args);
                    return Err(VmError::Return(gen));
                }
                // Tail call optimization: replace current frame instead of pushing.
                // Pop the current frame and reuse its stack_base and saved_env.
                let popped = self.frames.pop().unwrap();
                let stack_base = popped.stack_base;
                let parent_env = popped.saved_env;

                // Clear this frame's stack data
                self.stack.truncate(stack_base);

                // Set up the callee's environment
                let mut call_env = Self::merge_env_into_closure(&parent_env, &closure);
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
                self.env = call_env;

                // Push replacement frame at the same stack depth
                let argc = args.len();
                self.frames.push(CallFrame {
                    chunk: closure.func.chunk.clone(),
                    ip: 0,
                    stack_base,
                    saved_env: parent_env,
                    fn_name: closure.func.name.clone(),
                    argc,
                });
                // Continue the loop — execution proceeds in the new frame
            } else {
                // Not a closure — fall back to regular call behavior for builtins.
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
                // Result is on stack; the following Return opcode will return it.
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
            };
            self.stack.push(VmValue::Closure(Rc::new(closure)));
        } else if op == Op::BuildList as u8 {
            let frame = self.frames.last_mut().unwrap();
            let count = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let items = self.stack.split_off(self.stack.len().saturating_sub(count));
            self.stack.push(VmValue::List(Rc::new(items)));
        } else if op == Op::BuildDict as u8 {
            let frame = self.frames.last_mut().unwrap();
            let count = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let pairs = self
                .stack
                .split_off(self.stack.len().saturating_sub(count * 2));
            let mut map = BTreeMap::new();
            for pair in pairs.chunks(2) {
                if pair.len() == 2 {
                    let key = pair[0].display();
                    map.insert(key, pair[1].clone());
                }
            }
            self.stack.push(VmValue::Dict(Rc::new(map)));
        } else if op == Op::Subscript as u8 {
            let idx = self.pop()?;
            let obj = self.pop()?;
            let result = match (&obj, &idx) {
                (VmValue::List(items), VmValue::Int(i)) => {
                    if *i < 0 {
                        let pos = items.len() as i64 + *i;
                        if pos < 0 {
                            VmValue::Nil
                        } else {
                            items.get(pos as usize).cloned().unwrap_or(VmValue::Nil)
                        }
                    } else {
                        items.get(*i as usize).cloned().unwrap_or(VmValue::Nil)
                    }
                }
                (VmValue::Dict(map), _) => map.get(&idx.display()).cloned().unwrap_or(VmValue::Nil),
                (VmValue::String(s), VmValue::Int(i)) => {
                    if *i < 0 {
                        let pos = s.chars().count() as i64 + *i;
                        if pos < 0 {
                            VmValue::Nil
                        } else {
                            s.chars()
                                .nth(pos as usize)
                                .map(|c| VmValue::String(Rc::from(c.to_string())))
                                .unwrap_or(VmValue::Nil)
                        }
                    } else {
                        s.chars()
                            .nth(*i as usize)
                            .map(|c| VmValue::String(Rc::from(c.to_string())))
                            .unwrap_or(VmValue::Nil)
                    }
                }
                _ => {
                    return Err(VmError::TypeError(format!(
                        "cannot index into {} with {}",
                        obj.type_name(),
                        idx.type_name()
                    )));
                }
            };
            self.stack.push(result);
        } else if op == Op::Slice as u8 {
            let end_val = self.pop()?;
            let start_val = self.pop()?;
            let obj = self.pop()?;

            let result = match &obj {
                VmValue::List(items) => {
                    let len = items.len() as i64;
                    let start = match &start_val {
                        VmValue::Nil => 0i64,
                        VmValue::Int(i) => {
                            if *i < 0 {
                                (len + *i).max(0)
                            } else {
                                (*i).min(len)
                            }
                        }
                        _ => {
                            return Err(VmError::TypeError(format!(
                                "slice start must be an integer, got {}",
                                start_val.type_name()
                            )));
                        }
                    };
                    let end = match &end_val {
                        VmValue::Nil => len,
                        VmValue::Int(i) => {
                            if *i < 0 {
                                (len + *i).max(0)
                            } else {
                                (*i).min(len)
                            }
                        }
                        _ => {
                            return Err(VmError::TypeError(format!(
                                "slice end must be an integer, got {}",
                                end_val.type_name()
                            )));
                        }
                    };
                    if start >= end {
                        VmValue::List(Rc::new(vec![]))
                    } else {
                        let sliced: Vec<VmValue> = items[start as usize..end as usize].to_vec();
                        VmValue::List(Rc::new(sliced))
                    }
                }
                VmValue::String(s) => {
                    let chars: Vec<char> = s.chars().collect();
                    let len = chars.len() as i64;
                    let start = match &start_val {
                        VmValue::Nil => 0i64,
                        VmValue::Int(i) => {
                            if *i < 0 {
                                (len + *i).max(0)
                            } else {
                                (*i).min(len)
                            }
                        }
                        _ => {
                            return Err(VmError::TypeError(format!(
                                "slice start must be an integer, got {}",
                                start_val.type_name()
                            )));
                        }
                    };
                    let end = match &end_val {
                        VmValue::Nil => len,
                        VmValue::Int(i) => {
                            if *i < 0 {
                                (len + *i).max(0)
                            } else {
                                (*i).min(len)
                            }
                        }
                        _ => {
                            return Err(VmError::TypeError(format!(
                                "slice end must be an integer, got {}",
                                end_val.type_name()
                            )));
                        }
                    };
                    if start >= end {
                        VmValue::String(Rc::from(String::new()))
                    } else {
                        let sliced: String = chars[start as usize..end as usize].iter().collect();
                        VmValue::String(Rc::from(sliced))
                    }
                }
                _ => {
                    return Err(VmError::TypeError(format!(
                        "cannot slice {}",
                        obj.type_name()
                    )));
                }
            };
            self.stack.push(result);
        } else if op == Op::GetProperty as u8 {
            let frame = self.frames.last_mut().unwrap();
            let idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let name = Self::const_string(&frame.chunk.constants[idx])?;
            let obj = self.pop()?;
            let result = match &obj {
                VmValue::Dict(map) => map.get(&name).cloned().unwrap_or(VmValue::Nil),
                VmValue::List(items) => match name.as_str() {
                    "count" => VmValue::Int(items.len() as i64),
                    "empty" => VmValue::Bool(items.is_empty()),
                    "first" => items.first().cloned().unwrap_or(VmValue::Nil),
                    "last" => items.last().cloned().unwrap_or(VmValue::Nil),
                    _ => VmValue::Nil,
                },
                VmValue::String(s) => match name.as_str() {
                    "count" => VmValue::Int(s.chars().count() as i64),
                    "empty" => VmValue::Bool(s.is_empty()),
                    _ => VmValue::Nil,
                },
                VmValue::EnumVariant {
                    variant, fields, ..
                } => match name.as_str() {
                    "variant" => VmValue::String(Rc::from(variant.as_str())),
                    "fields" => VmValue::List(Rc::new(fields.clone())),
                    _ => VmValue::Nil,
                },
                VmValue::StructInstance { fields, .. } => {
                    fields.get(&name).cloned().unwrap_or(VmValue::Nil)
                }
                VmValue::Nil => {
                    return Err(VmError::TypeError(format!(
                        "cannot access property `{name}` on nil"
                    )));
                }
                _ => {
                    return Err(VmError::TypeError(format!(
                        "cannot access property `{name}` on {}",
                        obj.type_name()
                    )));
                }
            };
            self.stack.push(result);
        } else if op == Op::GetPropertyOpt as u8 {
            // Optional chaining: obj?.property — returns nil if obj is nil
            let frame = self.frames.last_mut().unwrap();
            let idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let name = Self::const_string(&frame.chunk.constants[idx])?;
            let obj = self.pop()?;
            let result = match &obj {
                VmValue::Nil => VmValue::Nil,
                VmValue::Dict(map) => map.get(&name).cloned().unwrap_or(VmValue::Nil),
                VmValue::List(items) => match name.as_str() {
                    "count" => VmValue::Int(items.len() as i64),
                    "empty" => VmValue::Bool(items.is_empty()),
                    "first" => items.first().cloned().unwrap_or(VmValue::Nil),
                    "last" => items.last().cloned().unwrap_or(VmValue::Nil),
                    _ => VmValue::Nil,
                },
                VmValue::String(s) => match name.as_str() {
                    "count" => VmValue::Int(s.chars().count() as i64),
                    "empty" => VmValue::Bool(s.is_empty()),
                    _ => VmValue::Nil,
                },
                VmValue::EnumVariant {
                    variant, fields, ..
                } => match name.as_str() {
                    "variant" => VmValue::String(Rc::from(variant.as_str())),
                    "fields" => VmValue::List(Rc::new(fields.clone())),
                    _ => VmValue::Nil,
                },
                VmValue::StructInstance { fields, .. } => {
                    fields.get(&name).cloned().unwrap_or(VmValue::Nil)
                }
                _ => VmValue::Nil,
            };
            self.stack.push(result);
        } else if op == Op::SetProperty as u8 {
            let frame = self.frames.last_mut().unwrap();
            let prop_idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let var_idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let prop_name = Self::const_string(&frame.chunk.constants[prop_idx])?;
            let var_name = Self::const_string(&frame.chunk.constants[var_idx])?;
            let new_value = self.pop()?;
            if let Some(obj) = self.env.get(&var_name) {
                match obj {
                    VmValue::Dict(map) => {
                        let mut new_map = (*map).clone();
                        new_map.insert(prop_name, new_value);
                        self.env
                            .assign(&var_name, VmValue::Dict(Rc::new(new_map)))?;
                    }
                    VmValue::StructInstance {
                        struct_name,
                        fields,
                    } => {
                        let mut new_fields = fields.clone();
                        new_fields.insert(prop_name, new_value);
                        self.env.assign(
                            &var_name,
                            VmValue::StructInstance {
                                struct_name,
                                fields: new_fields,
                            },
                        )?;
                    }
                    _ => {
                        return Err(VmError::TypeError(format!(
                            "cannot set property `{prop_name}` on {}",
                            obj.type_name()
                        )));
                    }
                }
            }
        } else if op == Op::SetSubscript as u8 {
            let frame = self.frames.last_mut().unwrap();
            let var_idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let var_name = Self::const_string(&frame.chunk.constants[var_idx])?;
            let index = self.pop()?;
            let new_value = self.pop()?;
            if let Some(obj) = self.env.get(&var_name) {
                match obj {
                    VmValue::List(items) => {
                        if let Some(i) = index.as_int() {
                            let mut new_items = (*items).clone();
                            let idx = if i < 0 {
                                (new_items.len() as i64 + i).max(0) as usize
                            } else {
                                i as usize
                            };
                            if idx < new_items.len() {
                                new_items[idx] = new_value;
                                self.env
                                    .assign(&var_name, VmValue::List(Rc::new(new_items)))?;
                            } else {
                                return Err(VmError::Runtime(format!(
                                    "Index {} out of bounds for list of length {}",
                                    i,
                                    items.len()
                                )));
                            }
                        }
                    }
                    VmValue::Dict(map) => {
                        let key = index.display();
                        let mut new_map = (*map).clone();
                        new_map.insert(key, new_value);
                        self.env
                            .assign(&var_name, VmValue::Dict(Rc::new(new_map)))?;
                    }
                    _ => {}
                }
            }
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
        } else if op == Op::Concat as u8 {
            let frame = self.frames.last_mut().unwrap();
            let count = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let parts = self.stack.split_off(self.stack.len().saturating_sub(count));
            let result: String = parts.iter().map(|p| p.display()).collect();
            self.stack.push(VmValue::String(Rc::from(result)));
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
        } else if op == Op::TryUnwrap as u8 {
            // Try-unwrap: if Result.Ok(v) → push v, if Result.Err(e) → return Result.Err(e)
            let val = self.pop()?;
            match &val {
                VmValue::EnumVariant {
                    enum_name,
                    variant,
                    fields,
                } if enum_name == "Result" => {
                    if variant == "Ok" {
                        self.stack
                            .push(fields.first().cloned().unwrap_or(VmValue::Nil));
                    } else {
                        // Err variant: return it from current function
                        return Err(VmError::Return(val));
                    }
                }
                other => {
                    return Err(VmError::TypeError(format!(
                        "? operator requires a Result value, got {}",
                        other.type_name()
                    )));
                }
            }
        } else if op == Op::Dup as u8 {
            let val = self.peek()?.clone();
            self.stack.push(val);
        } else if op == Op::Swap as u8 {
            let len = self.stack.len();
            if len >= 2 {
                self.stack.swap(len - 1, len - 2);
            }
        } else if op == Op::CheckType as u8 {
            let frame = self.frames.last_mut().unwrap();
            let var_idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let type_idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let var_name = match &frame.chunk.constants[var_idx] {
                Constant::String(s) => s.clone(),
                _ => return Err(VmError::TypeError("expected string constant".into())),
            };
            let expected_type = match &frame.chunk.constants[type_idx] {
                Constant::String(s) => s.clone(),
                _ => return Err(VmError::TypeError("expected string constant".into())),
            };
            if let Some(val) = self.env.get(&var_name) {
                let actual_type = val.type_name();
                let compatible = actual_type == expected_type
                    || (expected_type == "float" && actual_type == "int")
                    || (expected_type == "int" && actual_type == "float");
                if !compatible {
                    return Err(VmError::Runtime(format!(
                        "TypeError: parameter '{}' expected {}, got {} ({})",
                        var_name,
                        expected_type,
                        actual_type,
                        val.display()
                    )));
                }
            }
        } else if op == Op::IterInit as u8 {
            let iterable = self.pop()?;
            match iterable {
                VmValue::List(items) => {
                    self.iterators.push(super::IterState::Vec {
                        items: (*items).clone(),
                        idx: 0,
                    });
                }
                VmValue::Dict(map) => {
                    let items: Vec<VmValue> = map
                        .iter()
                        .map(|(k, v)| {
                            VmValue::Dict(Rc::new(BTreeMap::from([
                                ("key".to_string(), VmValue::String(Rc::from(k.as_str()))),
                                ("value".to_string(), v.clone()),
                            ])))
                        })
                        .collect();
                    self.iterators.push(super::IterState::Vec { items, idx: 0 });
                }
                VmValue::Set(items) => {
                    self.iterators.push(super::IterState::Vec {
                        items: (*items).clone(),
                        idx: 0,
                    });
                }
                VmValue::Channel(ch) => {
                    self.iterators.push(super::IterState::Channel {
                        receiver: ch.receiver.clone(),
                        closed: ch.closed.clone(),
                    });
                }
                VmValue::Generator(gen) => {
                    self.iterators.push(super::IterState::Generator { gen });
                }
                _ => {
                    self.iterators.push(super::IterState::Vec {
                        items: Vec::new(),
                        idx: 0,
                    });
                }
            }
        } else if op == Op::IterNext as u8 {
            let frame = self.frames.last_mut().unwrap();
            let target = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            if let Some(state) = self.iterators.last_mut() {
                match state {
                    super::IterState::Vec { items, idx } => {
                        if *idx < items.len() {
                            let item = items[*idx].clone();
                            *idx += 1;
                            self.stack.push(item);
                        } else {
                            self.iterators.pop();
                            let frame = self.frames.last_mut().unwrap();
                            frame.ip = target;
                        }
                    }
                    super::IterState::Channel { receiver, closed } => {
                        let rx = receiver.clone();
                        let is_closed = closed.load(std::sync::atomic::Ordering::Relaxed);
                        let mut guard = rx.lock().await;
                        // If sender is closed, drain remaining items without blocking
                        let item = if is_closed {
                            guard.try_recv().ok()
                        } else {
                            guard.recv().await
                        };
                        match item {
                            Some(val) => {
                                self.stack.push(val);
                            }
                            None => {
                                drop(guard);
                                self.iterators.pop();
                                let frame = self.frames.last_mut().unwrap();
                                frame.ip = target;
                            }
                        }
                    }
                    super::IterState::Generator { gen } => {
                        if gen.done.get() {
                            self.iterators.pop();
                            let frame = self.frames.last_mut().unwrap();
                            frame.ip = target;
                        } else {
                            let rx = gen.receiver.clone();
                            let mut guard = rx.lock().await;
                            match guard.recv().await {
                                Some(val) => {
                                    self.stack.push(val);
                                }
                                None => {
                                    gen.done.set(true);
                                    drop(guard);
                                    self.iterators.pop();
                                    let frame = self.frames.last_mut().unwrap();
                                    frame.ip = target;
                                }
                            }
                        }
                    }
                }
            } else {
                let frame = self.frames.last_mut().unwrap();
                frame.ip = target;
            }
        } else if op == Op::PopIterator as u8 {
            self.iterators.pop();
        } else if op == Op::Throw as u8 {
            let val = self.pop()?;
            return Err(VmError::Thrown(val));
        } else if op == Op::TryCatchSetup as u8 {
            let frame = self.frames.last_mut().unwrap();
            let catch_offset = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            // Read the error type name index (extra u16)
            let type_idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let error_type = match &frame.chunk.constants[type_idx] {
                Constant::String(s) => s.clone(),
                _ => String::new(),
            };
            self.exception_handlers.push(ExceptionHandler {
                catch_ip: catch_offset,
                stack_depth: self.stack.len(),
                frame_depth: self.frames.len(),
                error_type,
            });
        } else if op == Op::PopHandler as u8 {
            self.exception_handlers.pop();
        } else if op == Op::Parallel as u8 {
            let _par_span =
                super::ScopeSpan::new(crate::tracing::SpanKind::Parallel, "parallel".into());
            let closure = self.pop()?;
            let count_val = self.pop()?;
            let count = match &count_val {
                VmValue::Int(n) => (*n).max(0) as usize,
                _ => 0,
            };
            if let VmValue::Closure(closure) = closure {
                let mut handles = Vec::with_capacity(count);
                for i in 0..count {
                    let mut child = self.child_vm();
                    let closure = closure.clone();
                    handles.push(tokio::task::spawn_local(async move {
                        let result = child
                            .call_closure(&closure, &[VmValue::Int(i as i64)], &[])
                            .await?;
                        Ok((result, std::mem::take(&mut child.output)))
                    }));
                }
                let mut results = vec![VmValue::Nil; count];
                for (i, handle) in handles.into_iter().enumerate() {
                    let (val, task_output): (VmValue, String) = handle
                        .await
                        .map_err(|e| VmError::Runtime(format!("Parallel task error: {e}")))??;
                    self.output.push_str(&task_output);
                    results[i] = val;
                }
                self.stack.push(VmValue::List(Rc::new(results)));
            } else {
                self.stack.push(VmValue::Nil);
            }
        } else if op == Op::ParallelMap as u8 {
            let closure = self.pop()?;
            let list_val = self.pop()?;
            match (&list_val, &closure) {
                (VmValue::List(items), VmValue::Closure(closure)) => {
                    let len = items.len();
                    let mut handles = Vec::with_capacity(len);
                    for item in items.iter() {
                        let mut child = self.child_vm();
                        let closure = closure.clone();
                        let item = item.clone();
                        handles.push(tokio::task::spawn_local(async move {
                            let result = child.call_closure(&closure, &[item], &[]).await?;
                            Ok((result, std::mem::take(&mut child.output)))
                        }));
                    }
                    let mut results = Vec::with_capacity(len);
                    for handle in handles {
                        let (val, task_output): (VmValue, String) = handle
                            .await
                            .map_err(|e| VmError::Runtime(format!("Parallel map error: {e}")))??;
                        self.output.push_str(&task_output);
                        results.push(val);
                    }
                    self.stack.push(VmValue::List(Rc::new(results)));
                }
                _ => self.stack.push(VmValue::Nil),
            }
        } else if op == Op::ParallelSettle as u8 {
            let closure = self.pop()?;
            let list_val = self.pop()?;
            match (&list_val, &closure) {
                (VmValue::List(items), VmValue::Closure(closure)) => {
                    let len = items.len();
                    let mut handles = Vec::with_capacity(len);
                    for item in items.iter() {
                        let mut child = self.child_vm();
                        let closure = closure.clone();
                        let item = item.clone();
                        handles.push(tokio::task::spawn_local(async move {
                            let result = child.call_closure(&closure, &[item], &[]).await;
                            let output = std::mem::take(&mut child.output);
                            (result, output)
                        }));
                    }
                    let mut results = Vec::with_capacity(len);
                    let mut succeeded = 0i64;
                    let mut failed = 0i64;
                    for handle in handles {
                        let (result, task_output) = handle
                            .await
                            .map_err(|e| VmError::Runtime(format!("Parallel settle error: {e}")))?;
                        self.output.push_str(&task_output);
                        match result {
                            Ok(val) => {
                                succeeded += 1;
                                results.push(VmValue::EnumVariant {
                                    enum_name: "Result".into(),
                                    variant: "Ok".into(),
                                    fields: vec![val],
                                });
                            }
                            Err(e) => {
                                failed += 1;
                                results.push(VmValue::EnumVariant {
                                    enum_name: "Result".into(),
                                    variant: "Err".into(),
                                    fields: vec![VmValue::String(Rc::from(e.to_string()))],
                                });
                            }
                        }
                    }
                    let mut dict = BTreeMap::new();
                    dict.insert("results".to_string(), VmValue::List(Rc::new(results)));
                    dict.insert("succeeded".to_string(), VmValue::Int(succeeded));
                    dict.insert("failed".to_string(), VmValue::Int(failed));
                    self.stack.push(VmValue::Dict(Rc::new(dict)));
                }
                _ => self.stack.push(VmValue::Nil),
            }
        } else if op == Op::Spawn as u8 {
            let _spawn_span =
                super::ScopeSpan::new(crate::tracing::SpanKind::Spawn, "spawn".into());
            let closure = self.pop()?;
            if let VmValue::Closure(closure) = closure {
                self.task_counter += 1;
                let task_id = format!("vm_task_{}", self.task_counter);
                let mut child = self.child_vm();
                let cancel_token = Arc::new(std::sync::atomic::AtomicBool::new(false));
                child.cancel_token = Some(cancel_token.clone());
                let handle = tokio::task::spawn_local(async move {
                    let result = child.call_closure(&closure, &[], &[]).await?;
                    Ok((result, std::mem::take(&mut child.output)))
                });
                self.spawned_tasks.insert(
                    task_id.clone(),
                    VmTaskHandle {
                        handle,
                        cancel_token,
                    },
                );
                self.stack.push(VmValue::TaskHandle(task_id));
            } else {
                self.stack.push(VmValue::Nil);
            }
        } else if op == Op::Import as u8 {
            let frame = self.frames.last_mut().unwrap();
            let path_idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let import_path = Self::const_string(&frame.chunk.constants[path_idx])?;
            self.execute_import(&import_path, None).await?;
        } else if op == Op::SelectiveImport as u8 {
            let frame = self.frames.last_mut().unwrap();
            let path_idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let names_idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let import_path = Self::const_string(&frame.chunk.constants[path_idx])?;
            let names_str = Self::const_string(&frame.chunk.constants[names_idx])?;
            let names: Vec<String> = names_str.split(',').map(|s| s.to_string()).collect();
            self.execute_import(&import_path, Some(&names)).await?;
        } else if op == Op::DeadlineSetup as u8 {
            let dur_val = self.pop()?;
            let ms = match &dur_val {
                VmValue::Duration(ms) => *ms,
                VmValue::Int(n) => (*n).max(0) as u64,
                _ => 30_000,
            };
            let deadline = Instant::now() + std::time::Duration::from_millis(ms);
            self.deadlines.push((deadline, self.frames.len()));
        } else if op == Op::DeadlineEnd as u8 {
            self.deadlines.pop();
        } else if op == Op::BuildEnum as u8 {
            let frame = self.frames.last_mut().unwrap();
            let enum_idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let variant_idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let field_count = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let enum_name = Self::const_string(&frame.chunk.constants[enum_idx])?;
            let variant = Self::const_string(&frame.chunk.constants[variant_idx])?;
            let fields = self
                .stack
                .split_off(self.stack.len().saturating_sub(field_count));
            self.stack.push(VmValue::EnumVariant {
                enum_name,
                variant,
                fields,
            });
        } else if op == Op::MatchEnum as u8 {
            let frame = self.frames.last_mut().unwrap();
            let enum_idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let variant_idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let enum_name = Self::const_string(&frame.chunk.constants[enum_idx])?;
            let variant_name = Self::const_string(&frame.chunk.constants[variant_idx])?;
            let val = self.pop()?;
            let matches = match &val {
                VmValue::EnumVariant {
                    enum_name: en,
                    variant: vn,
                    ..
                } => *en == enum_name && *vn == variant_name,
                _ => false,
            };
            // Push the value back (we only peeked conceptually), then push the bool
            self.stack.push(val);
            self.stack.push(VmValue::Bool(matches));
        } else if op == Op::GetArgc as u8 {
            let argc = self.frames.last().map(|f| f.argc).unwrap_or(0);
            self.stack.push(VmValue::Int(argc as i64));
        } else if op == Op::Yield as u8 {
            let val = self.pop()?;
            if let Some(sender) = &self.yield_sender {
                // Inside a generator task: send the yielded value through the channel.
                // If the receiver has been dropped, the generator was abandoned.
                let _ = sender.send(val).await;
                // After sending, yield to the tokio executor to let the consumer
                // receive the value before we produce the next one.
                tokio::task::yield_now().await;
            }
            // After yield, push Nil as the result of the yield expression.
            self.stack.push(VmValue::Nil);
        } else {
            return Err(VmError::InvalidInstruction(op));
        }

        Ok(None)
    }

    // --- Arithmetic helpers ---

    fn add(&self, a: VmValue, b: VmValue) -> VmValue {
        match (&a, &b) {
            (VmValue::Int(x), VmValue::Int(y)) => VmValue::Int(x.wrapping_add(*y)),
            (VmValue::Float(x), VmValue::Float(y)) => VmValue::Float(x + y),
            (VmValue::Int(x), VmValue::Float(y)) => VmValue::Float(*x as f64 + y),
            (VmValue::Float(x), VmValue::Int(y)) => VmValue::Float(x + *y as f64),
            (VmValue::String(x), _) => VmValue::String(Rc::from(format!("{x}{}", b.display()))),
            (VmValue::List(x), VmValue::List(y)) => {
                let mut result = (**x).clone();
                result.extend(y.iter().cloned());
                VmValue::List(Rc::new(result))
            }
            (VmValue::Dict(x), VmValue::Dict(y)) => {
                let mut result = (**x).clone();
                result.extend(y.iter().map(|(k, v)| (k.clone(), v.clone())));
                VmValue::Dict(Rc::new(result))
            }
            _ => VmValue::String(Rc::from(format!("{}{}", a.display(), b.display()))),
        }
    }

    fn sub(&self, a: VmValue, b: VmValue) -> VmValue {
        match (&a, &b) {
            (VmValue::Int(x), VmValue::Int(y)) => VmValue::Int(x.wrapping_sub(*y)),
            (VmValue::Float(x), VmValue::Float(y)) => VmValue::Float(x - y),
            (VmValue::Int(x), VmValue::Float(y)) => VmValue::Float(*x as f64 - y),
            (VmValue::Float(x), VmValue::Int(y)) => VmValue::Float(x - *y as f64),
            _ => VmValue::Nil,
        }
    }

    fn mul(&self, a: VmValue, b: VmValue) -> VmValue {
        match (&a, &b) {
            (VmValue::Int(x), VmValue::Int(y)) => VmValue::Int(x.wrapping_mul(*y)),
            (VmValue::Float(x), VmValue::Float(y)) => VmValue::Float(x * y),
            (VmValue::Int(x), VmValue::Float(y)) => VmValue::Float(*x as f64 * y),
            (VmValue::Float(x), VmValue::Int(y)) => VmValue::Float(x * *y as f64),
            _ => VmValue::Nil,
        }
    }

    fn div(&self, a: VmValue, b: VmValue) -> Result<VmValue, VmError> {
        match (&a, &b) {
            (VmValue::Int(_), VmValue::Int(y)) if *y == 0 => Err(VmError::DivisionByZero),
            (VmValue::Int(x), VmValue::Int(y)) => Ok(VmValue::Int(x / y)),
            (VmValue::Float(_), VmValue::Float(y)) if *y == 0.0 => Err(VmError::DivisionByZero),
            (VmValue::Float(x), VmValue::Float(y)) => Ok(VmValue::Float(x / y)),
            (VmValue::Int(_), VmValue::Float(y)) if *y == 0.0 => Err(VmError::DivisionByZero),
            (VmValue::Int(x), VmValue::Float(y)) => Ok(VmValue::Float(*x as f64 / y)),
            (VmValue::Float(_), VmValue::Int(y)) if *y == 0 => Err(VmError::DivisionByZero),
            (VmValue::Float(x), VmValue::Int(y)) => Ok(VmValue::Float(x / *y as f64)),
            _ => Err(VmError::Runtime(format!(
                "Cannot divide {} by {}",
                a.type_name(),
                b.type_name()
            ))),
        }
    }

    fn modulo(&self, a: VmValue, b: VmValue) -> Result<VmValue, VmError> {
        match (&a, &b) {
            (VmValue::Int(_), VmValue::Int(y)) if *y == 0 => Err(VmError::DivisionByZero),
            (VmValue::Int(x), VmValue::Int(y)) => Ok(VmValue::Int(x % y)),
            (VmValue::Float(_), VmValue::Float(y)) if *y == 0.0 => Err(VmError::DivisionByZero),
            (VmValue::Float(x), VmValue::Float(y)) => Ok(VmValue::Float(x % y)),
            (VmValue::Int(_), VmValue::Float(y)) if *y == 0.0 => Err(VmError::DivisionByZero),
            (VmValue::Int(x), VmValue::Float(y)) => Ok(VmValue::Float(*x as f64 % y)),
            (VmValue::Float(_), VmValue::Int(y)) if *y == 0 => Err(VmError::DivisionByZero),
            (VmValue::Float(x), VmValue::Int(y)) => Ok(VmValue::Float(x % *y as f64)),
            _ => Err(VmError::Runtime(format!(
                "Cannot modulo {} by {}",
                a.type_name(),
                b.type_name()
            ))),
        }
    }
}
