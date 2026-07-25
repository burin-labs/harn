#![recursion_limit = "256"]
#![allow(clippy::result_large_err, clippy::cloned_ref_to_slice_refs)]
//! # harn-vm
//!
//! The Harn compiler, virtual machine, standard library, provider/LLM layer,
//! orchestration runtime, and host bridge.
//!
//! ## Stability
//!
//! This crate is consumed both by the in-tree surfaces (`harn-cli`,
//! `harn-serve`, the LSP and DAP) and by external embedders. The intended
//! embedding entry points are `Vm`, `Harness`, `compile_source`, and the
//! `llm`, `orchestration`, `agent_events`, `agent_sessions`, `config`, and
//! `security` modules. Other public items exist primarily for in-workspace use
//! and may change between minor releases; anything marked `#[doc(hidden)]` is
//! an implementation detail with no stability guarantee. The crate follows the
//! workspace version and is pre-1.0, so the public surface may still evolve.

/// Re-export of the unified clock substrate so downstream crates (CLI,
/// orchestrator, and cloud runtimes) can depend on a single canonical `Clock`
/// trait without each adding `harn-clock` as a direct dependency.
pub use harn_clock as clock;

pub mod a2a;
pub mod actor_chain;
pub mod agent_events;
pub(crate) mod agent_session_journal;
pub mod agent_sessions;
pub mod agent_transcript_budget;
pub mod atomic_io;
pub mod autonomy;
pub(crate) mod aws_sigv4;
pub mod bridge;
mod builtin_id;
pub mod builtin_profile;
pub mod bytecode_cache;
pub mod call_budget;
pub mod canonical_json;
pub mod channel_guardrails;
pub mod channels;
pub mod checkpoint;
mod chunk;
mod compiler;
pub mod composition;
pub mod conditional_replace;
pub mod config;
pub mod connectors;
pub mod corrections;
pub mod coverage;
pub(crate) mod durable_rate_limit;
pub mod duration_parse;
pub mod egress;
pub mod event_log;
pub mod events;
pub mod external_agent;
pub mod flow;
pub mod harness;
pub mod harness_auth;
pub(crate) mod harness_crypto;
pub mod harness_net;
pub mod harness_system;
pub mod harness_tenant;
pub mod host_attachments;
mod http;
pub mod jsonrpc;
pub(crate) mod limits;
pub mod llm;
pub mod llm_config;
pub mod mcp;
pub mod mcp_allowlist;
pub mod mcp_auth;
pub mod mcp_bulk_auth;
pub mod mcp_card;
pub mod mcp_client_request;
pub mod mcp_client_roots;
pub mod mcp_elicit;
pub mod mcp_file_upload;
pub mod mcp_host;
pub mod mcp_identity;
pub mod mcp_json_discovery;
pub mod mcp_oauth;
pub mod mcp_presets;
pub mod mcp_progress;
pub mod mcp_protocol;
pub mod mcp_registry;
pub mod mcp_sampling;
pub mod mcp_server;
pub mod metadata;
pub mod module_artifact;
pub mod module_source;
pub mod observability;
pub mod op_interrupt;
pub mod orchestration;
mod persistent_state;
pub mod personas;
mod prepared_module;
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
pub(crate) mod runtime_sqlite;
pub mod schema;
pub(crate) mod secret_patterns;
pub mod secrets;
pub mod security;
pub mod session_bundle;
pub mod session_timeline;
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
pub(crate) mod test_env;
pub mod testbench;
pub mod text;
pub mod text_diff;
pub mod tool_annotations;
pub mod tool_call_cancellations;
pub mod tool_surface;
pub mod tracing;
pub mod triggers;
pub mod trust_graph;
pub(crate) mod url_encoding;
pub mod user_dirs;

/// Initialize process-wide assets whose construction should happen before an
/// embedding host enters an async request or VM execution stack.
///
/// Hosts should call this once at startup. The operation is idempotent, and VM
/// construction retains a fallback for embedders that do not have an explicit
/// bootstrap phase.
pub fn initialize_runtime_assets() {
    secret_patterns::initialize_default_secret_patterns();
}

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
        pub use crate::triggers::test_util::clock_leak::{
            drain, enter_scope, install_scope, instant_now, reset, snapshot, wall_now, ClockLeak,
            ClockLeakScope, ClockLeakScopeGuard,
        };
    }
}

pub mod typecheck;
pub mod value;
pub mod verification;
pub mod visible_text;
mod vm;
pub(crate) mod wait_for_graph;
pub mod waitpoints;
pub mod windows_path;
pub mod workspace_anchor;
pub mod workspace_path;

pub use persistent_state::{register_persistent_state_builtins_at_root, PersistentStateRoot};
pub use prepared_module::{PreparedModuleCache, PreparedModuleCacheStats};

pub use actor_chain::{
    ActorChain, ActorChainEntry, ActorChainError, Principal, ScopeAttenuationMode,
    ScopeAttenuationPolicy, ScopeAttenuationViolation,
};
pub use builtin_id::BuiltinId;
pub use call_budget::{
    charge_mcp_call, charge_pg_query, install_mcp_call_budget, install_pg_query_budget,
    McpCallBudgetGuard, PgQueryBudgetGuard,
};
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
    HarnessKind, HarnessLlm, HarnessNet, HarnessObs, HarnessProcess, HarnessRandom, HarnessSecrets,
    HarnessStdio, HarnessSystem, HarnessTenant, HarnessTerm, MockAwareClock, MockHarnessBuilder,
    VmHarness,
};
pub use harness_auth::{
    current_auth_principal, enter_auth_principal, AuthPrincipal, AuthPrincipalScopeGuard,
    MISSING_PRINCIPAL_MESSAGE,
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
    peek_llm_cost_budget, peek_llm_token_budget, register_session_end_hook, set_llm_cost_budget,
    set_llm_token_budget, LlmBudgetGuard, LlmTokenBudgetGuard, SessionEndHookRegistration,
};
pub use mcp::{connect_mcp_server_from_json, connect_mcp_server_from_spec, register_mcp_builtins};
pub use mcp_allowlist::{
    build_catalog as build_mcp_catalog, catalog_for_request as mcp_catalog_for_request,
    AdvertisedItem as McpAdvertisedItem, CatalogRequest as McpCatalogRequest, McpAllowlist,
    McpAllowlistItem, McpCatalog, McpCatalogItem, McpCatalogServer, McpItemKind,
    MCP_ALLOWLIST_SCHEMA_VERSION,
};
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
    take_mcp_serve_metadata, take_mcp_serve_prompts, take_mcp_serve_registry,
    take_mcp_serve_resource_templates, take_mcp_serve_resources, tool_registry_to_mcp_tools,
    McpServer, McpServerMetadata,
};
pub use metadata::register_metadata_builtins;
pub use observability::audit::{audit_events as audit_obs_events, AuditFinding, AuditFindingKind};
pub use observability::execution_scope::{
    current_execution_scope, enter_execution_scope, mint_execution_scope, ExecutionScopeGuard,
};
pub use observability::request_id::{current_request_id, enter_request_id, RequestIdScopeGuard};
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
#[cfg(feature = "postgres")]
pub use stdlib::install_shared_pool_registry;
pub use stdlib::io::{
    reserve_stdio_for_current_thread, set_stdout_passthrough, take_stderr_buffer,
    StdioReservationGuard,
};
pub use stdlib::long_running::cancel_handle as cancel_long_running_handle;
pub use stdlib::observability::install_default_backend as install_obs_default_backend;
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
    TRIGGER_INBOX_OBSERVABILITY_TOPIC, TRIGGER_OPERATION_AUDIT_TOPIC, TRIGGER_OUTBOX_TOPIC,
    TRIGGER_TEST_FIXTURES, WORKER_QUEUE_CATALOG_TOPIC,
};
pub use trust_graph::{
    append_active_scope_attenuation_alert, append_active_trust_record,
    append_scope_attenuation_alert, append_trust_record, export_trust_chain,
    group_trust_records_by_trace, policy_for_agent, policy_for_autonomy_tier,
    query_trust_graph_records, query_trust_records, resolve_agent_autonomy_tier,
    summarize_trust_records, topic_for_agent, trust_score_for, verify_trust_chain, AutonomyTier,
    TrustAgentSummary, TrustChainExport, TrustChainExportMetadata, TrustChainExportProducer,
    TrustChainReport, TrustGraphRecord, TrustOutcome, TrustQueryFilters, TrustRecord,
    TrustRecordActionKind, TrustScore, TrustTraceGroup, METADATA_KEY_ACTOR_CHAIN,
    METADATA_KEY_ACTOR_CHAIN_ALERT, METADATA_KEY_EFFECTS_GRANT, METADATA_KEY_EFFECTS_USED,
    METADATA_KEY_PARENT_RECORD_ID, OPENTRUSTGRAPH_ACCEPTED_SCHEMAS, OPENTRUSTGRAPH_CHAIN_SCHEMA_V0,
    OPENTRUSTGRAPH_SCHEMA_V0, OPENTRUSTGRAPH_SCHEMA_V0_1, TRUST_ACTION_RELEASE,
    TRUST_GRAPH_GLOBAL_TOPIC, TRUST_GRAPH_LEGACY_GLOBAL_TOPIC, TRUST_GRAPH_LEGACY_TOPIC_PREFIX,
    TRUST_GRAPH_RECORDS_TOPIC, TRUST_GRAPH_TOPIC_PREFIX,
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

/// Lowers Harn `TypeExpr`s to JSON Schema with `type`-alias EXPANSION, built once
/// from a parsed program's alias declarations. Without expansion, a tool parameter
/// typed as a user alias (`p: EvalSource`, `p: FunnelStage`) erases to an empty
/// `{}` schema because the low-level lowering only recognizes built-in type names —
/// the exporter must first resolve the alias to its underlying shape/union (a
/// literal-union alias then round-trips as a JSON `enum`). Cycle-safe via the same
/// `expand_alias` guard the compiler and typechecker share.
pub struct SchemaAliasResolver {
    compiler: compiler::Compiler,
}

impl SchemaAliasResolver {
    /// A resolver with no aliases in scope — lowering is identical to the raw
    /// (unexpanded) form, so `Named(alias)` still lowers to `{}` when unknown.
    pub fn empty() -> Self {
        Self {
            compiler: compiler::Compiler::new(),
        }
    }

    /// Collect every `type` alias declared in `program`, so named references in
    /// tool signatures resolve to their bodies.
    pub fn from_program(program: &[harn_parser::SNode]) -> Self {
        let mut compiler = compiler::Compiler::new();
        compiler.collect_type_aliases(program);
        Self { compiler }
    }

    /// JSON Schema for one `TypeExpr`, expanding any named alias first. `None`
    /// when the (expanded) type has no JSON-Schema form (function types, ...).
    pub fn json_schema_for_type_expr(
        &self,
        type_expr: &harn_parser::TypeExpr,
    ) -> Option<serde_json::Value> {
        let expanded = self.compiler.expand_alias(type_expr);
        let schema = compiler::Compiler::type_expr_to_schema_value(&expanded)?;
        let json_schema = schema::schema_to_json_schema_value(&schema).ok()?;
        Some(llm::vm_value_to_json(&json_schema))
    }

    /// JSON Schema `object` for a parameter list (a served tool's `inputSchema`),
    /// expanding aliases per parameter.
    pub fn json_schema_for_typed_params(
        &self,
        params: &[harn_parser::TypedParam],
    ) -> serde_json::Value {
        let mut properties = serde_json::Map::new();
        let mut required = Vec::new();

        for param in params {
            let param_schema = param
                .type_expr
                .as_ref()
                .and_then(|type_expr| self.json_schema_for_type_expr(type_expr))
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
}

/// Raw lowering with no program aliases in scope. Prefer
/// [`SchemaAliasResolver::from_program`] when serving a module so named-alias
/// parameters resolve instead of erasing to `{}`.
pub fn json_schema_for_type_expr(type_expr: &harn_parser::TypeExpr) -> Option<serde_json::Value> {
    SchemaAliasResolver::empty().json_schema_for_type_expr(type_expr)
}

pub fn json_schema_for_typed_params(params: &[harn_parser::TypedParam]) -> serde_json::Value {
    SchemaAliasResolver::empty().json_schema_for_typed_params(params)
}

#[cfg(test)]
mod schema_alias_resolver_tests {
    use super::*;

    fn fn_params_schema(src: &str) -> serde_json::Value {
        let program = harn_parser::parse_source(src).expect("parse test source");
        let resolver = SchemaAliasResolver::from_program(&program);
        for node in &program {
            let (_, inner) = harn_parser::peel_attributes(node);
            if let harn_parser::Node::FnDecl { params, .. } = &inner.node {
                return resolver.json_schema_for_typed_params(params);
            }
        }
        panic!("no fn decl in test source");
    }

    #[test]
    fn named_shape_alias_projects_like_inline_shape() {
        let inline = fn_params_schema("pub fn f(p: {kind: string, path: string}) {}");
        let aliased =
            fn_params_schema("type Src = {kind: string, path: string}\npub fn f(p: Src) {}");
        assert_eq!(
            aliased, inline,
            "a named shape alias must project the same inputSchema as its inline shape",
        );
        assert_ne!(
            aliased["properties"]["p"],
            serde_json::json!({}),
            "the alias parameter must not erase to an empty schema",
        );
    }

    #[test]
    fn literal_union_alias_projects_json_enum() {
        let schema = fn_params_schema("type Kind = \"local\" | \"ssh\"\npub fn f(p: Kind) {}");
        let p = &schema["properties"]["p"];
        assert_eq!(p["type"], "string");
        assert_eq!(p["enum"], serde_json::json!(["local", "ssh"]));
    }

    #[test]
    fn unknown_named_type_still_erases_to_empty() {
        // No alias declared: unchanged behavior — an unknown named type lowers to {}.
        let schema = fn_params_schema("pub fn f(p: Unknown) {}");
        assert_eq!(schema["properties"]["p"], serde_json::json!({}));
    }
}

fn reset_llm_state_for_thread_reset() {
    llm::reset_llm_state();
    #[cfg(test)]
    reset_thread_local_state_test_hooks::before_llm_global_reset();
    // This full wipe is necessary between Harn programs to clear durable
    // cooldowns that would otherwise stall a later run under a paused clock.
    llm::reset_rate_limit_registry();
    llm_config::clear_user_overrides();
    llm_config::clear_runtime_provider_endpoint_overrides();
}

#[cfg(test)]
mod reset_thread_local_state_test_hooks {
    use std::sync::{Arc, Mutex, OnceLock};

    type Hook = Arc<dyn Fn() + Send + Sync + 'static>;

    static BEFORE_LLM_GLOBAL_RESET: OnceLock<Mutex<Option<Hook>>> = OnceLock::new();

    fn before_llm_global_reset_hook() -> &'static Mutex<Option<Hook>> {
        BEFORE_LLM_GLOBAL_RESET.get_or_init(|| Mutex::new(None))
    }

    pub(crate) struct HookGuard;

    impl Drop for HookGuard {
        fn drop(&mut self) {
            let mut hook = before_llm_global_reset_hook()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *hook = None;
        }
    }

    pub(crate) fn install_before_llm_global_reset(hook: Hook) -> HookGuard {
        let mut slot = before_llm_global_reset_hook()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *slot = Some(hook);
        HookGuard
    }

    pub(crate) fn before_llm_global_reset() {
        let hook = before_llm_global_reset_hook()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(hook) = hook {
            hook();
        }
    }
}

/// Reset all thread-local state that can leak between test runs.
pub fn reset_thread_local_state() {
    #[cfg(test)]
    {
        // `reset_thread_local_state` is also used by in-process unit tests. It
        // clears process-global LLM config/rate-limit state, so share the same
        // lock used by LLM env tests; otherwise a sibling reset can erase a
        // parked rate-limit test's registry while the test still owns a permit.
        let _guard = llm::env_guard();
        reset_llm_state_for_thread_reset();
    }
    #[cfg(not(test))]
    reset_llm_state_for_thread_reset();

    http::reset_http_state();
    channels::reset_channel_state();
    event_log::reset_active_event_log();
    egress::clear_explicit_egress_policy_requirement_for_host();
    egress::clear_ssrf_guard_requirement_for_host();
    stdlib::reset_stdlib_state();
    connectors::clear_active_connector_clients();
    orchestration::clear_runtime_hooks();
    orchestration::clear_file_edit_queue();
    orchestration::clear_execution_policy_stacks();
    orchestration::clear_command_policies();
    orchestration::clear_pipeline_on_finish();
    orchestration::reset_lifecycle_receipt_registry();
    orchestration::agent_inbox::reset();
    tool_call_cancellations::reset_registry();
    redact::clear_policy_stack();
    security::reset_thread_state();
    triggers::clear_dispatcher_state();
    triggers::clear_trigger_registry();
    events::reset_event_sinks();
    tracing::set_tracing_enabled(false);
    tracing::reset_tracing();
    builtin_profile::reset();
    agent_events::reset_all_sinks();
    agent_sessions::reset_session_store();
    mcp_registry::reset();
    mcp_host::reset_for_tests();
    call_budget::reset_call_budget_state();
    clock_mock::leak_audit::reset();
}

#[cfg(test)]
mod reset_leak_tests {
    //! Regression coverage for harn#2660: process-/thread-global
    //! registries that accumulated one entry per test because they were
    //! never drained by `reset_thread_local_state`. Each case populates a
    //! registry through its real entry point, runs the reset, and asserts
    //! the registry is empty again.
    use super::*;
    use crate::value::VmValue;

    #[test]
    fn reset_drains_pending_file_edit_notifications() {
        orchestration::queue_file_edited("stale.harn", serde_json::json!({"operation": "write"}));

        reset_thread_local_state();

        assert!(
            orchestration::drain_file_edits().is_empty(),
            "a later VM run must not receive file edits queued by the previous run"
        );
    }

    /// The recorder is enabled per RUN but lives for the PROCESS, so an
    /// embedder that runs one script with `--profile` and the next without it
    /// would keep paying for bookkeeping nobody reads and fold the second run's
    /// builtins into the first run's totals. Enablement therefore ends where
    /// every other process-global ends: here.
    #[test]
    fn reset_disables_and_drains_builtin_profile() {
        let _guard = builtin_profile::test_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        builtin_profile::enable();
        builtin_profile::record("run_shell", std::time::Duration::from_millis(5));
        assert!(builtin_profile::is_enabled());
        assert!(!builtin_profile::snapshot().is_empty());

        reset_thread_local_state();

        assert!(
            !builtin_profile::is_enabled(),
            "a profiled run must not leave the recorder on for the next one"
        );
        assert!(
            builtin_profile::snapshot().is_empty(),
            "builtin totals must be empty after reset"
        );
    }

    /// The changed-path map is the authoritative source for a sub-agent's
    /// `files_written` receipt, is process-global, and is drained only at
    /// teardown — which a session that errors never reaches. A later session
    /// reusing the id would report writes it never made.
    #[test]
    fn reset_drains_session_changed_paths() {
        let session = "sess-leak";
        agent_sessions::open_or_create(Some(session.to_string()));
        agent_sessions::record_session_changed_path(session, "/tmp/written-by-a-dead-run.txt");
        assert!(!agent_sessions::session_changed_paths(session).is_empty());
        reset_thread_local_state();
        assert!(
            agent_sessions::session_changed_paths(session).is_empty(),
            "a receipt must not inherit an abandoned session's writes"
        );
    }

    #[test]
    fn reset_drains_agent_inbox() {
        orchestration::agent_inbox::reset();
        orchestration::agent_inbox::push("sess-2660", "note", "leak", "test");
        assert!(orchestration::agent_inbox::session_count() > 0);
        reset_thread_local_state();
        assert_eq!(
            orchestration::agent_inbox::session_count(),
            0,
            "agent_inbox must be empty after reset"
        );
    }

    #[test]
    fn reset_drains_tool_call_cancellation_registry() {
        tool_call_cancellations::reset_registry();
        // Leak the guard so the entry survives until the reset runs —
        // this mirrors a dispatch abandoned mid-flight.
        let registered = tool_call_cancellations::register("sess-2660", "call-1", "tool");
        if let Some((_handle, guard)) = registered {
            std::mem::forget(guard);
        }
        assert!(tool_call_cancellations::registry_len() > 0);
        reset_thread_local_state();
        assert_eq!(
            tool_call_cancellations::registry_len(),
            0,
            "tool-call cancellation registry must be empty after reset"
        );
    }

    #[test]
    fn reset_drains_routing_policy_registry() {
        llm::routing::clear_policy_registry();
        let mut config: crate::value::DictMap = crate::value::DictMap::new();
        config.insert(
            crate::value::intern_key("chain"),
            VmValue::List(std::sync::Arc::new(vec![VmValue::String(
                arcstr::ArcStr::from("mock:mock"),
            )])),
        );
        llm::routing::build_routing_policy(&config).expect("intern a routing policy");
        assert!(llm::routing::policy_registry_len() > 0);
        reset_thread_local_state();
        assert_eq!(
            llm::routing::policy_registry_len(),
            0,
            "routing policy registry must be empty after reset"
        );
    }

    #[test]
    fn reset_holds_llm_env_guard_while_wiping_llm_globals() {
        let observed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed_hook = std::sync::Arc::clone(&observed);
        let _hook = reset_thread_local_state_test_hooks::install_before_llm_global_reset(
            std::sync::Arc::new(move || {
                assert!(
                    matches!(
                        llm::env_lock().try_lock(),
                        Err(std::sync::TryLockError::WouldBlock)
                    ),
                    "reset_thread_local_state must hold env_guard before wiping LLM globals"
                );
                observed_hook.store(true, std::sync::atomic::Ordering::SeqCst);
            }),
        );

        reset_thread_local_state();
        assert!(
            observed.load(std::sync::atomic::Ordering::SeqCst),
            "LLM global reset hook should have run"
        );
    }
}
