//! RAII guards for workflow-scoped mutation-session and approval-policy state.

use crate::orchestration::install_current_mutation_session;

pub(super) struct MutationSessionResetGuard;

impl Drop for MutationSessionResetGuard {
    fn drop(&mut self) {
        install_current_mutation_session(None);
    }
}

pub(super) struct WorkflowApprovalPolicyGuard(pub(super) bool);

impl Drop for WorkflowApprovalPolicyGuard {
    fn drop(&mut self) {
        if self.0 {
            crate::orchestration::pop_approval_policy();
        }
    }
}
