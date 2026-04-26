use crate::chunk::Constant;
use crate::value::{VmError, VmValue};

use super::super::ExceptionHandler;

impl super::super::Vm {
    pub(super) fn execute_throw(&mut self) -> Result<(), VmError> {
        let val = self.pop()?;
        Err(VmError::Thrown(val))
    }

    pub(super) fn execute_try_catch_setup(&mut self) {
        let frame = self.frames.last_mut().unwrap();
        let catch_offset = frame.chunk.read_u16(frame.ip) as usize;
        frame.ip += 2;
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
            VmValue::EnumVariant {
                enum_name,
                variant,
                fields,
            } if enum_name.as_ref() == "Result" => {
                if variant.as_ref() == "Ok" {
                    self.stack
                        .push(fields.first().cloned().unwrap_or(VmValue::Nil));
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
            VmValue::EnumVariant { enum_name, .. } if enum_name.as_ref() == "Result" => {
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
