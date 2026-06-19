use std::sync::Arc;

use crate::chunk::{AdaptiveBinaryOp, Chunk, Constant};
use crate::value::{VmError, VmValue};

impl super::super::Vm {
    fn constant_name_rc(chunk: &Chunk, idx: usize, fallback: &str) -> arcstr::ArcStr {
        chunk
            .constant_string_rc(idx)
            .unwrap_or_else(|| arcstr::ArcStr::from(fallback))
    }

    pub(super) fn execute_constant(&mut self) -> Result<(), VmError> {
        let frame = self.frames.last_mut().unwrap();
        let idx = frame.chunk.read_u16(frame.ip) as usize;
        frame.ip += 2;
        let val = match &frame.chunk.constants[idx] {
            Constant::Int(n) => VmValue::Int(*n),
            Constant::Float(n) => VmValue::Float(*n),
            Constant::String(_) => {
                // Route through the chunk's lazy `HarnStr` cache so repeated
                // pushes of the same string constant share a single
                // allocation — the push is then a refcount bump, not a fresh
                // materialization, per execution.
                let rc = frame
                    .chunk
                    .constant_string_rc(idx)
                    .expect("Constant::String idx must resolve to a HarnStr");
                VmValue::String(rc)
            }
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
        let (chunk, idx) = {
            let frame = self.frames.last_mut().unwrap();
            let idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            (Arc::clone(&frame.chunk), idx)
        };
        let name = Self::const_str(&chunk.constants[idx])?;
        if let Some(val) = self.active_local_slot_value(name) {
            self.stack.push(val);
        } else if let Some(val) = self.env.get(name) {
            self.stack.push(val);
        } else if let Some(val) = self
            .frames
            .last()
            .and_then(|f| f.module_state.as_ref())
            .and_then(|ms| ms.lock().get(name))
        {
            // Module-level var from the closure's originating module.
            self.stack.push(val);
        } else if let Some(val) = self.globals.get(name) {
            self.stack.push(val.clone());
        } else if let Some(id) = self.registered_builtin_id(name) {
            // Allow bare builtin references so they can be passed as callbacks.
            self.stack.push(VmValue::builtin_ref_id(
                id,
                Self::constant_name_rc(&chunk, idx, name),
            ));
        } else if self.builtins.contains_key(name) || self.async_builtins.contains_key(name) {
            // Collided IDs cannot use the direct index, but remain valid callbacks.
            self.stack.push(VmValue::BuiltinRef(Self::constant_name_rc(
                &chunk, idx, name,
            )));
        } else {
            let mut all_vars = self.visible_variables();
            for (k, v) in self.globals.iter() {
                all_vars.entry(k.clone()).or_insert_with(|| v.clone());
            }
            // Include builtin names so typos on builtin refs get suggestions.
            let mut candidates: Vec<String> = all_vars.keys().cloned().collect();
            candidates.extend(self.builtins.keys().cloned());
            candidates.extend(self.async_builtins.keys().cloned());
            if let Some(suggestion) =
                crate::value::closest_match(name, candidates.iter().map(|s| s.as_str()))
            {
                return Err(VmError::Runtime(format!(
                    "Undefined variable: {name} (did you mean `{suggestion}`?)"
                )));
            }
            return Err(VmError::UndefinedVariable(name.to_string()));
        }
        Ok(())
    }

    pub(super) fn execute_def_let(&mut self) -> Result<(), VmError> {
        let (chunk, idx) = {
            let frame = self.frames.last_mut().unwrap();
            let idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            (Arc::clone(&frame.chunk), idx)
        };
        let name = Self::const_str(&chunk.constants[idx])?;
        let val = self.pop()?;
        self.sync_current_frame_locals_to_env();
        self.env.define(name, val, false)
    }

    pub(super) fn execute_def_var(&mut self) -> Result<(), VmError> {
        let (chunk, idx) = {
            let frame = self.frames.last_mut().unwrap();
            let idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            (Arc::clone(&frame.chunk), idx)
        };
        let name = Self::const_str(&chunk.constants[idx])?;
        let val = self.pop()?;
        self.sync_current_frame_locals_to_env();
        self.env.define(name, val, true)
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
        let Some(slot) = frame.local_slots.get(slot_idx) else {
            return Err(VmError::Runtime(format!(
                "Invalid local slot index: {slot_idx}"
            )));
        };
        if !slot.initialized {
            // Only materialize the binding name on the cold error path.
            // Cloning the slot's `String` name on every successful read was a
            // per-instruction heap allocation — the dominant cost in the
            // `local_variable_lookup` hot loop. SOTA interpreters never
            // allocate on the fast read path.
            let name = frame
                .chunk
                .local_slots
                .get(slot_idx)
                .map(|info| info.name.clone())
                .unwrap_or_else(|| format!("<slot {slot_idx}>"));
            return Err(VmError::UndefinedVariable(name));
        }
        let value = slot.value.clone();
        self.stack.push(value);
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
        // Tear down any nested value being overwritten iteratively so a deep
        // local cannot overflow the stack on drop (scalars are a no-op).
        crate::value::recursion::dismantle(std::mem::replace(&mut slot.value, val));
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
        // Tear down any nested value being overwritten iteratively so a deep
        // local cannot overflow the stack on drop (scalars are a no-op).
        crate::value::recursion::dismantle(std::mem::replace(&mut slot.value, val));
        slot.synced = false;
        Ok(())
    }

    /// In-place `+`-concat into a local slot: the runtime for `x = x + e` and
    /// `x += e` where `x` resolves to a local slot.
    ///
    /// Pops `e`, reads slot `x`, and stores `x + e`. When `x` and `e` are
    /// matching collection kinds (`list + list` / `dict + dict` — adds that
    /// always succeed), `x`'s value is *taken* out of the slot before the add,
    /// so the slot no longer aliases the buffer and `Arc::try_unwrap` extends
    /// it in place — turning the `out = out + [item]` / `out += [item]`
    /// accumulator loop from O(n^2) into amortized O(n). The earlier
    /// compile-time emission only fired this fast path for statically-typed
    /// collections; gating on the *runtime* shapes here also covers
    /// dynamically-typed (`any`) accumulators.
    ///
    /// Every other case clones the slot value and routes the add through the
    /// shared adaptive-binary core, so (a) numeric accumulators keep their
    /// `Int`/`Float` inline-cache specialization and (b) a throwing add (e.g.
    /// `x += y` with incompatible operands) leaves the binding at its previous
    /// value rather than a placeholder — the take only happens for adds proven
    /// to succeed.
    pub(super) fn execute_concat_assign_local(&mut self) -> Result<(), VmError> {
        let (slot_idx, cache_id, slot_count, cache_slot) = {
            let frame = self.frames.last_mut().unwrap();
            let op_offset = frame.ip.saturating_sub(1);
            let slot_idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let cache_id = frame.chunk.cache_id();
            let slot_count = frame.chunk.inline_cache_slot_count();
            let cache_slot = frame.chunk.inline_cache_slot(op_offset);
            (slot_idx, cache_id, slot_count, cache_slot)
        };
        let rhs = self.pop()?;
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
        // Take the value in place only when the add is guaranteed to succeed
        // (matching collection kinds). Releasing the slot's reference lets the
        // concat's `Arc::try_unwrap` extend the buffer in place when it is not
        // otherwise aliased; for everything else the slot is cloned so a
        // throwing add leaves the binding intact.
        let take_in_place = matches!(
            (&slot.value, &rhs),
            (VmValue::List(_), VmValue::List(_)) | (VmValue::Dict(_), VmValue::Dict(_))
        );
        let lhs = if take_in_place {
            std::mem::replace(&mut slot.value, VmValue::Nil)
        } else {
            slot.value.clone()
        };
        let result = self.adaptive_binary_compute(
            AdaptiveBinaryOp::Add,
            lhs,
            rhs,
            cache_id,
            slot_count,
            cache_slot,
        )?;
        let frame = self.frames.last_mut().unwrap();
        let slot = frame
            .local_slots
            .get_mut(slot_idx)
            .expect("slot index validated above");
        crate::value::recursion::dismantle(std::mem::replace(&mut slot.value, result));
        slot.synced = false;
        Ok(())
    }

    pub(super) fn execute_set_var(&mut self) -> Result<(), VmError> {
        let (chunk, idx) = {
            let frame = self.frames.last_mut().unwrap();
            let idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            (Arc::clone(&frame.chunk), idx)
        };
        let name = Self::const_str(&chunk.constants[idx])?;
        let val = self.pop()?;
        // Local scope wins; otherwise route to the closure's shared
        // module_state. Fall through to env.assign only when neither
        // has it, so UndefinedVariable / ImmutableAssignment surface.
        if self.assign_active_local_slot(name, val.clone(), false)? {
            // Slot locals are the active binding for compiler-resolved names.
        } else if self.env.get(name).is_some() {
            self.env.assign(name, val)?;
        } else if let Some(ms) = self
            .frames
            .last()
            .and_then(|f| f.module_state.as_ref())
            .cloned()
        {
            let mut module_state = ms.lock();
            if module_state.get(name).is_some() {
                module_state.assign(name, val)?;
            } else {
                // Neither has it: let env.assign produce the diagnostic.
                self.env.assign(name, val)?;
            }
        } else {
            self.env.assign(name, val)?;
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
