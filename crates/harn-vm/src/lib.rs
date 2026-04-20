#![allow(clippy::result_large_err, clippy::cloned_ref_to_slice_refs)]

pub mod a2a;
pub mod agent_events;
pub mod agent_sessions;
pub mod bridge;
pub mod checkpoint;
mod chunk;
mod compiler;
pub mod connectors;
pub mod event_log;
pub mod events;
mod http;
pub mod jsonrpc;
pub mod llm;
pub mod llm_config;
pub mod mcp;
pub mod mcp_card;
pub mod mcp_registry;
pub mod mcp_server;
pub mod metadata;
pub mod observability;
pub mod orchestration;
pub mod record_filter;
pub mod runtime_paths;
pub mod schema;
pub mod secrets;
pub mod skills;
pub mod stdlib;
pub mod stdlib_modules;
pub mod store;
pub mod tool_annotations;
pub mod tracing;
pub mod triggers;
pub mod trust_graph;
pub mod value;
pub mod visible_text;
mod vm;
pub mod workspace_path;

pub use checkpoint::register_checkpoint_builtins;
pub use chunk::*;
pub use compiler::*;
pub use connectors::{
    active_connector_client, clear_active_connector_clients,
    cron::{CatchupMode, CronConnector},
    hmac::verify_hmac_signed,
    install_active_connector_clients, postprocess_normalized_event, ActivationHandle, ClientError,
    Connector, ConnectorClient, ConnectorCtx, ConnectorError, ConnectorMetricsSnapshot,
    ConnectorRegistry, GenericWebhookConnector, GitHubConnector, MetricsRegistry,
    PostNormalizeOutcome, ProviderPayloadSchema, RateLimitConfig, RateLimiterFactory, RawInbound,
    SlackConnector, TriggerBinding, TriggerKind, TriggerRegistry, WebhookSignatureVariant,
};
pub use http::{register_http_builtins, reset_http_state};
pub use llm::register_llm_builtins;
pub use llm::trigger_predicate::TriggerPredicateBudget;
pub use mcp::{
    connect_mcp_server, connect_mcp_server_from_json, connect_mcp_server_from_spec,
    register_mcp_builtins,
};
pub use mcp_card::{fetch_server_card, load_server_card_from_path, CardError};
pub use mcp_registry::{
    active_handle as mcp_active_handle, ensure_active as mcp_ensure_active,
    get_registration as mcp_get_registration, install_active as mcp_install_active,
    is_registered as mcp_is_registered, register_servers as mcp_register_servers,
    release as mcp_release, reset as mcp_reset_registry, snapshot_status as mcp_snapshot_status,
    sweep_expired as mcp_sweep_expired, RegisteredMcpServer, RegistryStatus,
};
pub use mcp_server::{
    take_mcp_serve_prompts, take_mcp_serve_registry, take_mcp_serve_resource_templates,
    take_mcp_serve_resources, tool_registry_to_mcp_tools, McpServer,
};
pub use metadata::{register_metadata_builtins, register_scan_builtins};
pub use record_filter::{normalize_record_filter_expression, CompiledRecordFilter};
pub use stdlib::hitl::{
    append_hitl_response, HitlHostResponse, HITL_APPROVALS_TOPIC, HITL_DUAL_CONTROL_TOPIC,
    HITL_ESCALATIONS_TOPIC, HITL_QUESTIONS_TOPIC,
};
pub use stdlib::host::{clear_host_call_bridge, set_host_call_bridge, HostCallBridge};
pub use stdlib::secret_scan::{
    append_secret_scan_audit, audit_secret_scan_active, scan_content as secret_scan_content,
    SecretFinding, SECRET_SCAN_AUDIT_TOPIC,
};
pub use stdlib::template::{
    lookup_prompt_consumers, lookup_prompt_span, prompt_render_indices, record_prompt_render_index,
    PromptSourceSpan, PromptSpanKind,
};
pub use stdlib::{
    register_agent_stdlib, register_core_stdlib, register_io_stdlib, register_vm_stdlib,
};
pub use store::register_store_builtins;
pub use triggers::{
    append_dispatch_cancel_request, begin_in_flight, binding_version_as_of, clear_dispatcher_state,
    clear_trigger_registry, drain, dynamic_deregister, dynamic_register, finish_in_flight,
    install_manifest_triggers, parse_flow_control_duration, pin_trigger_binding, provider_metadata,
    redact_headers, register_provider_schema, registered_provider_metadata,
    registered_provider_schema_names, reset_provider_catalog, resolve_live_or_as_of,
    resolve_live_trigger_binding, resolve_trigger_binding_as_of, run_trigger_harness_fixture,
    snapshot_dispatcher_stats, snapshot_trigger_bindings, unpin_trigger_binding,
    DispatchCancelRequest, DispatchError, DispatchOutcome, DispatchStatus, Dispatcher,
    DispatcherDrainReport, DispatcherStatsSnapshot, HeaderRedactionPolicy, InboxIndex,
    ProviderCatalog, ProviderCatalogError, ProviderId, ProviderMetadata, ProviderOutboundMethod,
    ProviderPayload, ProviderRuntimeMetadata, ProviderSchema, ProviderSecretRequirement,
    RecordedTriggerBinding, RetryPolicy, SignatureStatus, SignatureVerificationMetadata, TenantId,
    TraceId, TriggerBatchConfig, TriggerBindingSnapshot, TriggerBindingSource, TriggerBindingSpec,
    TriggerConcurrencyConfig, TriggerDebounceConfig, TriggerDispatchOutcome, TriggerEvent,
    TriggerEventId, TriggerExpressionSpec, TriggerFlowControlConfig, TriggerHandlerSpec,
    TriggerHarnessResult, TriggerId, TriggerMetricsSnapshot, TriggerPredicateSpec,
    TriggerPriorityOrderConfig, TriggerRateLimitConfig, TriggerRegistryError, TriggerRetryConfig,
    TriggerSingletonConfig, TriggerState, TriggerThrottleConfig, DEFAULT_INBOX_RETENTION_DAYS,
    TRIGGERS_LIFECYCLE_TOPIC, TRIGGER_ATTEMPTS_TOPIC, TRIGGER_CANCEL_REQUESTS_TOPIC,
    TRIGGER_DLQ_TOPIC, TRIGGER_INBOX_CLAIMS_TOPIC, TRIGGER_INBOX_ENVELOPES_TOPIC,
    TRIGGER_INBOX_LEGACY_TOPIC, TRIGGER_OPERATION_AUDIT_TOPIC, TRIGGER_OUTBOX_TOPIC,
    TRIGGER_TEST_FIXTURES,
};
pub use trust_graph::{
    append_active_trust_record, append_trust_record, query_trust_records,
    resolve_agent_autonomy_tier, summarize_trust_records, topic_for_agent, AutonomyTier,
    TrustAgentSummary, TrustOutcome, TrustQueryFilters, TrustRecord, OPENTRUSTGRAPH_SCHEMA_V0,
    TRUST_GRAPH_GLOBAL_TOPIC, TRUST_GRAPH_TOPIC_PREFIX,
};
pub use value::*;
pub use vm::*;

/// Lex, parse, type-check, and compile source to bytecode in one call.
/// Bails on the first type error. For callers that need diagnostics
/// rather than early exit, use `harn_parser::check_source` directly
/// and then call `Compiler::new().compile(&program)`.
pub fn compile_source(source: &str) -> Result<Chunk, String> {
    let program = harn_parser::check_source_strict(source).map_err(|e| e.to_string())?;
    Compiler::new().compile(&program).map_err(|e| e.to_string())
}

/// Reset all thread-local state that can leak between test runs.
pub fn reset_thread_local_state() {
    llm::reset_llm_state();
    llm_config::clear_user_overrides();
    http::reset_http_state();
    event_log::reset_active_event_log();
    stdlib::reset_stdlib_state();
    connectors::clear_active_connector_clients();
    orchestration::clear_runtime_hooks();
    triggers::clear_dispatcher_state();
    triggers::clear_trigger_registry();
    events::reset_event_sinks();
    agent_events::reset_all_sinks();
    agent_sessions::reset_session_store();
    mcp_registry::reset();
}
