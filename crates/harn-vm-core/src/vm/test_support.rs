use super::{TaskScope, Vm};
use crate::{VmError, VmValue};

impl Vm {
    #[doc(hidden)]
    pub async fn join_task_scope_for_test(&mut self, task_ids: Vec<String>) -> Result<(), VmError> {
        self.task_scopes.push(TaskScope {
            task_ids,
            frame_depth: 0,
            env_scope_depth: 0,
        });
        self.execute_task_scope_exit().await
    }

    #[doc(hidden)]
    pub async fn call_special_for_test(
        &mut self,
        name: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        assert!(self.try_call_special_name(name, args).await?);
        self.stack.pop().ok_or(VmError::StackUnderflow)
    }
}
