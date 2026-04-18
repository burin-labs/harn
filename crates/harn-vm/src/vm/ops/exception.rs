use crate::chunk::{Constant, Op};
use crate::value::{VmError, VmValue};

use super::super::ExceptionHandler;

impl super::super::Vm {
    pub(super) fn try_execute_exception_op(&mut self, op: u8) -> Result<bool, VmError> {
        if op == Op::Throw as u8 {
            let val = self.pop()?;
            return Err(VmError::Thrown(val));
        } else if op == Op::TryCatchSetup as u8 {
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
        } else if op == Op::PopHandler as u8 {
            self.exception_handlers.pop();
        } else if op == Op::TryUnwrap as u8 {
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
        } else {
            return Ok(false);
        }
        Ok(true)
    }
}
