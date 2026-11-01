//! Native pipeline settlement and finish-hook policy.

use crate::{
    value::{VmError, VmValue},
    vm::Vm,
};

pub(super) trait NativeVmLifecycle {
    fn run_pipeline_finish_lifecycle(
        &mut self,
        value: VmValue,
    ) -> impl std::future::Future<Output = Result<VmValue, VmError>> + Send;
    fn fire_finish_lifecycle_event(
        &mut self,
        event: crate::orchestration::HookEvent,
        payload: &serde_json::Value,
    ) -> impl std::future::Future<Output = Result<(), VmError>> + Send;
}

impl NativeVmLifecycle for Vm {
    /// Finish hooks run on this VM so their final output reaches the host.
    async fn run_pipeline_finish_lifecycle(&mut self, value: VmValue) -> Result<VmValue, VmError> {
        use crate::orchestration::{
            take_pipeline_on_finish, unsettled_state_snapshot_async, HookEvent,
        };
        let _tape_phase =
            crate::testbench::tape::enter_phase(crate::testbench::tape::TapePhase::RuntimeFinalize);
        let on_finish = take_pipeline_on_finish();
        let unsettled = unsettled_state_snapshot_async().await;
        let pre_payload = serde_json::json!({
            "event": HookEvent::PreFinish.as_str(),
            "return_value": crate::value::json::vm_value_to_json(&value),
            "unsettled": unsettled.to_json(),
            "has_on_finish": on_finish.is_some(),
        });
        self.fire_finish_lifecycle_event(HookEvent::PreFinish, &pre_payload)
            .await?;
        if !unsettled.is_empty() {
            let payload = serde_json::json!({
                "event": HookEvent::OnUnsettledDetected.as_str(),
                "unsettled": unsettled.to_json(),
            });
            self.fire_finish_lifecycle_event(HookEvent::OnUnsettledDetected, &payload)
                .await?;
        }
        let final_value = if let Some(closure) = on_finish {
            let harness_value = self.root_harness_value().ok_or_else(|| {
                VmError::Runtime(
                    "pipeline finish callback requires Harness, but no root Harness is installed"
                        .to_string(),
                )
            })?;
            self.call_closure_pub(&closure, &[harness_value, value])
                .await?
        } else {
            value
        };
        let post_payload = serde_json::json!({
            "event": HookEvent::PostFinish.as_str(),
            "return_value": crate::value::json::vm_value_to_json(&final_value),
            "unsettled": unsettled.to_json(),
        });
        self.fire_finish_lifecycle_event(HookEvent::PostFinish, &post_payload)
            .await?;
        Ok(final_value)
    }

    /// PreFinish rejects Block; PostFinish is advisory; unsettled work can block.
    async fn fire_finish_lifecycle_event(
        &mut self,
        event: crate::orchestration::HookEvent,
        payload: &serde_json::Value,
    ) -> Result<(), VmError> {
        use crate::orchestration::{HookControl, HookEvent};
        let invocations = crate::orchestration::matching_vm_lifecycle_hooks(event, payload);
        if invocations.is_empty() {
            return Ok(());
        }
        let harness = self.root_harness_value().ok_or_else(|| {
            VmError::Runtime(
                "pipeline lifecycle hook requires Harness, but no root Harness is installed"
                    .to_string(),
            )
        })?;
        let mut current_payload = payload.clone();
        for invocation in invocations {
            let arg = crate::value::json::json_to_vm_value(&current_payload);
            let closure = invocation.resolve(self).await?;
            let raw = invocation
                .scope(self.call_closure_pub(&closure, &[harness.clone(), arg]))
                .await?;
            let (action, effects) =
                crate::orchestration::collect_hook_effects_and_action(event, raw, VmValue::Nil)?;
            crate::orchestration::inject_hook_effects_into_current_session(effects)?;
            let control = crate::orchestration::parse_hook_control_for_finish(event, &action)?;
            match control {
                HookControl::Allow => {}
                HookControl::Block { reason } => {
                    if matches!(event, HookEvent::PreFinish) {
                        return Err(VmError::Runtime(format!(
                            "PreFinish hook returned block, which is not a valid control: {reason}. \
                             To delay pipeline finish until unsettled work clears, use \
                             OnFinish.block_until_settled (std/lifecycle) or return Modify/Allow \
                             from PreFinish."
                        )));
                    }
                    if matches!(event, HookEvent::PostFinish) {
                        continue;
                    }
                    return Err(VmError::Runtime(format!(
                        "{} hook blocked pipeline finish: {reason}",
                        event.as_str()
                    )));
                }
                HookControl::Modify { payload: modified } => current_payload = modified,
                HookControl::Decision { .. } => {}
            }
        }
        Ok(())
    }
}
