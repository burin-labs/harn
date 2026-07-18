use crate::chunk::Op;

use super::{Compiler, LocalBinding, LocalBindingKind, LocalStorage};

enum CallableDefinitionTarget {
    Slot(u16),
    Environment,
    ShadowedByValue,
}

impl Compiler {
    /// Whether this mutable source binding must be boxed into a shared cell
    /// because a nested callable captures this exact declaration.
    #[inline]
    fn is_boxed_capture(&self, binding: &harn_parser::lexical::BindingId, mutable: bool) -> bool {
        mutable && self.captured_bindings.contains(binding)
    }

    fn define_local_slot(&mut self, name: &str, mutable: bool) -> Option<u16> {
        if self.module_level || harn_parser::is_discard_name(name) {
            return None;
        }
        let current = self.local_scopes.last_mut()?;
        if let Some(existing) = current.get_mut(name) {
            if existing.kind == LocalBindingKind::Callable {
                existing.kind = LocalBindingKind::Value;
                existing.mutable = mutable;
                return match existing.storage {
                    LocalStorage::Slot(slot) => {
                        if let Some(info) = self.chunk.local_slots.get_mut(slot as usize) {
                            info.mutable = mutable;
                        }
                        Some(slot)
                    }
                    LocalStorage::Environment => None,
                };
            }
            if existing.mutable || mutable {
                if mutable {
                    existing.mutable = true;
                    if let LocalStorage::Slot(slot) = existing.storage {
                        if let Some(info) = self.chunk.local_slots.get_mut(slot as usize) {
                            info.mutable = true;
                        }
                    }
                }
                return match existing.storage {
                    LocalStorage::Slot(slot) => Some(slot),
                    LocalStorage::Environment => None,
                };
            }
            return None;
        }
        let slot = self
            .chunk
            .add_local_slot(name.to_string(), mutable, self.scope_depth);
        current.insert(
            name.to_string(),
            LocalBinding {
                storage: LocalStorage::Slot(slot),
                kind: LocalBindingKind::Value,
                mutable,
            },
        );
        Some(slot)
    }

    fn resolve_local_binding(&self, name: &str) -> Option<LocalBinding> {
        if self.module_level {
            return None;
        }
        for scope in self.local_scopes.iter().rev() {
            if let Some(binding) = scope.get(name) {
                return Some(*binding);
            }
        }
        None
    }

    pub(super) fn has_local_binding(&self, name: &str) -> bool {
        self.resolve_local_binding(name).is_some()
    }

    pub(super) fn resolve_local_slot(&self, name: &str) -> Option<u16> {
        match self.resolve_local_binding(name)?.storage {
            LocalStorage::Slot(slot) => Some(slot),
            LocalStorage::Environment => None,
        }
    }

    pub(super) fn emit_get_binding(&mut self, name: &str) {
        if let Some(slot) = self.resolve_local_slot(name) {
            self.chunk.emit_u16(Op::GetLocalSlot, slot, self.line);
        } else {
            let idx = self.string_constant(name);
            self.chunk.emit_u16(Op::GetVar, idx, self.line);
        }
    }

    pub(super) fn emit_define_binding(&mut self, name: &str, mutable: bool) {
        if let Some(slot) = self.define_local_slot(name, mutable) {
            self.chunk.emit_u16(Op::DefLocalSlot, slot, self.line);
        } else {
            let idx = self.string_constant(name);
            let op = if mutable { Op::DefVar } else { Op::DefLet };
            self.chunk.emit_u16(op, idx, self.line);
        }
    }

    fn define_callable_target(&mut self, name: &str) -> CallableDefinitionTarget {
        if self.module_level || harn_parser::is_discard_name(name) {
            return CallableDefinitionTarget::Environment;
        }
        let Some(current) = self.local_scopes.last_mut() else {
            return CallableDefinitionTarget::Environment;
        };
        if let Some(existing) = current.get(name) {
            return match (existing.kind, existing.storage) {
                (LocalBindingKind::Value, _) => CallableDefinitionTarget::ShadowedByValue,
                (LocalBindingKind::Callable, LocalStorage::Slot(slot)) => {
                    CallableDefinitionTarget::Slot(slot)
                }
                (LocalBindingKind::Callable, LocalStorage::Environment) => {
                    CallableDefinitionTarget::Environment
                }
            };
        }
        let slot = self
            .chunk
            .add_local_slot(name.to_string(), false, self.scope_depth);
        current.insert(
            name.to_string(),
            LocalBinding {
                storage: LocalStorage::Slot(slot),
                kind: LocalBindingKind::Callable,
                mutable: false,
            },
        );
        CallableDefinitionTarget::Slot(slot)
    }

    /// Bind a named callable unless an earlier value declaration owns the
    /// same name in this lexical scope. The typechecker resolves locals before
    /// its hoisted function table; codegen must preserve that ordering instead
    /// of replacing the value when the function declaration executes.
    pub(super) fn emit_callable_binding(&mut self, name: &str) {
        match self.define_callable_target(name) {
            CallableDefinitionTarget::Slot(slot) => {
                self.chunk.emit_u16(Op::DefLocalSlot, slot, self.line);
            }
            CallableDefinitionTarget::Environment => {
                let idx = self.string_constant(name);
                self.chunk.emit_u16(Op::DefLet, idx, self.line);
            }
            CallableDefinitionTarget::ShadowedByValue => {
                self.chunk.emit(Op::Pop, self.line);
            }
        }
    }

    /// Define a binding parsed from source. Synthetic compiler temporaries use
    /// [`Self::emit_define_binding`] and are deliberately absent from lexical
    /// capture analysis.
    pub(super) fn emit_source_binding(
        &mut self,
        name: &str,
        mutable: bool,
        binding: harn_parser::lexical::BindingId,
    ) {
        if self.is_boxed_capture(&binding, mutable) {
            // Box a closure-captured mutable local into a shared cell. Runs
            // regardless of `module_level`: a captured top-level `let` needs the
            // same shared cell so a top-level closure observes its writes. In a
            // function-like body, retain an environment-backed lexical marker
            // so a later hoisted callable cannot steal earlier references.
            if !self.module_level && !harn_parser::is_discard_name(name) {
                if let Some(scope) = self.local_scopes.last_mut() {
                    scope.insert(
                        name.to_string(),
                        LocalBinding {
                            storage: LocalStorage::Environment,
                            kind: LocalBindingKind::Value,
                            mutable,
                        },
                    );
                }
            }
            let idx = self.string_constant(name);
            self.chunk.emit_u16(Op::DefCell, idx, self.line);
        } else {
            self.emit_define_binding(name, mutable);
        }
    }

    pub(super) fn emit_init_or_define_binding(&mut self, name: &str, mutable: bool) {
        if let Some(slot) = self.resolve_local_slot(name) {
            self.chunk.emit_u16(Op::DefLocalSlot, slot, self.line);
        } else {
            self.emit_define_binding(name, mutable);
        }
    }

    pub(super) fn emit_set_binding(&mut self, name: &str) {
        if let Some(slot) = self.resolve_local_slot(name) {
            self.chunk.emit_u16(Op::SetLocalSlot, slot, self.line);
        } else {
            let idx = self.string_constant(name);
            self.chunk.emit_u16(Op::SetVar, idx, self.line);
        }
    }
}
