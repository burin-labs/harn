use tokio::sync::broadcast;

use super::*;

impl Dispatcher {
    pub(super) async fn dispatch_persona(
        &self,
        binding: &TriggerBinding,
        route: &DispatchUri,
        event: &TriggerEvent,
        autonomy_tier: AutonomyTier,
        wait_lease: Option<DispatchWaitLease>,
        cancel_rx: &mut broadcast::Receiver<()>,
    ) -> Result<DispatchCallResult, DispatchError> {
        let TriggerHandlerSpec::Persona {
            binding: persona_binding,
            callable,
        } = &binding.handler
        else {
            return Err(DispatchError::Local(format!(
                "trigger '{}' resolved to a persona dispatch URI but does not carry a persona binding",
                binding.id.as_str()
            )));
        };
        let admission = crate::personas::begin_persona_trigger(
            &self.event_log,
            persona_binding,
            event.provider.as_str(),
            &event.kind,
            trigger_event_persona_metadata(event),
            crate::PersonaRunCost::default(),
            crate::persona_now_ms(),
        )
        .await
        .map_err(DispatchError::Local)?;
        let receipt = match admission {
            crate::personas::PersonaRunAdmission::Terminal(receipt) => receipt,
            crate::personas::PersonaRunAdmission::Admitted(context) => {
                match self
                    .invoke_vm_callable(
                        callable,
                        &binding.binding_key(),
                        event,
                        None,
                        binding.id.as_str(),
                        &event.qualified_kind(),
                        autonomy_tier,
                        wait_lease,
                        cancel_rx,
                    )
                    .await
                {
                    Ok(value) => {
                        let result = vm_value_to_json(&value);
                        let cost = persona_run_cost_from_dispatch_result(&result);
                        crate::personas::complete_persona_run(
                            &self.event_log,
                            persona_binding,
                            context.with_cost(cost),
                            Some(result),
                            crate::persona_now_ms(),
                        )
                        .await
                        .map_err(DispatchError::Local)?
                    }
                    Err(error) => {
                        let failure = error.to_string();
                        crate::personas::fail_persona_run(
                            &self.event_log,
                            persona_binding,
                            context,
                            &failure,
                            crate::persona_now_ms(),
                        )
                        .await
                        .map_err(|lifecycle_error| {
                            DispatchError::Local(format!(
                                "{failure}; failed to record persona terminal state: {lifecycle_error}"
                            ))
                        })?;
                        return Err(error);
                    }
                }
            }
        };
        Ok(DispatchCallResult {
            output: serde_json::to_value(receipt)
                .map_err(|error| DispatchError::Serde(error.to_string()))?,
            metadata: route.dispatch_boundary_metadata(),
        })
    }
}
