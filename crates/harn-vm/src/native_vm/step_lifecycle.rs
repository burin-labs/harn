//! Native step budget policy, independent of interpreter frame unwinding.

use crate::{
    step_runtime,
    value::{VmError, VmValue},
    vm::step_context::{StepBoundaryDecision, StepErrorBoundary},
};

pub(super) fn error_boundary(error: VmError, frame_depth: usize) -> StepBoundaryDecision {
    if !step_runtime::is_step_budget_exhausted(&error) {
        return StepBoundaryDecision::Throw(error);
    }
    let Some(step_depth) = step_runtime::active_step_frame_depth() else {
        return StepBoundaryDecision::Throw(error);
    };
    // Deeper frames must unwind before the step's own boundary takes effect.
    if step_depth != frame_depth {
        return StepBoundaryDecision::Throw(error);
    }
    let boundary = step_runtime::with_active_step(|step| step.definition.boundary())
        .unwrap_or(StepErrorBoundary::Fail);
    match boundary {
        StepErrorBoundary::Continue => StepBoundaryDecision::Continue {
            message: error_message(&error),
        },
        StepErrorBoundary::Escalate => {
            let identity = step_runtime::with_active_step(|step| {
                (
                    step.definition.name.clone(),
                    step.definition.function.clone(),
                )
            });
            step_runtime::pop_and_record(step_depth, "escalated", Some(error_message(&error)));
            let (step_name, function) = identity.unzip();
            StepBoundaryDecision::Throw(step_runtime::mark_escalated(
                error,
                step_name.as_deref(),
                function.as_deref(),
            ))
        }
        StepErrorBoundary::Fail => {
            step_runtime::pop_and_record(step_depth, "failed", Some(error_message(&error)));
            StepBoundaryDecision::Throw(error)
        }
    }
}

fn error_message(error: &VmError) -> String {
    match error {
        VmError::Thrown(VmValue::Dict(dict)) => dict
            .get("message")
            .map(|v| v.display())
            .unwrap_or_else(|| error.to_string()),
        _ => error.to_string(),
    }
}
