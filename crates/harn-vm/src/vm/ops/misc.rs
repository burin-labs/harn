use crate::chunk::Op;
use crate::value::{VmError, VmValue};

impl super::super::Vm {
    pub(super) fn execute_check_type(&mut self) -> Result<(), VmError> {
        let (chunk, function_name, var_idx, type_idx) = {
            let frame = self.frames.last_mut().unwrap();
            let var_idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let type_idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            (
                std::sync::Arc::clone(&frame.chunk),
                frame.fn_name.clone(),
                var_idx,
                type_idx,
            )
        };
        let var_name = Self::const_str(&chunk.constants[var_idx])?;
        let expected_type = Self::const_str(&chunk.constants[type_idx])?;
        if let Some(val) = self.env.get(var_name) {
            let actual_type = val.type_name();
            let compatible = actual_type == expected_type
                || (expected_type == "float" && actual_type == "int")
                || (expected_type == "int" && actual_type == "float");
            if !compatible {
                return Err(VmError::Runtime(format!(
                    "TypeError: function '{}' parameter '{}' expected {}, got {} ({})",
                    function_name,
                    var_name,
                    expected_type,
                    actual_type,
                    val.display()
                )));
            }
        }
        Ok(())
    }

    /// Check an annotated `let` / `const` initializer against its declared
    /// type. The value stays on the stack for the binding lowering that
    /// follows, so this reads the top of the stack rather than popping it.
    pub(super) fn execute_assert_binding_type(&mut self) -> Result<(), VmError> {
        let (chunk, slot_idx) = {
            let frame = self.frames.last_mut().unwrap();
            let slot_idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            (std::sync::Arc::clone(&frame.chunk), slot_idx)
        };
        let Some(slot) = chunk.binding_types.get(slot_idx) else {
            return Err(VmError::InvalidInstruction(Op::AssertBindingType as u8));
        };
        let value = self.peek()?;
        crate::typecheck::validate_binding_type(value, slot, None)
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
