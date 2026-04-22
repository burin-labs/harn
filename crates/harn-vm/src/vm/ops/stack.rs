use std::rc::Rc;

use crate::chunk::Constant;
use crate::value::{VmError, VmValue};

impl super::super::Vm {
    pub(super) fn execute_constant(&mut self) -> Result<(), VmError> {
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
        Ok(())
    }

    pub(super) fn execute_nil(&mut self) {
        self.stack.push(VmValue::Nil);
    }

    pub(super) fn execute_true(&mut self) {
        self.stack.push(VmValue::Bool(true));
    }

    pub(super) fn execute_false(&mut self) {
        self.stack.push(VmValue::Bool(false));
    }

    pub(super) fn execute_get_var(&mut self) -> Result<(), VmError> {
        let frame = self.frames.last_mut().unwrap();
        let idx = frame.chunk.read_u16(frame.ip) as usize;
        frame.ip += 2;
        let name = match &frame.chunk.constants[idx] {
            Constant::String(s) => s.clone(),
            _ => return Err(VmError::TypeError("expected string constant".into())),
        };
        if let Some(val) = self.active_local_slot_value(&name) {
            self.stack.push(val);
        } else if let Some(val) = self.env.get(&name) {
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
            let mut all_vars = self.visible_variables();
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
        Ok(())
    }

    pub(super) fn execute_def_let(&mut self) -> Result<(), VmError> {
        let frame = self.frames.last_mut().unwrap();
        let idx = frame.chunk.read_u16(frame.ip) as usize;
        frame.ip += 2;
        let name = Self::const_string(&frame.chunk.constants[idx])?;
        let val = self.pop()?;
        self.sync_current_frame_locals_to_env();
        self.env.define(&name, val, false)
    }

    pub(super) fn execute_def_var(&mut self) -> Result<(), VmError> {
        let frame = self.frames.last_mut().unwrap();
        let idx = frame.chunk.read_u16(frame.ip) as usize;
        frame.ip += 2;
        let name = Self::const_string(&frame.chunk.constants[idx])?;
        let val = self.pop()?;
        self.sync_current_frame_locals_to_env();
        self.env.define(&name, val, true)
    }

    pub(super) fn execute_push_scope(&mut self) {
        self.env.push_scope();
        if let Some(frame) = self.frames.last_mut() {
            frame.local_scope_depth += 1;
        }
    }

    pub(super) fn execute_pop_scope(&mut self) {
        self.release_sync_guards_for_current_scope();
        self.env.pop_scope();
        if let Some(frame) = self.frames.last_mut() {
            frame.local_scope_depth = frame.local_scope_depth.saturating_sub(1);
        }
    }

    pub(super) fn execute_get_local_slot(&mut self) -> Result<(), VmError> {
        let frame = self.frames.last_mut().unwrap();
        let slot_idx = frame.chunk.read_u16(frame.ip) as usize;
        frame.ip += 2;
        let name = frame
            .chunk
            .local_slots
            .get(slot_idx)
            .map(|info| info.name.clone())
            .unwrap_or_else(|| format!("<slot {slot_idx}>"));
        let Some(slot) = frame.local_slots.get(slot_idx) else {
            return Err(VmError::Runtime(format!(
                "Invalid local slot index: {slot_idx}"
            )));
        };
        if !slot.initialized {
            return Err(VmError::UndefinedVariable(name));
        }
        self.stack.push(slot.value.clone());
        Ok(())
    }

    pub(super) fn execute_def_local_slot(&mut self) -> Result<(), VmError> {
        let slot_idx = {
            let frame = self.frames.last_mut().unwrap();
            let slot_idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            slot_idx
        };
        let val = self.pop()?;
        let frame = self.frames.last_mut().unwrap();
        let Some(slot) = frame.local_slots.get_mut(slot_idx) else {
            return Err(VmError::Runtime(format!(
                "Invalid local slot index: {slot_idx}"
            )));
        };
        slot.value = val;
        slot.initialized = true;
        slot.synced = false;
        Ok(())
    }

    pub(super) fn execute_set_local_slot(&mut self) -> Result<(), VmError> {
        let slot_idx = {
            let frame = self.frames.last_mut().unwrap();
            let slot_idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            slot_idx
        };
        let val = self.pop()?;
        let frame = self.frames.last_mut().unwrap();
        let Some(info) = frame.chunk.local_slots.get(slot_idx) else {
            return Err(VmError::Runtime(format!(
                "Invalid local slot index: {slot_idx}"
            )));
        };
        if !info.mutable {
            return Err(VmError::ImmutableAssignment(info.name.clone()));
        }
        let Some(slot) = frame.local_slots.get_mut(slot_idx) else {
            return Err(VmError::Runtime(format!(
                "Invalid local slot index: {slot_idx}"
            )));
        };
        if !slot.initialized {
            return Err(VmError::UndefinedVariable(info.name.clone()));
        }
        slot.value = val;
        slot.synced = false;
        Ok(())
    }

    pub(super) fn execute_set_var(&mut self) -> Result<(), VmError> {
        let frame = self.frames.last_mut().unwrap();
        let idx = frame.chunk.read_u16(frame.ip) as usize;
        frame.ip += 2;
        let name = Self::const_string(&frame.chunk.constants[idx])?;
        let val = self.pop()?;
        // Local scope wins; otherwise route to the closure's shared
        // module_state. Fall through to env.assign only when neither
        // has it, so UndefinedVariable / ImmutableAssignment surface.
        if self.assign_active_local_slot(&name, val.clone(), false)? {
            // Slot locals are the active binding for compiler-resolved names.
        } else if self.env.get(&name).is_some() {
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
                // Neither has it: let env.assign produce the diagnostic.
                self.env.assign(&name, val)?;
            }
        } else {
            self.env.assign(&name, val)?;
        }
        Ok(())
    }

    pub(super) fn execute_pop(&mut self) -> Result<(), VmError> {
        self.pop().map(drop)
    }

    pub(super) fn execute_dup(&mut self) -> Result<(), VmError> {
        let val = self.peek()?.clone();
        self.stack.push(val);
        Ok(())
    }

    pub(super) fn execute_swap(&mut self) {
        let len = self.stack.len();
        if len >= 2 {
            self.stack.swap(len - 1, len - 2);
        }
    }

    pub(super) fn execute_get_argc(&mut self) {
        let argc = self.frames.last().map(|f| f.argc).unwrap_or(0);
        self.stack.push(VmValue::Int(argc as i64));
    }
}
