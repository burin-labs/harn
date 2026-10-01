use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use serde::Serialize;

use super::Vm;

/// Counted interpreter work, independent of wall time and module compilation caches.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct VmWork {
    /// Decoded bytecode instructions dispatched, including instructions that fail.
    /// Native work performed inside a builtin is not measured by this counter.
    pub vm_steps: u64,
}

/// Opt-in instruction counter shared by a VM and all subsequently created children.
#[derive(Clone, Debug, Default)]
pub struct VmWorkRecorder(Arc<AtomicU64>);

impl VmWorkRecorder {
    /// Snapshot cumulative work. Read after children stop for a terminal count.
    pub fn snapshot(&self) -> VmWork {
        VmWork {
            vm_steps: self.0.load(Ordering::Relaxed),
        }
    }

    pub(super) fn record_step(&self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

impl Vm {
    /// Enable counted work for this execution tree. Repeated calls retain the count.
    /// Fresh VMs and baseline instances start with recording disabled.
    pub fn enable_work_recording(&mut self) -> VmWorkRecorder {
        self.work_recorder
            .get_or_insert_with(VmWorkRecorder::default)
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Chunk, Constant, Op, VmError, VmValue};

    fn four_steps() -> Chunk {
        let mut chunk = Chunk::new();
        let left = chunk.add_constant(Constant::Int(41));
        let right = chunk.add_constant(Constant::Int(1));
        chunk.emit_u16(Op::Constant, left, 1);
        chunk.emit_u16(Op::Constant, right, 1);
        chunk.emit(Op::Add, 1);
        chunk.emit(Op::Return, 1);
        chunk
    }

    #[tokio::test]
    async fn counts_dispatched_instructions_not_operand_bytes_across_children() {
        let mut vm = Vm::new();
        assert!(vm.work_recorder.is_none());
        let recorder = vm.enable_work_recording();
        assert_eq!(recorder.snapshot().vm_steps, 0);
        let chunk = four_steps();
        assert!(chunk.code.len() > 4);
        assert!(matches!(
            vm.execute(&chunk).await.unwrap(),
            VmValue::Int(42)
        ));
        assert_eq!(recorder.snapshot().vm_steps, 4);

        let mut child = vm.child_vm();
        assert!(matches!(
            child.execute(&chunk).await.unwrap(),
            VmValue::Int(42)
        ));
        assert_eq!(recorder.snapshot().vm_steps, 8);
        assert_eq!(vm.enable_work_recording().snapshot(), recorder.snapshot());
        assert!(vm.baseline().instantiate().work_recorder.is_none());
        assert!(Vm::new().work_recorder.is_none());
    }

    #[tokio::test]
    async fn debugger_counts_the_same_steps_and_failure_counts_its_dispatch() {
        let mut vm = Vm::new();
        let recorder = vm.enable_work_recording();
        vm.start(&four_steps()).unwrap();
        for _ in 0..4 {
            vm.step_execute().await.unwrap();
        }
        assert_eq!(recorder.snapshot().vm_steps, 4);

        let mut failing = Chunk::new();
        failing.emit(Op::Add, 1);
        let mut vm = Vm::new();
        let recorder = vm.enable_work_recording();
        assert!(matches!(
            vm.execute(&failing).await,
            Err(VmError::StackUnderflow)
        ));
        assert_eq!(recorder.snapshot().vm_steps, 1);

        let mut vm = Vm::new();
        let recorder = vm.enable_work_recording();
        vm.execute(&Chunk::new()).await.unwrap();
        assert_eq!(recorder.snapshot().vm_steps, 0);
    }
}
