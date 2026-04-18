use crate::chunk::{Constant, Op};
use crate::value::{VmError, VmValue};

impl super::super::Vm {
    pub(super) async fn try_execute_misc_op(&mut self, op: u8) -> Result<bool, VmError> {
        if op == Op::CheckType as u8 {
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
        } else if op == Op::Yield as u8 {
            let val = self.pop()?;
            if let Some(sender) = &self.yield_sender {
                // Dropped receiver = generator was abandoned; ignore send error.
                let _ = sender.send(val).await;
                // Let the consumer pull this value before we produce the next.
                tokio::task::yield_now().await;
            }
            self.stack.push(VmValue::Nil);
        } else {
            return Ok(false);
        }
        Ok(true)
    }
}
