use crate::value::{VmError, VmValue};

/// Exception handler for try/catch.
pub(crate) struct ExceptionHandler {
    pub(crate) catch_ip: usize,
    pub(crate) stack_depth: usize,
    pub(crate) frame_depth: usize,
    pub(crate) env_scope_depth: usize,
    /// When present, this catch only handles errors whose enum_name matches.
    pub(crate) error_type: Option<crate::value::HarnStr>,
}

/// The most recent error delivered to a handler. `finally`/`defer` cleanup
/// rethrows the value it caught, and a `catch` may `throw` its binding again;
/// either way the original error and stack trace, not a stringified copy
/// located at the rethrow, continue to propagate.
#[derive(Clone)]
pub(crate) struct CaughtError {
    pub(crate) value: VmValue,
    pub(crate) error: VmError,
    pub(crate) stack_trace: Vec<(String, usize, usize, Option<String>)>,
}

impl super::super::Vm {
    pub(super) fn execute_throw(&mut self) -> Result<(), VmError> {
        let val = self.pop()?;
        // Rethrowing the exact value a handler received (cleanup does this
        // after `finally`/`defer` runs) re-raises the original error, so its
        // kind and origin survive the cleanup instead of being restringified.
        if let Some(caught) = self
            .last_caught_error
            .as_ref()
            .filter(|caught| same_allocation(&caught.value, &val))
        {
            self.error_stack_trace = caught.stack_trace.clone();
            return Err(caught.error.clone());
        }
        Err(VmError::Thrown(val))
    }

    pub(super) fn execute_try_catch_setup(&mut self) {
        let frame = self.frames.last_mut().unwrap();
        let catch_offset = frame.chunk.read_u16(frame.ip) as usize;
        frame.ip += 2;
        let type_idx = frame.chunk.read_u16(frame.ip) as usize;
        frame.ip += 2;
        let error_type = frame
            .chunk
            .constant_string_rc(type_idx)
            .filter(|name| !name.is_empty());
        self.exception_handlers.push(ExceptionHandler {
            catch_ip: catch_offset,
            stack_depth: self.stack.len(),
            frame_depth: self.frames.len(),
            env_scope_depth: self.env.scope_depth(),
            error_type,
        });
    }

    pub(super) fn execute_pop_handler(&mut self) {
        self.exception_handlers.pop();
    }

    pub(super) fn execute_try_unwrap(&mut self) -> Result<(), VmError> {
        let val = self.pop()?;
        match &val {
            VmValue::EnumVariant(enum_variant) if enum_variant.has_enum_name("Result") => {
                if enum_variant.is_variant("Result", "Ok") {
                    self.stack
                        .push(enum_variant.fields.first().cloned().unwrap_or(VmValue::Nil));
                    Ok(())
                } else {
                    Err(VmError::Return(val))
                }
            }
            other => Err(VmError::TypeError(format!(
                "? operator requires a Result value, got {}",
                other.type_name()
            ))),
        }
    }

    pub(super) fn execute_try_wrap_ok(&mut self) -> Result<(), VmError> {
        let val = self.pop()?;
        match &val {
            VmValue::EnumVariant(enum_variant) if enum_variant.has_enum_name("Result") => {
                self.stack.push(val);
            }
            _ => {
                self.stack
                    .push(VmValue::enum_variant("Result", "Ok", vec![val]));
            }
        }
        Ok(())
    }
}

/// Reference identity for heap values. Scalars have no identity, so a scalar
/// rethrow is indistinguishable from a fresh throw and reports its own site.
fn same_allocation(a: &VmValue, b: &VmValue) -> bool {
    match (a, b) {
        (VmValue::String(x), VmValue::String(y)) => arcstr::ArcStr::ptr_eq(x, y),
        (VmValue::Bytes(x), VmValue::Bytes(y)) => std::sync::Arc::ptr_eq(x, y),
        (VmValue::List(x), VmValue::List(y)) => std::sync::Arc::ptr_eq(x, y),
        (VmValue::Dict(x), VmValue::Dict(y)) => std::sync::Arc::ptr_eq(x, y),
        (VmValue::EnumVariant(x), VmValue::EnumVariant(y)) => std::sync::Arc::ptr_eq(x, y),
        (VmValue::StructInstance(x), VmValue::StructInstance(y)) => std::sync::Arc::ptr_eq(x, y),
        _ => false,
    }
}
