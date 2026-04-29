use crate::chunk::Constant;
use crate::value::{VmError, VmValue};

impl super::super::Vm {
    pub(super) fn execute_check_type(&mut self) -> Result<(), VmError> {
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
        Ok(())
    }

    pub(super) async fn execute_yield(&mut self) -> Result<(), VmError> {
        let val = self.pop()?;
        if let Some(sender) = &self.yield_sender {
            // Dropped receiver = generator was abandoned; ignore send error.
            let _ = sender.send(Ok(val)).await;
            // Let the consumer pull this value before we produce the next.
            tokio::task::yield_now().await;
        }
        self.stack.push(VmValue::Nil);
        Ok(())
    }
}
