#![recursion_limit = "256"]
#![allow(clippy::result_large_err, clippy::cloned_ref_to_slice_refs)]

/// Re-export of the unified clock substrate so downstream crates (CLI,
/// orchestrator, harn-cloud) can depend on a single canonical `Clock`
/// trait without each adding `harn-clock` as a direct dependency.
pub use harn_clock as clock;

pub mod a2a;
pub mod agent_events;
pub mod agent_sessions;
pub mod atomic_io;
pub mod autonomy;
pub(crate) mod aws_sigv4;
pub mod bridge;
mod builtin_id;
pub mod bytecode_cache;
pub mod channel_guardrails;
pub mod channels;
pub mod checkpoint;
mod chunk;
mod compiler;
pub mod composition;
pub mod config;
pub mod connectors;
pub mod corrections;
pub mod egress;
pub mod event_log;
pub mod events;
pub mod flow;
pub mod harness;
pub(crate) mod harness_crypto;
pub mod harness_net;
pub mod harness_system;
pub mod harness_tenant;
mod http;
pub mod jsonrpc;
pub mod llm;
pub mod llm_config;
pub mod mcp;
pub mod mcp_auth;
pub mod mcp_card;
pub mod mcp_elicit;
pub mod mcp_file_upload;
pub mod mcp_host;
pub mod mcp_progress;
pub mod mcp_protocol;
pub mod mcp_registry;
pub mod mcp_sampling;
pub mod mcp_server;
pub mod metadata;
pub mod module_artifact;
pub mod observability;
pub mod orchestration;
pub mod personas;
pub mod process_sandbox;
pub mod profile;
pub mod provenance;
pub mod provider_catalog;
pub mod receipts;
pub mod record_filter;
pub mod redact;
pub mod run_events;
pub mod runtime_context;
pub(crate) mod runtime_guards;
pub mod runtime_limits;
pub mod runtime_paths;
pub mod schema;
pub(crate) mod secret_patterns;
pub mod secrets;
pub mod session_bundle;
pub mod sessions;
pub(crate) mod shared_state;
pub mod shells;
pub mod skills;
pub mod stdlib;
pub mod stdlib_modules;
pub mod step_runtime;
pub mod store;
pub(crate) mod synchronization;
pub mod tenant;
pub(crate) mod term;
pub mod testbench;
pub mod tool_annotations;
pub mod tool_call_cancellations;
pub mod tool_surface;
pub mod tracing;
pub mod triggers;
pub mod trust_graph;
pub(crate) mod url_encoding;

/// Crate-wide deterministic clock mock used by stdlib time builtins, the
/// trigger dispatcher, the cron scheduler, and Rust-side tests. Re-exports
/// the long-lived implementation under `triggers::test_util::clock` so all
/// callers go through one source of truth.
pub mod clock_mock {
    pub use crate::triggers::test_util::clock::{
        active_mock_clock, advance, clear_overrides, install_override, instant_now, is_mocked,
        now_ms, now_utc, sleep, ClockInstant, ClockOverrideGuard, MockClock,
    };

    /// Runtime audit for capabilities that observe real wall-clock or
    /// monotonic time while a testbench mock is installed. See the module
    /// docs for the full design.
    pub mod leak_audit {
        #[cfg(test)]
        pub use crate::triggers::test_util::clock_leak::TEST_LOCK;
        pub use crate::triggers::test_util::clock_leak::{
            drain, instant_now, reset, snapshot, wall_now, ClockLeak,
        };
    }
}

pub mod typecheck;
pub mod value;
pub mod visible_text;
mod vm;
pub mod waitpoints;
pub mod workspace_anchor;
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
    hmac::{verify_hmac_signed, SIGNATURE_VERIFY_AUDIT_TOPIC},
    install_active_connector_clients, install_active_metrics_registry,
    postprocess_normalized_event, ActivationHandle, ClientError, Connector, ConnectorClient,
    ConnectorCtx, ConnectorError, ConnectorExportEffectClass, ConnectorHttpResponse,
    ConnectorMetricsSnapshot, ConnectorNormalizeResult, ConnectorRegistry, GenericWebhookConnector,
    HarnConnectorEffectPolicies, MetricsRegistry, PostNormalizeOutcome, ProviderPayloadSchema,
    RateLimitConfig, RateLimiterFactory, RawInbound, StreamConnector, TriggerBinding, TriggerKind,
    TriggerRegistry, WebhookSignatureVariant,
};
pub use corrections::{
    append_correction_record, apply_corrections_to_policy, correction_query_filters_from_json,
    correction_record_from_json, policy_with_corrections, query_correction_records,
    CorrectionQueryFilters, CorrectionRecord, CorrectionScope, CORRECTIONS_TOPIC,
    CORRECTION_EVENT_KIND, CORRECTION_SCHEMA_V0,
};
pub use harness::{
    DenyEvent, Harness, HarnessCall, HarnessClock, HarnessCrypto, HarnessEnv, HarnessFs,
    HarnessKind, HarnessLlm, HarnessNet, HarnessProcess, HarnessRandom, HarnessStdio,
    HarnessSystem, HarnessTenant, HarnessTerm, MockAwareClock, MockHarnessBuilder, VmHarness,
};
pub use harness_net::{
    bypass_enabled as net_policy_bypass_enabled, NetMatcher, NetPolicy, NetPolicyAudit,
    NetPolicyDecision, NetPolicyDefault, NetPolicyRule, OnViolation, HARN_NET_POLICY_BYPASS_ENV,
    NET_POLICY_AUDIT_TOPIC,
};
pub use harness_tenant::{
    current_tenant_id, enter_tenant, TenantScopeGuard, MISSING_TENANT_MESSAGE,
};
pub use http::{register_http_builtins, reset_http_state};
pub use llm::register_llm_builtins;
pub use llm::trigger_predicate::TriggerPredicateBudget;
pub use llm::{
    current_agent_session_id, install_llm_cost_budget, install_llm_token_budget,
    register_session_end_hook, LlmBudgetGuard, LlmTokenBudgetGuard,
};
pub use mcp::{connect_mcp_server_from_json, connect_mcp_server_from_spec, register_mcp_builtins};
pub use mcp_card::{fetch_server_card, load_server_card_from_path, CardError};
pub use mcp_host::{
    cache_stats as mcp_host_cache_stats, set_allowlist as set_mcp_host_allowlist,
    AllowlistDecision as McpHostAllowlistDecision, AllowlistGuard as McpHostAllowlistGuard,
    BreakerState as McpHostBreakerState, CacheStats as McpHostCacheStats, McpHostStatus,
    SpawnOptions as McpHostSpawnOptions, SupervisionPolicy as McpHostSupervisionPolicy,
};
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
pub use metadata::register_metadata_builtins;
pub use orchestration::{
    benchmark_adapted_replay_pair, benchmark_replay_trace, build_replay_benchmark_report,
    OpenCodeJsonlAdapter, ReplayBenchmarkCloudIngest, ReplayBenchmarkError,
    ReplayBenchmarkFixtureReceipt, ReplayBenchmarkFixtureReport, ReplayBenchmarkMetrics,
    ReplayBenchmarkReport, ReplayBenchmarkSuiteIdentity, ReplayBenchmarkSummary,
    ReplayCategoryMetric, ReplayDebuggingProxyMetrics, ReplayRuntimeCostMetrics,
    ReplayTraceAdapter, OPENCODE_JSONL_ADAPTER_ID, OPENCODE_JSONL_ADAPTER_SCHEMA_VERSION,
    REPLAY_BENCHMARK_CLOUD_INGEST_KIND, REPLAY_BENCHMARK_REPORT_SCHEMA_VERSION,
};
pub use orchestration::{
    canonicalize_run, first_divergence, run_replay_oracle_trace, ReplayAllowlistRule,
    ReplayDivergence, ReplayExpectation, ReplayOracleError, ReplayOracleReport, ReplayOracleTrace,
    ReplayTraceRun, ReplayTraceRunCounts, REPLAY_TRACE_SCHEMA_VERSION,
};
pub use orchestration::{
    install_handoff_routes, snapshot_handoff_routes, HandoffRouteConfig,
    HandoffRouteDecisionRecord, HandoffRouteTargetConfig,
};
pub use personas::{
    disable_persona, fire_schedule as fire_persona_schedule, fire_trigger as fire_persona_trigger,
    format_ms as format_persona_ms, now_ms as persona_now_ms, parse_rfc3339_ms as parse_persona_ms,
    pause_persona, persona_status, record_persona_spend, register_persona_supervision_sink,
    register_persona_value_sink, report_repair_worker_status, restore_persona_checkpoint,
    resume_persona, PersonaAssignmentStatus, PersonaBudgetPolicy, PersonaBudgetStatus,
    PersonaCheckpointAction, PersonaCheckpointRestoreOutcome, PersonaCheckpointRestoreRequest,
    PersonaCheckpointResume, PersonaCheckpointUpdate, PersonaHandoffInboxItem, PersonaLease,
    PersonaLifecycleState, PersonaQueuePositionUpdate, PersonaQueuedWork, PersonaReceiptUpdate,
    PersonaRepairWorkerLifecycle, PersonaRepairWorkerStatusUpdate, PersonaRunCost,
    PersonaRunReceipt, PersonaRuntimeBinding, PersonaStatus, PersonaSupervisionEvent,
    PersonaSupervisionSink, PersonaSupervisionSinkRegistration, PersonaTriggerEnvelope,
    PersonaValueEvent, PersonaValueEventKind, PersonaValueReceipt, PersonaValueSink,
    PersonaValueSinkRegistration, StageDecl, StageExit, PERSONA_RUNTIME_TOPIC,
};
pub use provenance::{
    build_signed_receipt, load_or_generate_agent_signing_key, verify_receipt, ProvenanceReceipt,
    ReceiptBuildOptions, ReceiptVerificationReport,
};
pub use receipts::{
    Receipt, ReceiptSink, ReceiptStatus, ReceiptValidationError, RedactingReceiptSink,
    RedactionClass, RECEIPT_SCHEMA_ID, RECEIPT_SCHEMA_JSON, RECEIPT_SCHEMA_VERSION,
};
pub use record_filter::{normalize_record_filter_expression, CompiledRecordFilter};
pub use runtime_limits::{
    RuntimeLimitDescription, RuntimeLimitEntry, RuntimeLimits, RuntimeLimitsReport,
    RUNTIME_LIMIT_DESCRIPTIONS,
};
pub use schema::json_to_vm_value;
pub use sessions::{
    CreateSession, ExpireSession, Session, SessionAttributes, SessionError, SessionStore,
    TouchSession, SESSIONS_TOPIC,
};
pub use stdlib::hitl::{
    append_hitl_response, ApprovalRequest, HitlHostResponse, HITL_APPROVALS_TOPIC,
    HITL_DUAL_CONTROL_TOPIC, HITL_ESCALATIONS_TOPIC, HITL_QUESTIONS_TOPIC,
};
pub use stdlib::host::{clear_host_call_bridge, set_host_call_bridge, HostCallBridge};
pub use stdlib::http_response::{
    parse_envelope as parse_http_envelope, HttpEnvelope, HttpHeaderValue, WsUpgradeSpec,
    HTTP_RESPONSE_TAG_KEY, HTTP_RESPONSE_TAG_VERSION,
};
pub use stdlib::io::{set_stdout_passthrough, take_stderr_buffer};
pub use stdlib::long_running::cancel_handle as cancel_long_running_handle;
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
    register_vm_stdlib_with_deferred_llm,
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
    install_manifest_triggers, install_orchestrator_budget, install_provider_catalog,
    micros_to_usd, note_autonomous_decision, note_orchestrator_budget_cost,
    orchestrator_budget_would_exceed, parse_flow_control_duration, pause, pin_trigger_binding,
    provider_metadata, record_predicate_cost_sample, redact_headers, register_provider_schema,
    registered_provider_metadata, registered_provider_schema_names, reset_binding_budget_windows,
    reset_provider_catalog, reset_provider_catalog_with, resolve_live_or_as_of,
    resolve_live_trigger_binding, resolve_trigger_binding_as_of, resume,
    run_trigger_harness_fixture, scheduler_in_flight_by_key, scheduler_ready_stats_by_key,
    snapshot_dispatcher_stats, snapshot_orchestrator_budget, snapshot_trigger_bindings,
    unpin_trigger_binding, usd_to_micros, worker_claims_topic_name, worker_job_topic_name,
    worker_response_topic_name, ClaimedWorkerJob, DispatchCancelRequest, DispatchError,
    DispatchOutcome, DispatchStatus, Dispatcher, DispatcherDrainReport, DispatcherStatsSnapshot,
    FairnessKey, HeaderRedactionPolicy, InboxIndex, NotionPolledChangeEvent,
    OrchestratorBudgetConfig, OrchestratorBudgetSnapshot, ProviderCatalog, ProviderCatalogError,
    ProviderId, ProviderMetadata, ProviderOutboundMethod, ProviderPayload, ProviderRuntimeMetadata,
    ProviderSchema, ProviderSecretRequirement, ReadyKeyStats, RecordedTriggerBinding, RetryPolicy,
    SchedulableJob, SchedulerKeyStat, SchedulerPolicy, SchedulerSnapshot, SchedulerState,
    SchedulerStrategy, SignatureStatus, SignatureVerificationMetadata, StreamEventPayload,
    TenantId, TraceId, TriggerBatchConfig, TriggerBindingSnapshot, TriggerBindingSource,
    TriggerBindingSpec, TriggerBudgetExhaustionStrategy, TriggerConcurrencyConfig,
    TriggerDebounceConfig, TriggerDispatchOutcome, TriggerEvent, TriggerEventId,
    TriggerExpressionSpec, TriggerFlowControlConfig, TriggerHandlerSpec, TriggerHarnessResult,
    TriggerId, TriggerMetricsSnapshot, TriggerPredicateSpec, TriggerPriorityOrderConfig,
    TriggerRateLimitConfig, TriggerRegistryError, TriggerRetryConfig, TriggerSingletonConfig,
    TriggerState, TriggerThrottleConfig, WorkerQueue, WorkerQueueClaimHandle,
    WorkerQueueEnqueueReceipt, WorkerQueueInspectSnapshot, WorkerQueueJob, WorkerQueueJobState,
    WorkerQueuePriority, WorkerQueueResponseRecord, WorkerQueueState, WorkerQueueSummary,
    DEFAULT_INBOX_RETENTION_DAYS, DEFAULT_STARVATION_AGE_MS, TRIGGERS_LIFECYCLE_TOPIC,
    TRIGGER_ATTEMPTS_TOPIC, TRIGGER_CANCEL_REQUESTS_TOPIC, TRIGGER_DLQ_TOPIC,
    TRIGGER_INBOX_CLAIMS_TOPIC, TRIGGER_INBOX_ENVELOPES_TOPIC, TRIGGER_INBOX_LEGACY_TOPIC,
    TRIGGER_OPERATION_AUDIT_TOPIC, TRIGGER_OUTBOX_TOPIC, TRIGGER_TEST_FIXTURES,
    WORKER_QUEUE_CATALOG_TOPIC,
};
pub use trust_graph::{
    append_active_trust_record, append_trust_record, export_trust_chain,
    group_trust_records_by_trace, policy_for_agent, policy_for_autonomy_tier,
    query_trust_graph_records, query_trust_records, resolve_agent_autonomy_tier,
    summarize_trust_records, topic_for_agent, trust_score_for, verify_trust_chain, AutonomyTier,
    TrustAgentSummary, TrustChainExport, TrustChainExportMetadata, TrustChainExportProducer,
    TrustChainReport, TrustGraphRecord, TrustOutcome, TrustQueryFilters, TrustRecord,
    TrustRecordActionKind, TrustScore, TrustTraceGroup, METADATA_KEY_EFFECTS_GRANT,
    METADATA_KEY_EFFECTS_USED, METADATA_KEY_PARENT_RECORD_ID, OPENTRUSTGRAPH_ACCEPTED_SCHEMAS,
    OPENTRUSTGRAPH_CHAIN_SCHEMA_V0, OPENTRUSTGRAPH_SCHEMA_V0, OPENTRUSTGRAPH_SCHEMA_V0_1,
    TRUST_ACTION_RELEASE, TRUST_GRAPH_GLOBAL_TOPIC, TRUST_GRAPH_LEGACY_GLOBAL_TOPIC,
    TRUST_GRAPH_LEGACY_TOPIC_PREFIX, TRUST_GRAPH_RECORDS_TOPIC, TRUST_GRAPH_TOPIC_PREFIX,
};
pub use value::*;
pub use vm::*;

#[cfg(feature = "vm-bench-internals")]
#[doc(hidden)]
pub mod bench_internals;

/// Lex, parse, type-check, and compile source to bytecode in one call.
/// Bails on the first type error. For callers that need diagnostics
/// rather than early exit, use `harn_parser::check_source` directly
/// and then call `Compiler::new().compile(&program)`.
pub fn compile_source(source: &str) -> Result<Chunk, String> {
    let program = harn_parser::check_source_strict(source).map_err(|e| e.to_string())?;
    Compiler::new().compile(&program).map_err(|e| e.to_string())
}

/// Same as [`compile_source`] but compiles a specific named pipeline as
/// the program entry point instead of the default-pipeline-or-first
/// selection rule. Returns a runtime error when no pipeline with
/// `pipeline_name` exists in the source.
pub fn compile_source_named(source: &str, pipeline_name: &str) -> Result<Chunk, String> {
    let program = harn_parser::check_source_strict(source).map_err(|e| e.to_string())?;
    let has_pipeline = program.iter().any(|sn| {
        let (_, inner) = harn_parser::peel_attributes(sn);
        matches!(&inner.node, harn_parser::Node::Pipeline { name, .. } if name == pipeline_name)
    });
    if !has_pipeline {
        return Err(format!("no pipeline named `{pipeline_name}` in source"));
    }
    Compiler::new()
        .compile_named(&program, pipeline_name)
        .map_err(|e| e.to_string())
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
    channels::reset_channel_state();
    event_log::reset_active_event_log();
    egress::clear_explicit_egress_policy_requirement_for_host();
    stdlib::reset_stdlib_state();
    connectors::clear_active_connector_clients();
    orchestration::clear_runtime_hooks();
    orchestration::clear_execution_policy_stacks();
    orchestration::clear_command_policies();
    orchestration::clear_pipeline_on_finish();
    orchestration::reset_lifecycle_receipt_registry();
    redact::clear_policy_stack();
    triggers::clear_dispatcher_state();
    triggers::clear_trigger_registry();
    events::reset_event_sinks();
    tracing::set_tracing_enabled(false);
    tracing::reset_tracing();
    agent_events::reset_all_sinks();
    agent_sessions::reset_session_store();
    mcp_registry::reset();
    mcp_host::reset_for_tests();
    clock_mock::leak_audit::reset();
}
