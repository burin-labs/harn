use std::rc::Rc;

use crate::chunk::{Constant, Op};
use crate::value::{VmError, VmValue};

impl super::super::Vm {
    pub(super) fn try_execute_stack_op(&mut self, op: u8) -> Result<bool, VmError> {
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
            } else if let Some(val) = self
                .frames
                .last()
                .and_then(|f| f.module_state.as_ref())
                .and_then(|ms| ms.borrow().get(&name))
            {
                // Module-level var from the closure's originating module.
                self.stack.push(val);
            } else if let Some(val) = self.globals.get(&name) {
                self.stack.push(val.clone());
            } else if let Some(id) = self.registered_builtin_id(&name) {
                // Allow bare builtin references so they can be passed as callbacks.
                self.stack.push(VmValue::BuiltinRefId {
                    id,
                    name: Rc::from(name.as_str()),
                });
            } else if self.builtins.contains_key(&name) || self.async_builtins.contains_key(&name) {
                // Collided IDs cannot use the direct index, but remain valid callbacks.
                self.stack
                    .push(VmValue::BuiltinRef(Rc::from(name.as_str())));
            } else {
                let mut all_vars = self.env.all_variables();
                for (k, v) in &self.globals {
                    all_vars.entry(k.clone()).or_insert_with(|| v.clone());
                }
                // Include builtin names so typos on builtin refs get suggestions.
                let mut candidates: Vec<String> = all_vars.keys().cloned().collect();
                candidates.extend(self.builtins.keys().cloned());
                candidates.extend(self.async_builtins.keys().cloned());
                if let Some(suggestion) =
                    crate::value::closest_match(&name, candidates.iter().map(|s| s.as_str()))
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
        } else if op == Op::PushScope as u8 {
            self.env.push_scope();
        } else if op == Op::PopScope as u8 {
            self.env.pop_scope();
        } else if op == Op::SetVar as u8 {
            let frame = self.frames.last_mut().unwrap();
            let idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let name = Self::const_string(&frame.chunk.constants[idx])?;
            let val = self.pop()?;
            // Local scope wins; otherwise route to the closure's shared
            // module_state. Fall through to env.assign only when neither
            // has it, so UndefinedVariable / ImmutableAssignment surface.
            if self.env.get(&name).is_some() {
                self.env.assign(&name, val)?;
            } else if let Some(ms) = self
                .frames
                .last()
                .and_then(|f| f.module_state.as_ref())
                .cloned()
            {
                if ms.borrow().get(&name).is_some() {
                    ms.borrow_mut().assign(&name, val)?;
                } else {
                    // Neither has it — let env.assign produce the diagnostic.
                    self.env.assign(&name, val)?;
                }
            } else {
                self.env.assign(&name, val)?;
            }
        } else if op == Op::Pop as u8 {
            self.pop()?;
        } else if op == Op::Dup as u8 {
            let val = self.peek()?.clone();
            self.stack.push(val);
        } else if op == Op::Swap as u8 {
            let len = self.stack.len();
            if len >= 2 {
                self.stack.swap(len - 1, len - 2);
            }
        } else if op == Op::GetArgc as u8 {
            let argc = self.frames.last().map(|f| f.argc).unwrap_or(0);
            self.stack.push(VmValue::Int(argc as i64));
        } else {
            return Ok(false);
        }
        Ok(true)
    }
}
