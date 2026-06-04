mod catalog;
mod core;
mod normalize;
mod payloads;
/// Rust normalized-event structs generated from the canonical Harn schema
/// module (`crates/harn-stdlib/src/stdlib/stdlib_event_schemas.harn`) by
/// `harn connector-schema-codegen`. These coexist with the hand-written
/// `payloads` structs and are proven equivalent by the
/// `schemas_generated_parity` tests; switching the trigger boundary to produce
/// them is a follow-up.
#[allow(dead_code)]
mod schemas_generated;
mod util;

#[cfg(test)]
mod tests;

pub use catalog::{
    install_provider_catalog, provider_metadata, register_provider_schema,
    registered_provider_metadata, registered_provider_schema_names, reset_provider_catalog,
    reset_provider_catalog_with, ProviderCatalog, ProviderCatalogError, ProviderMetadata,
    ProviderOutboundMethod, ProviderRuntimeMetadata, ProviderSchema, ProviderSecretRequirement,
    SignatureVerificationMetadata,
};
pub use core::{
    redact_headers, HeaderRedactionPolicy, ProviderId, SignatureStatus, TenantId, TraceId,
    TriggerEvent, TriggerEventId,
};
pub use payloads::{
    A2aPushPayload, ChannelEventPayload, CronEventPayload, ExtensionProviderPayload,
    GenericWebhookPayload, GitHubCheckRunEventPayload, GitHubCheckSuiteEventPayload,
    GitHubDeploymentStatusEventPayload, GitHubEventCommon, GitHubEventPayload,
    GitHubInstallationEventPayload, GitHubInstallationRepositoriesEventPayload,
    GitHubIssueCommentEventPayload, GitHubIssuesEventPayload, GitHubMergeGroupEventPayload,
    GitHubPullRequestEventPayload, GitHubPullRequestReviewEventPayload, GitHubPushEventPayload,
    GitHubStatusEventPayload, GitHubWorkflowRunEventPayload, KnownProviderPayload,
    LinearCustomerEventPayload, LinearCustomerRequestEventPayload, LinearCycleEventPayload,
    LinearEventCommon, LinearEventPayload, LinearIssueChange, LinearIssueCommentEventPayload,
    LinearIssueEventPayload, LinearIssueLabelEventPayload, LinearProjectEventPayload,
    NotionEventPayload, NotionPolledChangeEvent, ProviderPayload, SlackAppHomeOpenedEventPayload,
    SlackAppMentionEventPayload, SlackAssistantThreadStartedEventPayload, SlackEventCommon,
    SlackEventPayload, SlackMessageEventPayload, SlackReactionAddedEventPayload,
    StreamEventPayload,
};
pub use schemas_generated::{
    GitForgePullRequestEvent, GitForgePullRequestRef, GitForgeRepositoryRef,
    GitForgeWritebackTarget,
};
