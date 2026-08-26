use harn_vm::event_log::Topic;
use harn_vm::{TenantId, TriggerEvent};

use crate::DispatchError;

pub(super) fn topic(
    name: &str,
    tenant_id: Option<&TenantId>,
) -> Result<Topic, harn_vm::event_log::LogError> {
    let topic = Topic::new(name)?;
    match tenant_id {
        Some(tenant_id) => harn_vm::tenant_topic(tenant_id, &topic),
        None => Ok(topic),
    }
}

pub(super) fn enforce_event(
    event: &mut TriggerEvent,
    tenant_id: Option<&TenantId>,
) -> Result<(), DispatchError> {
    let Some(tenant_id) = tenant_id else {
        return Ok(());
    };
    match event.tenant_id.as_ref() {
        Some(event_tenant) if event_tenant != tenant_id => Err(DispatchError::Validation(format!(
            "worker tenant '{}' cannot dispatch event for tenant '{}'",
            tenant_id.0, event_tenant.0
        ))),
        Some(_) => Ok(()),
        None => {
            event.tenant_id = Some(tenant_id.clone());
            Ok(())
        }
    }
}
