#![allow(clippy::result_large_err, clippy::cloned_ref_to_slice_refs)]

pub mod a2a;
pub mod agent_events;
pub mod agent_sessions;
pub mod bridge;
mod builtin_id;
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
pub mod personas;
pub mod process_sandbox;
pub mod record_filter;
pub mod runtime_context;
pub mod runtime_paths;
pub mod schema;
pub mod secrets;
pub(crate) mod shared_state;
pub mod skills;
pub mod stdlib;
pub mod stdlib_modules;
pub mod store;
pub(crate) mod synchronization;
pub mod tenant;
pub mod tool_annotations;
pub mod tracing;
pub mod triggers;
pub mod trust_graph;
pub mod value;
pub mod visible_text;
mod vm;
pub mod waitpoints;
pub mod workspace_path;

pub use builtin_id::BuiltinId;
pub use checkpoint::register_checkpoint_builtins;
pub use chunk::*;
pub use compiler::*;
pub use connectors::{
    active_connector_client, active_metrics_registry, clear_active_connector_clients,
    clear_active_metrics_registry, connector_export_denied_builtin_reason,
    connector_export_effect_class,
    cron::{CatchupMode, CronConnector},
    default_connector_export_policy,
    harn_module::{
        load_contract as load_harn_connector_contract, HarnConnector, HarnConnectorContract,
    },
    hmac::verify_hmac_signed,
    install_active_connector_clients, install_active_metrics_registry,
    load_pending_webhook_handshakes, postprocess_normalized_event, ActivationHandle, ClientError,
    Connector, ConnectorClient, ConnectorCtx, ConnectorError, ConnectorExportEffectClass,
    ConnectorHttpResponse, ConnectorMetricsSnapshot, ConnectorNormalizeResult, ConnectorRegistry,
    GenericWebhookConnector, GitHubConnector, HarnConnectorEffectPolicies, LinearConnector,
    MetricsRegistry, NotionConnector, PersistedNotionWebhookHandshake, PostNormalizeOutcome,
    ProviderPayloadSchema, RateLimitConfig, RateLimiterFactory, RawInbound, SlackConnector,
    StreamConnector, TriggerBinding, TriggerKind, TriggerRegistry, WebhookSignatureVariant,
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
pub use personas::{
    disable_persona, fire_schedule as fire_persona_schedule, fire_trigger as fire_persona_trigger,
    format_ms as format_persona_ms, now_ms as persona_now_ms, parse_rfc3339_ms as parse_persona_ms,
    pause_persona, persona_status, record_persona_spend, resume_persona, PersonaBudgetPolicy,
    PersonaBudgetStatus, PersonaLease, PersonaLifecycleState, PersonaRunCost, PersonaRunReceipt,
    PersonaRuntimeBinding, PersonaStatus, PersonaTriggerEnvelope, PERSONA_RUNTIME_TOPIC,
};
pub use record_filter::{normalize_record_filter_expression, CompiledRecordFilter};
pub use schema::json_to_vm_value;
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
pub use stdlib::waitpoint::{
    process_waitpoint_resume_event, service_waitpoints_once, WAITPOINT_RESUME_TOPIC,
};
pub use stdlib::workflow_messages::{
    workflow_pause_for_base, workflow_publish_query_for_base, workflow_query_for_base,
    workflow_respond_update_for_base, workflow_resume_for_base, workflow_signal_for_base,
    workflow_update_for_base, WorkflowMailboxState,
};
pub use stdlib::{
    register_agent_stdlib, register_core_stdlib, register_io_stdlib, register_vm_stdlib,
};
pub use store::register_store_builtins;
pub use tenant::{
    tenant_event_topic_prefix, tenant_secret_namespace, tenant_topic, validate_tenant_id, ApiKeyId,
    TenantApiKeyRecord, TenantBudget, TenantEventLog, TenantRecord, TenantRegistrySnapshot,
    TenantResolutionError, TenantScope, TenantSecretProvider, TenantStatus, TenantStore,
    TENANT_EVENT_TOPIC_PREFIX, TENANT_REGISTRY_DIR, TENANT_REGISTRY_FILE,
    TENANT_SECRET_NAMESPACE_PREFIX,
};
pub use triggers::{
    append_dispatch_cancel_request, begin_in_flight, binding_autonomy_budget_would_exceed,
    binding_budget_would_exceed, binding_version_as_of, classify_trigger_dlq_error,
    clear_dispatcher_state, clear_orchestrator_budget, clear_trigger_registry, drain,
    dynamic_deregister, dynamic_register, expected_predicate_cost_usd_micros, finish_in_flight,
    install_manifest_triggers, install_orchestrator_budget, micros_to_usd,
    note_autonomous_decision, note_orchestrator_budget_cost, orchestrator_budget_would_exceed,
    parse_flow_control_duration, pin_trigger_binding, provider_metadata,
    record_predicate_cost_sample, redact_headers, register_provider_schema,
    registered_provider_metadata, registered_provider_schema_names, reset_binding_budget_windows,
    reset_provider_catalog, reset_provider_catalog_with, resolve_live_or_as_of,
    resolve_live_trigger_binding, resolve_trigger_binding_as_of, run_trigger_harness_fixture,
    snapshot_dispatcher_stats, snapshot_orchestrator_budget, snapshot_trigger_bindings,
    unpin_trigger_binding, usd_to_micros, worker_claims_topic_name, worker_job_topic_name,
    worker_response_topic_name, ClaimedWorkerJob, DispatchCancelRequest, DispatchError,
    DispatchOutcome, DispatchStatus, Dispatcher, DispatcherDrainReport, DispatcherStatsSnapshot,
    HeaderRedactionPolicy, InboxIndex, NotionPolledChangeEvent, OrchestratorBudgetConfig,
    OrchestratorBudgetSnapshot, ProviderCatalog, ProviderCatalogError, ProviderId,
    ProviderMetadata, ProviderOutboundMethod, ProviderPayload, ProviderRuntimeMetadata,
    ProviderSchema, ProviderSecretRequirement, RecordedTriggerBinding, RetryPolicy,
    SignatureStatus, SignatureVerificationMetadata, StreamEventPayload, TenantId, TraceId,
    TriggerBatchConfig, TriggerBindingSnapshot, TriggerBindingSource, TriggerBindingSpec,
    TriggerBudgetExhaustionStrategy, TriggerConcurrencyConfig, TriggerDebounceConfig,
    TriggerDispatchOutcome, TriggerEvent, TriggerEventId, TriggerExpressionSpec,
    TriggerFlowControlConfig, TriggerHandlerSpec, TriggerHarnessResult, TriggerId,
    TriggerMetricsSnapshot, TriggerPredicateSpec, TriggerPriorityOrderConfig,
    TriggerRateLimitConfig, TriggerRegistryError, TriggerRetryConfig, TriggerSingletonConfig,
    TriggerState, TriggerThrottleConfig, WorkerQueue, WorkerQueueClaimHandle,
    WorkerQueueEnqueueReceipt, WorkerQueueJob, WorkerQueueJobState, WorkerQueuePriority,
    WorkerQueueResponseRecord, WorkerQueueState, WorkerQueueSummary, DEFAULT_INBOX_RETENTION_DAYS,
    TRIGGERS_LIFECYCLE_TOPIC, TRIGGER_ATTEMPTS_TOPIC, TRIGGER_CANCEL_REQUESTS_TOPIC,
    TRIGGER_DLQ_TOPIC, TRIGGER_INBOX_CLAIMS_TOPIC, TRIGGER_INBOX_ENVELOPES_TOPIC,
    TRIGGER_INBOX_LEGACY_TOPIC, TRIGGER_OPERATION_AUDIT_TOPIC, TRIGGER_OUTBOX_TOPIC,
    TRIGGER_TEST_FIXTURES, WORKER_QUEUE_CATALOG_TOPIC,
};
pub use trust_graph::{
    append_active_trust_record, append_trust_record, group_trust_records_by_trace,
    policy_for_agent, policy_for_autonomy_tier, query_trust_records, resolve_agent_autonomy_tier,
    summarize_trust_records, topic_for_agent, trust_score_for, verify_trust_chain, AutonomyTier,
    TrustAgentSummary, TrustChainReport, TrustOutcome, TrustQueryFilters, TrustRecord, TrustScore,
    TrustTraceGroup, OPENTRUSTGRAPH_SCHEMA_V0, TRUST_GRAPH_GLOBAL_TOPIC,
    TRUST_GRAPH_LEGACY_GLOBAL_TOPIC, TRUST_GRAPH_LEGACY_TOPIC_PREFIX, TRUST_GRAPH_TOPIC_PREFIX,
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

pub fn json_schema_for_type_expr(type_expr: &harn_parser::TypeExpr) -> Option<serde_json::Value> {
    let schema = compiler::Compiler::type_expr_to_schema_value(type_expr)?;
    let json_schema = schema::schema_to_json_schema_value(&schema).ok()?;
    Some(llm::vm_value_to_json(&json_schema))
}

pub fn json_schema_for_typed_params(params: &[harn_parser::TypedParam]) -> serde_json::Value {
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();

    for param in params {
        let param_schema = param
            .type_expr
            .as_ref()
            .and_then(json_schema_for_type_expr)
            .unwrap_or_else(|| serde_json::json!({}));
        if param.default_value.is_none() {
            required.push(serde_json::Value::String(param.name.clone()));
        }
        properties.insert(param.name.clone(), param_schema);
    }

    let mut schema = serde_json::Map::new();
    schema.insert(
        "type".to_string(),
        serde_json::Value::String("object".to_string()),
    );
    schema.insert(
        "properties".to_string(),
        serde_json::Value::Object(properties),
    );
    if !required.is_empty() {
        schema.insert("required".to_string(), serde_json::Value::Array(required));
    }
    serde_json::Value::Object(schema)
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
