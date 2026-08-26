use harn_vm::triggers::event::{GenericWebhookPayload, KnownProviderPayload};
use harn_vm::{ProviderId, ProviderPayload, SignatureStatus, TriggerEvent};

/// Provider id stamped on synthetic job events. Reuses the generic webhook
/// payload so request JSON stays in `provider_payload.raw`, like other
/// webhook-shaped trigger handlers.
pub(super) const JOB_PROVIDER: &str = "webhook";

pub(super) fn job_event(
    job_name: &str,
    request: serde_json::Value,
    tenant_id: Option<harn_vm::TenantId>,
) -> TriggerEvent {
    TriggerEvent::new(
        ProviderId::from(JOB_PROVIDER),
        "job",
        None,
        format!("job:{job_name}:{}", uuid::Uuid::new_v4()),
        tenant_id,
        std::collections::BTreeMap::new(),
        ProviderPayload::Known(KnownProviderPayload::Webhook(GenericWebhookPayload {
            source: Some(format!("job:{job_name}")),
            content_type: Some("application/json".to_string()),
            raw: request,
        })),
        SignatureStatus::Verified,
    )
}
