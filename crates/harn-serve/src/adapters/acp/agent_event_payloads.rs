//! Typed bodies of `_harn/agentEvent` notifications.
//!
//! Each struct is the exact wire body for one event kind, minus the envelope's
//! `sessionId` and `kind`. The adapter serializes these types and the protocol
//! artifacts derive their bindings from the same types' JSON schema, so the
//! published shape cannot drift from what the adapter writes.
//!
//! A field typed `serde_json::Value` is a value Harn itself carries untyped:
//! a script-authored record, a provider payload, or a nested runtime record
//! with its own published schema.

use schemars::JsonSchema;
use serde::Serialize;
use serde_json::{Map, Value};

/// One `_harn/agentEvent` body and its wire discriminator.
pub trait AgentEventPayload: Serialize {
    const KIND: &'static str;
}

/// An event whose body is a whole record Harn does not type: a script-authored
/// signal or a handler diagnostic forwarded verbatim.
#[derive(Clone, Debug, Default, Serialize, JsonSchema)]
pub struct OpenRecord {
    #[serde(flatten)]
    pub fields: Map<String, Value>,
}

impl OpenRecord {
    /// A non-object body has no fields to forward; it would also fail the
    /// envelope's own object check, so it is projected as an empty record.
    pub fn from_value(value: Value) -> Self {
        match value {
            Value::Object(fields) => Self { fields },
            _ => Self::default(),
        }
    }
}

macro_rules! open_kinds {
    ($($kind:literal => $name:ident),* $(,)?) => {
        $(
            #[derive(Clone, Debug, Default, Serialize, JsonSchema)]
            #[serde(transparent)]
            pub struct $name(pub OpenRecord);

            impl $name {
                pub fn from_value(value: Value) -> Self {
                    Self(OpenRecord::from_value(value))
                }
            }
        )*
    };
}

open_kinds! {
    "agent_loop_stall_warning" => AgentLoopStallWarning,
    "cache_hit" => CacheHit,
    "cache_miss" => CacheMiss,
    "llm_call_log" => LlmCallLog,
    "llm_fallback_attempt" => LlmFallbackAttempt,
    "llm_routing_decision" => LlmRoutingDecision,
    "llm_shadow_diff" => LlmShadowDiff,
    "reserved_terminal_verify" => ReservedTerminalVerify,
    "semantic_cache_hit" => SemanticCacheHit,
    "semantic_cache_miss" => SemanticCacheMiss,
    "session_health" => SessionHealth,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentScratchpadReorganization {
    pub iteration: usize,
    pub status: String,
    pub details: Value,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AnchorChanged {
    pub previous: Option<Value>,
    pub current: Value,
    pub carry_transcript: bool,
    pub compacted: bool,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BoundaryFailure {
    pub boundary: String,
    pub failure_kind: String,
    pub owner: String,
    pub detail: String,
    pub dropped_count: usize,
    pub dropped_bytes: usize,
    pub unreported: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub excerpt: Option<String>,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BudgetCircuitBreaker {
    pub breaker_kind: String,
    pub consecutive_count: usize,
    pub paused_for_ms: u64,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BudgetExhausted {
    pub max_iterations: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iteration: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wall_clock_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<f64>")]
    pub limit_value: Option<serde_json::Number>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub projected_cost_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_cost_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub projected_input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub projected_output_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub projection_basis: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headroom_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityGap {
    pub level: String,
    pub capability: String,
    pub provider: String,
    pub model: String,
    pub fallback_tool_format: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requested_tool_format: Option<String>,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CodeLibrarianQueryNlFallback {
    pub attempted_cypher: Option<String>,
    pub mcts_depth: usize,
    pub mcts_expansions: usize,
    pub result_count: usize,
    pub text: String,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CompassRoutingDecision {
    pub tool_call_id: String,
    pub mode: String,
    pub action: String,
    pub persona: String,
    pub original_tool: String,
    pub routed_tool: String,
    pub target_tool: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// The run envelope shared by `composition_start`, `composition_finish`, and
/// `composition_error`.
#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CompositionRun {
    pub run_id: String,
    pub language: String,
    pub snippet_hash: String,
    pub binding_manifest_hash: String,
    pub requested_side_effect_ceiling: String,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    pub artifacts: Vec<Value>,
    pub result: Option<Value>,
    pub failure_category: Option<String>,
    pub error: Option<String>,
    pub duration_ms: Option<u64>,
    pub metadata: Value,
}

impl CompositionRun {
    pub fn new(run: &harn_vm::composition::CompositionRunEnvelope) -> Self {
        Self {
            run_id: run.run_id.clone(),
            language: run.language.clone(),
            snippet_hash: run.snippet_hash.clone(),
            binding_manifest_hash: run.binding_manifest_hash.clone(),
            requested_side_effect_ceiling: run.requested_side_effect_ceiling.as_str().to_string(),
            stdout: run.stdout.clone(),
            stderr: run.stderr.clone(),
            artifacts: run.artifacts.clone(),
            result: run.result.clone(),
            failure_category: run
                .failure_category
                .map(|category| category.as_str().to_string()),
            error: run.error.clone(),
            duration_ms: run.duration_ms,
            metadata: run.metadata.clone(),
        }
    }
}

macro_rules! composition_run_kinds {
    ($($name:ident),*) => {
        $(
            #[derive(Clone, Debug, Serialize, JsonSchema)]
            #[serde(transparent)]
            pub struct $name(pub CompositionRun);
        )*
    };
}

composition_run_kinds!(CompositionStart, CompositionFinish, CompositionError);

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CompositionChildCall {
    pub run_id: String,
    pub tool_call_id: String,
    pub tool_name: String,
    pub operation_index: u64,
    pub annotations: Option<Value>,
    pub requested_side_effect_level: String,
    pub policy_context: Value,
    pub raw_input: Value,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CompositionChildResult {
    pub run_id: String,
    pub tool_call_id: String,
    pub tool_name: String,
    pub operation_index: u64,
    pub status: String,
    pub raw_output: Option<Value>,
    pub error: Option<String>,
    pub error_category: Option<String>,
    /// A bare executor name, or `{"kind": "mcp_server", "serverName": ...}`.
    pub executor: Option<Value>,
    pub duration_ms: Option<u64>,
    pub execution_duration_ms: Option<u64>,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ControlOutcome {
    pub control_id: String,
    pub method: String,
    pub outcome: String,
    pub status: String,
    pub actor: Value,
    pub target: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DaemonWatchdogTripped {
    pub attempts: usize,
    pub elapsed_ms: u64,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FeedbackInjected {
    pub feedback_kind: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub streak: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iteration: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_claimed_for_repair: Option<bool>,
    /// `false` means the mechanism wrote a receipt and delivered nothing;
    /// only an absent field means the question went unanswered.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivered: Option<bool>,
}

/// Evidence that the tools-disabled final summary still contained a tool call.
#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FinalWrapupUnconsumedToolCall {
    pub parse_status: String,
    pub parsed_call_count: usize,
    pub tool_names: Vec<String>,
    pub diagnostics: Vec<String>,
    pub evidence_line: String,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FinalWrapup {
    pub final_status: String,
    pub stop_reason: String,
    pub iteration: usize,
    pub host_directive: bool,
    pub terminal_kind: String,
    pub unconsumed_tool_call: Option<FinalWrapupUnconsumedToolCall>,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HostAttachment {
    pub injection_id: String,
    pub media_type: String,
    pub flavor: Value,
    pub artifact_pointer: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub rendered: Value,
    pub description: Option<String>,
    pub description_model: Option<String>,
    pub delivery: Value,
    pub delivered_at_seam: Option<String>,
    pub sequence: u64,
    pub provenance: Value,
    pub sanitization: Value,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HostToolResult {
    pub injection_id: String,
    pub tool_call_id: String,
    pub tool_name: String,
    pub tool_kind: Option<Value>,
    pub raw_input: Value,
    pub status: String,
    pub raw_output: Option<Value>,
    pub result_pointer: Option<String>,
    pub error: Option<String>,
    pub duration_ms: Option<u64>,
    pub delivery: Value,
    pub delivered_at_seam: Option<String>,
    pub sequence: u64,
    pub provenance: Value,
    pub sanitization: Value,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InputGuardrailVerdict {
    pub iteration: usize,
    pub tripwire: bool,
    pub reason: String,
    pub label: String,
    pub confidence: f64,
    pub confidence_threshold: f64,
    pub classifier_kind: Option<String>,
    pub model: Option<String>,
    pub error: Option<String>,
}

/// `iterationInfo` is the agent loop's script-authored iteration record.
#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct IterationEnd {
    pub iteration: usize,
    pub iteration_info: Value,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct IterationStart {
    pub iteration: usize,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub provider: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub model: String,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct JudgeDecision {
    pub iteration: usize,
    /// The answer of record. `judgeVerdict` and `override` tell an override
    /// apart from the judge's own word.
    pub verdict: String,
    pub reasoning: String,
    pub next_step: Option<String>,
    pub judge_duration_ms: u64,
    pub source: Option<String>,
    pub trigger: Option<String>,
    pub reason: Option<String>,
    pub confirm: Option<bool>,
    pub converted_from: Option<String>,
    pub escalation_recommended: Option<bool>,
    pub escalation_target: Option<String>,
    pub judge_verdict: Option<String>,
    #[serde(rename = "override")]
    pub judge_override: Option<Value>,
    pub gap_class: Option<String>,
    pub judge_outcome: Option<String>,
    pub terminal_evidence_preserved: Option<bool>,
    pub requirement_report: Option<Value>,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct JudgeStarted {
    pub iteration: usize,
    pub trigger: Option<String>,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LoopCheckpoint {
    pub iteration: usize,
    pub checkpoint_kind: String,
    pub delivered: usize,
    #[serde(skip_serializing_if = "is_zero")]
    pub inbox_delivered: usize,
    #[serde(skip_serializing_if = "is_zero")]
    pub typed_delivered: usize,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub dispatch_skipped: bool,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LoopControlDecision {
    pub iteration: usize,
    pub action: String,
    pub old_limit: usize,
    pub new_limit: usize,
    pub reason: String,
    pub status: String,
}

/// The loop's stuck detector, or a script-authored stuck signal whose record
/// is forwarded under `signal` fields of its own.
#[derive(Clone, Debug, Default, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LoopStuck {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_nudges: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_iteration: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tail_excerpt: Option<String>,
    #[serde(flatten)]
    pub signal: Map<String, Value>,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct McpAuthRequired {
    pub server: String,
    pub resource: String,
    pub scope: Option<String>,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct McpCatalogChanged {
    pub server: Option<String>,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct McpNotification {
    pub server: String,
    pub method: String,
    pub direction: String,
    pub params: Value,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MissingToolCallVerdict {
    pub iteration: usize,
    pub action: String,
    pub original_action: String,
    pub tool_name: String,
    pub confidence: f64,
    pub confidence_threshold: f64,
    pub evidence: String,
    pub language: Option<String>,
    pub classifier_kind: Option<String>,
    pub model: Option<String>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModelJob {
    pub event: Value,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationDecision {
    pub decision: Value,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PackThinkingStripped {
    pub model: String,
    pub requested: String,
    pub reason: String,
}

/// A progress report with neither plan entries nor a message; reports that
/// carry either ride ACP's own plan or narration updates instead.
#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProgressReported {
    pub message: Option<String>,
    pub entries: Value,
    pub replace: bool,
    pub metadata: Value,
}

/// Provider-reported usage for one model call. Its keys are snake_case.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct ProviderUsage {
    pub call_id: String,
    pub provider: String,
    pub model: String,
    pub run_usage: Value,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PurposeLabel {
    pub label: String,
    /// `declared` by the agent, or `inferred` by a harness-side labeler.
    pub source: String,
    pub iteration: Option<usize>,
    pub tool_call_ids: Vec<String>,
    pub tool_call_count: Option<usize>,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RepairOutputContractApplied {
    pub iteration: usize,
    pub tool_count: usize,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RequireSuccessfulToolsViolation {
    pub violation_kind: String,
    pub source: String,
    pub actor: Option<String>,
    pub run_id: Option<String>,
    pub redacted_summary: String,
    pub recurrence_hints: Vec<String>,
    pub metadata: Value,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ScopeClassifierVerdict {
    pub iteration: usize,
    pub label: String,
    pub original_label: String,
    pub confidence: f64,
    pub confidence_threshold: f64,
    pub evidence: String,
    pub skip_main_turn: bool,
    pub classifier_kind: Option<String>,
    pub model: Option<String>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SelfConsistencyTie {
    pub answer: String,
    pub total: usize,
    pub distribution: Value,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionClosed {
    pub reason: String,
    pub status: String,
    pub metadata: Value,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StepJudgeDecision {
    pub iteration: usize,
    pub verdict: String,
    pub reasoning: String,
    pub critique: String,
    pub confidence: f64,
    pub judge_duration_ms: u64,
    pub vetoed: bool,
    pub skipped: bool,
    pub reason: Option<String>,
    pub judge_error: bool,
    pub unavailable_reason: Option<String>,
    pub unavailable_count: usize,
    pub on_veto: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: f64,
    pub provider: String,
    pub model: String,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StructuralValidatorDecision {
    pub iteration: usize,
    pub rule: String,
    pub diagnostic: String,
    pub recommended_action: String,
    pub vetoed: bool,
    pub skipped: bool,
    pub reason: Option<String>,
    pub on_failure: String,
    pub attempts: usize,
    pub max_attempts: usize,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SubagentJoin {
    pub parent_session_id: String,
    pub parent_run_id: String,
    pub child_session_id: String,
    pub child_run_id: String,
    pub worker_id: String,
    pub completed_at_ms: i64,
    pub joined_at_ms: i64,
    /// How long the finished child waited to be collected, not the parent's
    /// wait; `waitMs` is the parent's wait.
    pub collection_lag_ms: i64,
    pub wait_started_at_ms: Option<i64>,
    pub wait_ms: Option<u64>,
    pub result_processing_started_at_ms: Option<i64>,
    pub result_processing_completed_at_ms: Option<i64>,
    pub result_processing_ms: Option<u64>,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SubagentStop {
    pub parent_run_id: String,
    pub child_run_id: String,
    pub parent_session_id: Option<String>,
    pub child_session_id: Option<String>,
    pub terminal_status: String,
    pub terminal_class: String,
    pub reason: String,
    pub result_ref: Option<String>,
    pub receipt_ref: Option<String>,
    pub cancellation: Option<Value>,
    pub timeout: Option<Value>,
    pub completed_at_ms: i64,
}

/// `receipt` follows the published tool-batch disposition receipt.
#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ToolBatchDisposition {
    pub receipt: Value,
}

/// `audit` is the middleware's script-authored record; `receipt` follows the
/// published tool-call receipt schema.
#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallAudit {
    pub tool_call_id: String,
    pub tool_name: String,
    pub audit: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receipt: Option<Value>,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ToolFormatOverride {
    pub provider: String,
    pub model: String,
    pub requested_format: String,
    pub recommended_format: String,
    pub catalog_parity: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub override_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub applied_format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub steered: Option<bool>,
}

/// The producer-owned turn phase. `candidate_reply` accompanies `verifying`;
/// `reply` and `outcome` accompany `terminal`.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct TurnPhaseChanged {
    pub phase: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidate_reply: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<Value>,
}

impl TurnPhaseChanged {
    pub fn new(phase: &harn_vm::agent_events::AgentTurnPhase) -> Self {
        use harn_vm::agent_events::AgentTurnPhase;
        let (name, candidate_reply, reply, outcome) = match phase {
            AgentTurnPhase::Generating => ("generating", None, None, None),
            AgentTurnPhase::Verifying { candidate_reply } => {
                ("verifying", Some(candidate_reply.clone()), None, None)
            }
            AgentTurnPhase::Terminal { reply, outcome } => (
                "terminal",
                None,
                Some(reply.clone()),
                Some(serde_json::to_value(outcome).expect("terminal outcome serializes")),
            ),
        };
        Self {
            phase: name.to_string(),
            candidate_reply,
            reply,
            outcome,
        }
    }
}

/// `checkpoint` is a script-authored record discriminated by its `schema`.
#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TypedCheckpoint {
    pub checkpoint: Value,
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

macro_rules! agent_event_registry {
    ($($kind:literal => $name:ident),* $(,)?) => {
        $(impl AgentEventPayload for $name { const KIND: &'static str = $kind; })*

        /// Every kind the adapter emits through `_harn/agentEvent`. Adding a
        /// kind is additive; clients SHOULD ignore a kind they do not know.
        pub const HARN_AGENT_EVENT_KINDS: &[&str] = &[$($kind),*];

        /// `(kind, record name, schema)` for every registered body, in
        /// registry order.
        fn payload_schemas() -> Vec<(&'static str, &'static str, Value)> {
            vec![$(($kind, stringify!($name), payload_schema::<$name>())),*]
        }
    };
}

agent_event_registry! {
    "agent_loop_stall_warning" => AgentLoopStallWarning,
    "agent_scratchpad_reorganization" => AgentScratchpadReorganization,
    "anchor_changed" => AnchorChanged,
    "boundary_failure" => BoundaryFailure,
    "budget_circuit_breaker" => BudgetCircuitBreaker,
    "budget_exhausted" => BudgetExhausted,
    "cache_hit" => CacheHit,
    "cache_miss" => CacheMiss,
    "capability_gap" => CapabilityGap,
    "code_librarian_query_nl_fallback" => CodeLibrarianQueryNlFallback,
    "compass_routing_decision" => CompassRoutingDecision,
    "composition_child_call" => CompositionChildCall,
    "composition_child_result" => CompositionChildResult,
    "composition_error" => CompositionError,
    "composition_finish" => CompositionFinish,
    "composition_start" => CompositionStart,
    "control_outcome" => ControlOutcome,
    "daemon_watchdog_tripped" => DaemonWatchdogTripped,
    "feedback_injected" => FeedbackInjected,
    "final_wrapup" => FinalWrapup,
    "host_attachment" => HostAttachment,
    "host_tool_result" => HostToolResult,
    "input_guardrail_verdict" => InputGuardrailVerdict,
    "iteration_end" => IterationEnd,
    "iteration_start" => IterationStart,
    "judge_decision" => JudgeDecision,
    "judge_started" => JudgeStarted,
    "llm_call_log" => LlmCallLog,
    "llm_fallback_attempt" => LlmFallbackAttempt,
    "llm_routing_decision" => LlmRoutingDecision,
    "llm_shadow_diff" => LlmShadowDiff,
    "loop_checkpoint" => LoopCheckpoint,
    "loop_control_decision" => LoopControlDecision,
    "loop_stuck" => LoopStuck,
    "mcp_auth_required" => McpAuthRequired,
    "mcp_catalog_changed" => McpCatalogChanged,
    "mcp_notification" => McpNotification,
    "missing_tool_call_verdict" => MissingToolCallVerdict,
    "model_job" => ModelJob,
    "orchestration_decision" => OrchestrationDecision,
    "pack_thinking_stripped" => PackThinkingStripped,
    "progress_reported" => ProgressReported,
    "provider_usage" => ProviderUsage,
    "purpose_label" => PurposeLabel,
    "repair_output_contract_applied" => RepairOutputContractApplied,
    "require_successful_tools_violation" => RequireSuccessfulToolsViolation,
    "reserved_terminal_verify" => ReservedTerminalVerify,
    "scope_classifier_verdict" => ScopeClassifierVerdict,
    "self_consistency_tie" => SelfConsistencyTie,
    "semantic_cache_hit" => SemanticCacheHit,
    "semantic_cache_miss" => SemanticCacheMiss,
    "session_closed" => SessionClosed,
    "session_health" => SessionHealth,
    "step_judge_decision" => StepJudgeDecision,
    "structural_validator_decision" => StructuralValidatorDecision,
    "subagent_join" => SubagentJoin,
    "subagent_stop" => SubagentStop,
    "tool_batch_disposition" => ToolBatchDisposition,
    "tool_call_audit" => ToolCallAudit,
    "tool_format_override" => ToolFormatOverride,
    "turn_phase_changed" => TurnPhaseChanged,
    "typed_checkpoint" => TypedCheckpoint,
}

pub const AGENT_EVENT_SCHEMA_ARTIFACT: &str = "schemas/agent-event.schema.json";

fn payload_schema<T: JsonSchema>() -> Value {
    let generator = schemars::generate::SchemaSettings::draft2020_12()
        .for_serialize()
        .with(|settings| settings.inline_subschemas = true)
        .into_generator();
    let mut schema = serde_json::to_value(generator.into_root_schema_for::<T>())
        .expect("agent event payload schema serializes");
    let object = schema.as_object_mut().expect("payload schema is an object");
    object.remove("$schema");
    object.remove("title");
    schema
}

/// JSON Schema for `_harn/agentEvent` params: a `sessionId`, a `kind`, and
/// the kind's body. `$defs` names each body after its Rust type and
/// `x-harn-kinds` maps every kind to its definition.
pub fn agent_event_schema() -> Value {
    let mut definitions = Map::new();
    let mut kinds = Map::new();
    let mut variants = Vec::new();
    for (kind, name, schema) in payload_schemas() {
        definitions.insert(name.to_string(), schema);
        kinds.insert(kind.to_string(), Value::String(name.to_string()));
        variants.push(serde_json::json!({
            "if": {"properties": {"kind": {"const": kind}}, "required": ["kind"]},
            "then": {"$ref": format!("#/$defs/{name}")},
        }));
    }
    serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": "https://harnlang.com/schemas/agent-event.v1.json",
        "title": "Harn agent event",
        "type": "object",
        "required": ["sessionId", "kind"],
        "properties": {
            "sessionId": {"type": "string"},
            "kind": {"type": "string"},
            "replayed": {"type": "boolean"},
        },
        "allOf": variants,
        "x-harn-kinds": kinds,
        "$defs": definitions,
    })
}
