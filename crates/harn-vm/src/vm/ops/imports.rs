use crate::value::VmError;

impl super::super::Vm {
    pub(super) async fn execute_import_op(&mut self) -> Result<(), VmError> {
        let frame = self.frames.last_mut().unwrap();
        let path_idx = frame.chunk.read_u16(frame.ip) as usize;
        frame.ip += 2;
        let import_path = Self::const_string(&frame.chunk.constants[path_idx])?;
        self.execute_import(&import_path, None).await
    }

    pub(super) async fn execute_selective_import(&mut self) -> Result<(), VmError> {
        let frame = self.frames.last_mut().unwrap();
        let path_idx = frame.chunk.read_u16(frame.ip) as usize;
        frame.ip += 2;
        let names_idx = frame.chunk.read_u16(frame.ip) as usize;
        frame.ip += 2;
        let import_path = Self::const_string(&frame.chunk.constants[path_idx])?;
        let names_str = Self::const_string(&frame.chunk.constants[names_idx])?;
        let names: Vec<String> = names_str.split(',').map(|s| s.to_string()).collect();
        self.execute_import(&import_path, Some(&names)).await
    }
}
