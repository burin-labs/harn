use crate::value::VmError;

impl super::super::Vm {
    pub(super) fn execute_jump(&mut self) {
        let frame = self.frames.last_mut().unwrap();
        let target = frame.chunk.read_u16(frame.ip) as usize;
        frame.ip = target;
    }

    pub(super) fn execute_jump_if_false(&mut self) -> Result<(), VmError> {
        let frame = self.frames.last_mut().unwrap();
        let target = frame.chunk.read_u16(frame.ip) as usize;
        frame.ip += 2;
        let val = self.peek()?;
        if !val.is_truthy() {
            let frame = self.frames.last_mut().unwrap();
            frame.ip = target;
        }
        Ok(())
    }

    pub(super) fn execute_jump_if_true(&mut self) -> Result<(), VmError> {
        let frame = self.frames.last_mut().unwrap();
        let target = frame.chunk.read_u16(frame.ip) as usize;
        frame.ip += 2;
        let val = self.peek()?;
        if val.is_truthy() {
            let frame = self.frames.last_mut().unwrap();
            frame.ip = target;
        }
        Ok(())
    }
}
