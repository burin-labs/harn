mod catalog;
mod core;
mod normalize;
mod payloads;
/// Native tooling projections generated from the canonical Harn connector
/// schema module. Runtime ingress preserves package-owned extension payloads.
#[allow(dead_code)]
mod schemas_generated;
mod util;

#[cfg(test)]
mod tests;

pub use catalog::{
    provider_metadata, register_provider_schemas, registered_provider_metadata,
    registered_provider_schema_names, reset_provider_catalog, ProviderCatalog,
    ProviderCatalogError, ProviderMetadata, ProviderOutboundMethod, ProviderRuntimeMetadata,
    ProviderSchema, ProviderSecretRequirement, SignatureVerificationMetadata,
};
pub use core::{
    redact_headers, HeaderRedactionPolicy, ProviderId, SignatureStatus, TenantId, TraceId,
    TriggerEvent, TriggerEventId,
};
pub use payloads::{
    A2aPushPayload, ChannelEventPayload, CronEventPayload, ExtensionProviderPayload,
    GenericWebhookPayload, KnownProviderPayload, ProviderPayload, StreamEventPayload,
};
pub use schemas_generated::{
    GitForgePullRequestEvent, GitForgePullRequestRef, GitForgeRepositoryRef,
    GitForgeWritebackTarget,
};
