//! Native rollback ownership around a host-bounded interpreter entry.

use crate::{
    value::{VmError, VmValue},
    vm::{TopLevelEntry, Vm},
};
use std::time::Instant;

pub(super) async fn execute(
    vm: &mut Vm,
    entry: TopLevelEntry,
    deadline: Instant,
) -> Result<VmValue, VmError> {
    crate::orchestration::scope_ambient_transaction(async {
        let pipeline_checkpoint = crate::orchestration::checkpoint_pipeline_lifecycle();
        let deadline_guard = vm.install_execution_deadline(deadline);
        // The tracing wrapper must drop before the interpreter's span guards,
        // then restore the deadline and pipeline checkpoints in that order.
        let result = crate::tracing::checkpoint_future(vm.execute_top_level(entry)).await;
        deadline_guard.complete();
        pipeline_checkpoint.complete();
        result
    })
    .await
}
