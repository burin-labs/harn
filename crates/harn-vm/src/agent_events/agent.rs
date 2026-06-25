use serde::{Deserialize, Serialize};

use crate::composition::{CompositionChildCall, CompositionChildResult, CompositionRunEnvelope};
use crate::llm::receipts::ToolCallReceipt;
use crate::orchestration::{HandoffArtifact, MutationSessionRecord};
use crate::tool_annotations::ToolKind;

use super::tool::{ToolCallErrorCategory, ToolCallStatus, ToolExecutor};
use super::worker::{FsWatchEvent, WorkerEvent};

/// Events emitted by the agent loop. Some variants map 1:1 to ACP
/// `sessionUpdate` variants; Harn-specific lifecycle events ride on the
/// extension stream.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    AgentMessageChunk {
        session_id: String,
        content: String,
    },
    AgentThoughtChunk {
        session_id: String,
        content: String,
    },
    UserMessage {
        session_id: String,
        message_id: String,
        content: Vec<serde_json::Value>,
    },
    ToolCall {
        session_id: String,
        tool_call_id: String,
        tool_name: String,
        kind: Option<ToolKind>,
        status: ToolCallStatus,
        raw_input: serde_json::Value,
        /// Set to `Some(true)` by the streaming candidate detector
        /// (harn#692) when this event represents a tool-call shape
        /// detected in the model's in-flight assistant text but whose
        /// arguments have not finished parsing yet. Clients can render a
        /// spinner / placeholder while the model writes the body. The
        /// detector follows up with a `ToolCallUpdate { parsing: false,
        /// .. }` carrying either `status: pending` (promoted) or
        /// `status: failed` with `error_category: parse_aborted`.
        /// `None` (the default) means "this is a normal post-parse tool
        /// call, no candidate phase was active" so the on-disk shape
        /// stays compatible with replays recorded before this field
        /// existed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parsing: Option<bool>,
        /// Mutation-session audit context active when the tool was
        /// dispatched (see harn#699). Hosts use it to group every tool
        /// emission belonging to the same write-capable session.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        audit: Option<MutationSessionRecord>,
    },
    ToolCallUpdate {
        session_id: String,
        tool_call_id: String,
        tool_name: String,
        status: ToolCallStatus,
        raw_output: Option<serde_json::Value>,
        error: Option<String>,
        /// Wall-clock milliseconds from the parse-to-execution boundary
        /// to the terminal `Completed`/`Failed` update. Includes the
        /// time spent in any wrapping orchestration logic (loop checks,
        /// post-tool hooks, microcompaction). Populated only on the
        /// terminal update — `None` on intermediate `Pending` /
        /// `InProgress` updates so clients can ignore the field until
        /// it shows up.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
        /// Milliseconds spent in the actual host/builtin/MCP dispatch
        /// call only (the inner `dispatch_tool_execution` window).
        /// Populated only on the terminal update; `None` otherwise.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        execution_duration_ms: Option<u64>,
        /// Structured classification of the failure (when `status` is
        /// `Failed`). Paired with `error` so clients can render each
        /// category distinctly without parsing free-form strings. Always
        /// `None` for non-Failed updates and serialized as
        /// `errorCategory` in the ACP wire format.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error_category: Option<ToolCallErrorCategory>,
        /// Where the tool actually ran. `None` only for events emitted
        /// from sites that pre-date the dispatch decision (e.g. the
        /// pending → in-progress transition the loop emits before the
        /// dispatcher picks a backend).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        executor: Option<ToolExecutor>,
        /// Companion to `ToolCall.parsing` (harn#692). The streaming
        /// candidate detector emits the *terminal* candidate event as a
        /// `ToolCallUpdate` with `parsing: Some(false)` to retract the
        /// in-flight `parsing: true` chip — either by promoting the
        /// candidate (`status: pending`, populated `raw_output: None`,
        /// `error: None`) or aborting it (`status: failed`,
        /// `error_category: parse_aborted`). `None` means this update is
        /// not part of a candidate-phase transition.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parsing: Option<bool>,
        /// Best-effort partial parse of the streamed tool-call arguments.
        /// Populated by the SSE transport on `Pending` updates as the
        /// model streams `input_json_delta` (Anthropic) or
        /// `tool_calls[].function.arguments` deltas (OpenAI). `None` on
        /// terminal updates and on emissions from non-streaming paths
        /// (#693). When the partial bytes are not yet parseable as JSON
        /// the transport falls back to `raw_input_partial`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_input: Option<serde_json::Value>,
        /// Raw concatenated bytes of the streamed tool-call arguments
        /// when a permissive parse failed (#693). Mutually exclusive
        /// with `raw_input`: clients render whichever is present.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_input_partial: Option<String>,
        /// Mutation-session audit context for the tool call. Carries the
        /// same payload as on the paired `ToolCall` event so a host
        /// processing a single update doesn't have to correlate against
        /// the prior pending event.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        audit: Option<MutationSessionRecord>,
    },
    Plan {
        session_id: String,
        plan: serde_json::Value,
    },
    ProgressReported {
        session_id: String,
        message: Option<String>,
        entries: serde_json::Value,
        replace: bool,
        metadata: serde_json::Value,
    },
    /// Emitted when the Burin compass observes a freeform edit and either
    /// suggests a structural primitive, rewrites the tool call, or falls
    /// back because rewrite mode could not prove equivalence.
    CompassRoutingDecision {
        session_id: String,
        tool_call_id: String,
        mode: String,
        action: String,
        persona: String,
        original_tool: String,
        routed_tool: String,
        target_tool: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
    /// Emitted after an agent scratchpad reorganization attempt. The
    /// scratchpad body itself stays in session state; this event carries
    /// status and count/error metadata so live UIs and eval harnesses can
    /// audit whether reorganization helped or damaged the working set.
    AgentScratchpadReorganization {
        session_id: String,
        iteration: usize,
        status: String,
        details: serde_json::Value,
    },
    /// A renderable, declarative artifact spec emitted by an agent. Harn
    /// validates the payload and transports it; host surfaces own rendering
    /// and may fall back to the plain-text representation.
    Artifact {
        session_id: String,
        artifact_id: String,
        kind: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        mime_type: String,
        spec: serde_json::Value,
        fallback: String,
        size_bytes: u64,
        provenance: serde_json::Value,
        metadata: serde_json::Value,
    },
    /// Fires at the top of every model round-trip inside an
    /// `agent_loop` invocation. Maps to the `iteration_start` steering
    /// seam. Not the same as ACP's outer `prompt_turn` boundary; an
    /// `agent_turn`/`prompt_turn` cycle contains many of these.
    IterationStart {
        session_id: String,
        iteration: usize,
        /// Configured provider for the impending LLM call. Empty when the
        /// caller defers provider selection to the routing layer.
        /// Surfaces here so observers can show "about to call X/Y" before
        /// the call returns — previously this only landed in the
        /// transcript after the response, leaving live pulse-check
        /// consumers without a model attribution for in-flight iterations.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        provider: String,
        /// Configured model id. Same semantics as `provider`.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        model: String,
    },
    /// Fires at the bottom of every model round-trip, after tool
    /// dispatch (or after the dispatch was skipped). Sibling of
    /// `IterationStart`.
    IterationEnd {
        session_id: String,
        iteration: usize,
        /// Free-form dict carrying the post-call snapshot. Stable keys
        /// emitted by the agent loop: `tool_count`, `text`, plus the
        /// LLM-result projection — `provider`, `model`, `response_ms`,
        /// `input_tokens`, `output_tokens`, `thinking_chars`. Hosts that
        /// surface latency/cost panes key off these without re-parsing
        /// the transcript JSONL.
        iteration_info: serde_json::Value,
    },
    /// Emitted when a first-class agent session is explicitly closed by
    /// `agent_session_close`. This gives event-log consumers a typed
    /// terminal marker even when no final model turn runs.
    SessionClosed {
        session_id: String,
        reason: String,
        status: String,
        metadata: serde_json::Value,
    },
    /// Emitted when `agent_session_reanchor` swaps the primary workspace
    /// anchor (#2218). Hosts use this to drive cross-project handoff UX.
    /// Carries the previous and current anchors so consumers can diff
    /// without re-fetching session state.
    AnchorChanged {
        session_id: String,
        previous: Option<serde_json::Value>,
        current: serde_json::Value,
        carry_transcript: bool,
        compacted: bool,
        reason: Option<String>,
    },
    JudgeDecision {
        session_id: String,
        iteration: usize,
        verdict: String,
        reasoning: String,
        next_step: Option<String>,
        judge_duration_ms: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trigger: Option<String>,
    },
    /// Per-step critique decision emitted by `agent_step_judge`.
    /// Sibling of [`JudgeDecision`] but fired BEFORE tool dispatch on
    /// every assistant turn (when configured), not just at completion.
    /// `on_veto` carries the configured remediation shape
    /// (`"replace"` or `"retain"`); `cost_usd` is best-effort from the
    /// stdlib economics estimator and may be 0 when pricing is unknown.
    /// `skipped` marks configured short-circuits that did not call the
    /// judge model.
    StepJudgeDecision {
        session_id: String,
        iteration: usize,
        verdict: String,
        reasoning: String,
        critique: String,
        confidence: f64,
        judge_duration_ms: u64,
        vetoed: bool,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        skipped: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        /// True when this `verdict: "pass"` is the result of the step-judge
        /// model itself erroring and `fail_open` swallowing the error — the
        /// turn proceeded, but the adversarial-review surface was UNAVAILABLE
        /// (not a genuine approval). Lets telemetry tell an inert reviewer
        /// apart from a real pass. Mirrors `reason: "judge_unavailable"`.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        judge_error: bool,
        on_veto: String,
        input_tokens: u64,
        output_tokens: u64,
        cost_usd: f64,
        provider: String,
        model: String,
    },
    /// Deterministic pre-dispatch critique emitted by the structural
    /// validator middleware. Fires before any LLM-backed judge so hosts
    /// can distinguish "$0 structural retry" from semantic critique.
    StructuralValidatorDecision {
        session_id: String,
        iteration: usize,
        rule: String,
        diagnostic: String,
        recommended_action: String,
        vetoed: bool,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        skipped: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        on_failure: String,
        attempts: usize,
        max_attempts: usize,
    },
    ScopeClassifierVerdict {
        session_id: String,
        iteration: usize,
        label: String,
        original_label: String,
        confidence: f64,
        confidence_threshold: f64,
        evidence: String,
        skip_main_turn: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        classifier_kind: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    MissingToolCallVerdict {
        session_id: String,
        iteration: usize,
        action: String,
        original_action: String,
        tool_name: String,
        confidence: f64,
        confidence_threshold: f64,
        evidence: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        language: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        classifier_kind: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    TypedCheckpoint {
        session_id: String,
        checkpoint: serde_json::Value,
    },
    FeedbackInjected {
        session_id: String,
        kind: String,
        content: String,
    },
    /// Emitted when the agent loop exhausts `max_iterations` without any
    /// explicit break condition firing. Distinct from a natural "done" or
    /// a "stuck" nudge-exhaustion: this is strictly a budget cap.
    BudgetExhausted {
        session_id: String,
        max_iterations: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cost_usd: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        wall_clock_ms: Option<u64>,
    },
    /// Emitted when a loop-level budget circuit breaker trips after N
    /// consecutive retryable failures. `paused_for_ms` is the mock-time-aware
    /// backoff already honored before the terminal budget event.
    BudgetCircuitBreaker {
        session_id: String,
        kind: String,
        consecutive_count: usize,
        paused_for_ms: u64,
    },
    /// Emitted when the loop breaks because consecutive text-only turns
    /// hit `max_nudges`. Parity with `BudgetExhausted` / `IterationEnd` for
    /// hosts that key off agent-terminal events.
    LoopStuck {
        session_id: String,
        max_nudges: usize,
        last_iteration: usize,
        tail_excerpt: String,
    },
    /// Emitted when the daemon idle-wait loop trips its watchdog because
    /// every configured wake source returned `None` for N consecutive
    /// attempts. Exists so a broken daemon doesn't hang the session
    /// silently.
    DaemonWatchdogTripped {
        session_id: String,
        attempts: usize,
        elapsed_ms: u64,
    },
    /// Emitted when a skill is activated. Carries the match reason so
    /// replayers can reconstruct *why* a given skill took effect at
    /// this iteration.
    SkillActivated {
        session_id: String,
        skill_name: String,
        iteration: usize,
        reason: String,
    },
    /// Emitted when a previously-active skill is deactivated because
    /// the reassess phase no longer matches it.
    SkillDeactivated {
        session_id: String,
        skill_name: String,
        iteration: usize,
    },
    /// Emitted once per activation when the skill's `allowed_tools` filter
    /// narrows the effective tool surface exposed to the model.
    SkillScopeTools {
        session_id: String,
        skill_name: String,
        allowed_tools: Vec<String>,
    },
    /// Emitted when the agent loop ratchets the model-visible tool
    /// surface narrower after observing recent tool-call usage. Unlike
    /// `SkillScopeTools`, this is session-local and can only remove
    /// tools from the currently-effective surface.
    SkillNarrow {
        session_id: String,
        reason: String,
        removed_tools: Vec<String>,
        remaining_tools: Vec<String>,
        #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
        policy: serde_json::Value,
        #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
        removed_tool_details: serde_json::Value,
        #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
        kept_tool_details: serde_json::Value,
    },
    /// Emitted when a `tool_search` query is issued by the model. Carries
    /// the raw query args, the configured strategy, and a `mode` tag
    /// distinguishing the client-executed fallback (`"client"`) from
    /// provider-native paths (`"anthropic"` / `"openai"`). Mirrors the
    /// transcript event shape so hosts can render a search-in-progress
    /// chip in real time — the replay path walks the transcript after
    /// the turn, which is too late for live UX.
    ToolSearchQuery {
        session_id: String,
        tool_use_id: String,
        name: String,
        query: serde_json::Value,
        strategy: String,
        mode: String,
    },
    /// Emitted when `tool_search` resolves — carries the list of tool
    /// names newly promoted into the model's effective surface for the
    /// next turn. Pair-emitted with `ToolSearchQuery` on every search.
    ToolSearchResult {
        session_id: String,
        tool_use_id: String,
        promoted: Vec<String>,
        strategy: String,
        mode: String,
    },
    TranscriptCompacted {
        session_id: String,
        mode: String,
        reason: String,
        strategy: String,
        archived_messages: usize,
        estimated_tokens_before: usize,
        estimated_tokens_after: usize,
        snapshot_asset_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        instruction_mode: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        instruction_source: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        compaction_policy: Option<serde_json::Value>,
    },
    /// Emitted whenever `transcript_project` derives a model-visible
    /// prefix from the immutable raw transcript. Hosts that render a
    /// side-by-side raw/projected view subscribe to this — the typed
    /// payload mirrors the metadata on the persisted
    /// `transcript.projection` transcript event so clients don't have to
    /// re-parse the transcript to sync UI state.
    TranscriptProjected {
        session_id: String,
        policy: String,
        reason: String,
        prefix_hash: String,
        kept_count: usize,
        dropped_count: usize,
        provider_safety_blocked: bool,
        #[serde(default, skip_serializing_if = "is_zero_usize")]
        redacted_count: usize,
        #[serde(default, skip_serializing_if = "is_zero_usize")]
        reclaimed_tokens: usize,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        roots_consulted: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        redaction_pointers: Vec<serde_json::Value>,
    },
    /// Emitted when a pending `system_reminder` is rendered into the
    /// next provider request. ACP clients show these in a reminder lane
    /// instead of mixing them into assistant text chunks.
    ReminderEmitted {
        session_id: String,
        reminder_id: String,
        tags: Vec<String>,
        body: String,
        role_hint: String,
        rendered_role: String,
        source: String,
        ttl_turns: Option<i64>,
    },
    Handoff {
        session_id: String,
        artifact_id: String,
        handoff: Box<HandoffArtifact>,
    },
    FsWatch {
        session_id: String,
        subscription_id: String,
        events: Vec<FsWatchEvent>,
    },
    /// Emitted when hostlib staged filesystem state changes for a session.
    /// The ACP adapter maps this to the existing `progress` extension so
    /// clients can update rollup-diff badges without waiting for a prompt
    /// turn boundary.
    StagedWritesPending {
        session_id: String,
        pending_count: usize,
        total_bytes: u64,
    },
    /// Per-call outcome of `hostlib_fs_safe_text_patch`. Hosts subscribe to
    /// this to roll up stale-base / hunk-conflict rates and average
    /// hunks-per-patch without scraping result dicts out of pipeline logs.
    /// Fired from both the staged-overlay and direct-disk code paths so
    /// the rollup is comprehensive.
    SafeTextPatchResult {
        session_id: String,
        path: String,
        result: String,
        hunks_count: usize,
        bytes_written: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        failed_hunk_index: Option<usize>,
    },
    /// ACP control-plane arbitration outcome. Emitted for accepted,
    /// idempotent, and rejected controls so replay/audit consumers can show
    /// who acted and why a late or unauthorized action lost.
    ControlOutcome {
        session_id: String,
        control_id: String,
        method: String,
        outcome: String,
        status: String,
        actor: serde_json::Value,
        target: serde_json::Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
        metadata: serde_json::Value,
    },
    /// Lifecycle update for a delegated/background worker. Carries the
    /// canonical typed `event` variant alongside the worker's current
    /// `status` string and the structured `metadata` payload that
    /// `worker_bridge_metadata` builds (task, mode, timing, child
    /// run/snapshot paths, audit-session, etc.). The `audit` field is
    /// the same `MutationSessionRecord` JSON serialization carried on
    /// the bridge wire so ACP/A2A consumers don't need to re-derive it.
    ///
    /// One-to-one with the bridge-side `worker_update` session-update
    /// notification: ACP and A2A adapters subscribe to this variant
    /// and translate it into their respective wire formats. The
    /// `session_id` is the parent agent session that owns the worker
    /// (i.e. the session whose VM spawned the worker), so a single
    /// host stays subscribed to the same sink for both message and
    /// worker traffic.
    WorkerUpdate {
        session_id: String,
        worker_id: String,
        worker_name: String,
        worker_task: String,
        worker_mode: String,
        event: WorkerEvent,
        status: String,
        metadata: serde_json::Value,
        audit: Option<serde_json::Value>,
    },
    /// A human-in-the-loop primitive (`ask_user`, `request_approval`,
    /// `dual_control`, `escalate`) has just suspended the script and is
    /// waiting on a response. Hosts that bridge the VM onto a remote
    /// transport (ACP, A2A) translate this into a "paused / awaiting
    /// input" wire signal so the client knows the task isn't stuck —
    /// it's blocked on the human side. Pair-emitted with `HitlResolved`
    /// when the waitpoint completes/cancels/times out.
    HitlRequested {
        session_id: String,
        request_id: String,
        kind: String,
        payload: serde_json::Value,
    },
    /// Companion to `HitlRequested`: the waitpoint has resolved (either
    /// a response arrived, the deadline elapsed, or the request was
    /// cancelled). `outcome` is one of `"answered"`, `"timeout"`,
    /// `"cancelled"`. Hosts use this to flip task state back to
    /// `working` after an `input-required` pause.
    HitlResolved {
        session_id: String,
        request_id: String,
        kind: String,
        outcome: String,
    },
    /// Emitted by the agent loop's adaptive iteration budget /
    /// `loop_control` policy when a budget extension or early stop fires.
    /// Generic enough to cover both shapes — `action` distinguishes them.
    /// Carries the iteration the decision applied to, the previous /
    /// resulting iteration limit, the policy reason string, and (for
    /// stops) the loop status.
    LoopControlDecision {
        session_id: String,
        iteration: usize,
        action: String,
        old_limit: usize,
        new_limit: usize,
        reason: String,
        status: String,
    },
    /// Emitted when `agent_loop` detects adjacent repeated tool calls with
    /// identical arguments. The warning payload avoids raw arguments by
    /// default and carries digests so hosts can correlate repeats without
    /// exposing potentially sensitive tool inputs.
    AgentLoopStallWarning {
        session_id: String,
        warning: serde_json::Value,
    },
    /// Emitted when a concrete provider/model pair lacks a catalog
    /// recommendation for a capability and the runtime chooses a fallback.
    CapabilityGap {
        session_id: String,
        level: String,
        capability: String,
        provider: String,
        model: String,
        fallback_tool_format: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        requested_tool_format: Option<String>,
        message: String,
    },
    /// Emitted when a caller explicitly forces a tool format that
    /// differs from the capability catalog's recommendation or known
    /// native/text parity guidance.
    ToolFormatOverride {
        session_id: String,
        provider: String,
        model: String,
        requested_format: String,
        recommended_format: String,
        catalog_parity: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        override_reason: Option<String>,
    },
    /// Emitted when a `tool_caller` middleware (see std/llm/tool_middleware)
    /// attaches structured audit metadata to a tool call — typically a
    /// user-facing `summary`, a `description`, an ACP-style `kind`, an MCP
    /// `hints` block, a `consent` decision, the per-layer `layers` log, or
    /// free-form `metadata` keys (A2A-style extension slot).
    ///
    /// One-to-one with the underlying tool-call: hosts can join on
    /// `tool_call_id` to render middleware-attached chips alongside the
    /// existing `ToolCall` / `ToolCallUpdate` stream. The `audit` payload
    /// is intentionally free-form JSON so middleware can carry whatever
    /// shape the harness author chooses without needing protocol-level
    /// changes per new middleware. When present, `receipt` carries the
    /// stable typed, privacy-preserving record hosts can persist or mirror.
    ToolCallAudit {
        session_id: String,
        tool_call_id: String,
        tool_name: String,
        audit: serde_json::Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        receipt: Option<ToolCallReceipt>,
    },
    /// Emitted by `std/cache::with_cache` (both the generic and LLM
    /// forms) when a cached lookup returns a hit. Carries the
    /// content-addressed key, the backend that served the value, and a
    /// `metrics` block with the cost-moat receipts the persona value
    /// ledger (harn-cloud#58) and crystallization receipts read:
    /// `model_calls_avoided`, plus `tokens_saved` / `latency_saved_ms`
    /// when the cached envelope carried `usage` / `latency_ms`.
    CacheHit {
        session_id: String,
        key: String,
        backend: String,
        namespace: String,
        payload: serde_json::Value,
    },
    /// Paired with `CacheHit`. Emitted on the miss path when the
    /// fresh result is stored. `payload.metrics.compute_ms` carries
    /// the wall-clock cost of the underlying computation, which
    /// callers can feed back as `estimate.latency_saved_ms` on the
    /// next hit.
    CacheMiss {
        session_id: String,
        key: String,
        backend: String,
        namespace: String,
        payload: serde_json::Value,
    },
    /// A language-neutral tool-composition snippet has started. The envelope
    /// identifies the snippet and binding manifest hashes plus the side-effect
    /// ceiling requested for the whole parent run.
    CompositionStart {
        session_id: String,
        run: CompositionRunEnvelope,
    },
    /// A composition snippet is dispatching a child binding call. The child
    /// remains visible as its own operation with annotations and policy context
    /// instead of being hidden inside the parent composition blob.
    CompositionChildCall {
        session_id: String,
        call: CompositionChildCall,
    },
    /// A child binding operation emitted a status/result update.
    CompositionChildResult {
        session_id: String,
        result: CompositionChildResult,
    },
    /// A composition run finished successfully and carries stdout/stderr,
    /// artifacts, and the structured result in the terminal envelope.
    CompositionFinish {
        session_id: String,
        run: CompositionRunEnvelope,
    },
    /// A composition run failed before producing a successful terminal result.
    /// The terminal envelope carries the failure category and optional error.
    CompositionError {
        session_id: String,
        run: CompositionRunEnvelope,
    },
    /// Emitted once per `__agent_loop_checkpoint(...)` pass. The single
    /// named seam through which the agent loop drains queued bridge
    /// injections and inbox feedback. Hosts use it to debug "did the
    /// loop check for steering at the expected boundary" without having
    /// to grep the loop body for inline drain calls.
    ///
    /// `kind` is one of the documented seam names: `iteration_start`,
    /// `pre_tool_dispatch`, `post_tool_dispatch`, `iteration_end`,
    /// `pre_compact`, `post_compact`, `daemon_idle_pre`,
    /// `daemon_idle_post`, `loop_exit`. `delivered` is the count of
    /// bridge injections drained at this seam (inbox drains are
    /// reported separately under `inbox_delivered`). `dispatch_skipped`
    /// is true only when an `interrupt_immediate` injection arrived at
    /// `pre_tool_dispatch` and the pending tool batch was skipped.
    LoopCheckpoint {
        session_id: String,
        iteration: usize,
        kind: String,
        delivered: usize,
        #[serde(default, skip_serializing_if = "is_zero_usize")]
        inbox_delivered: usize,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        dispatch_skipped: bool,
    },
    /// Surfaced when Harn is acting as an MCP **client** and a peer
    /// server sends a server-to-client message during an agent session:
    /// a `notifications/progress` / `notifications/message` /
    /// `notifications/*/list_changed` notification, or an inbound
    /// `elicitation/create` / `sampling/createMessage` request.
    ///
    /// Emitted alongside (not in place of) the existing agent-inbox
    /// relay so a thin ACP client can render a live progress bar, log
    /// line, elicitation prompt, or sampling affordance without parsing
    /// the inbox transcript. `direction` is `"notification"` for
    /// fire-and-forget server notifications and `"request"` for inbound
    /// requests that still resolve through the existing client-role
    /// dispatch path (this event does not change that response). `method`
    /// is the raw MCP JSON-RPC method; `params` is its untouched payload.
    McpNotification {
        session_id: String,
        server: String,
        method: String,
        direction: String,
        params: serde_json::Value,
    },
    /// Surfaced when the effective MCP catalog changes — either because a
    /// server emitted a `notifications/tools/list_changed` (or the
    /// resource/prompt equivalents), or because the persisted enable/disable
    /// allowlist was edited. A thin ACP client (the burin-code TUI / GUI)
    /// treats this as a cue to re-fetch the catalog (e.g. via the
    /// `mcp/catalog` request) and re-render its toggle UI, rather than
    /// reconciling any local state. `server` is the server whose list
    /// changed, or `None` when the change is allowlist-wide. `reason` is a
    /// short tag (`"list_changed"` or `"allowlist_updated"`).
    McpCatalogChanged {
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        server: Option<String>,
        reason: String,
    },
    /// Surfaced when an MCP server harn is acting as a client for answers a
    /// request with `401 Unauthorized` mid-session, meaning its OAuth token is
    /// missing or expired. This is a cue for a thin ACP client (the burin-code
    /// TUI / GUI) to start an authorization: call `mcp/authorize` to mint a
    /// browser URL, open it, and forward the redirect's `code`+`state` back via
    /// `mcp/oauth_callback`. Token exchange and storage stay in harn. `server`
    /// is the configured server name; `resource` is its canonical RFC 8707
    /// resource indicator; `scope` is the `scope` parameter from the
    /// `WWW-Authenticate` challenge, when present.
    McpAuthRequired {
        session_id: String,
        server: String,
        resource: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scope: Option<String>,
    },
}

fn is_zero_usize(value: &usize) -> bool {
    *value == 0
}

impl AgentEvent {
    pub fn session_id(&self) -> &str {
        match self {
            Self::AgentMessageChunk { session_id, .. }
            | Self::AgentThoughtChunk { session_id, .. }
            | Self::UserMessage { session_id, .. }
            | Self::ToolCall { session_id, .. }
            | Self::ToolCallUpdate { session_id, .. }
            | Self::Plan { session_id, .. }
            | Self::ProgressReported { session_id, .. }
            | Self::CompassRoutingDecision { session_id, .. }
            | Self::AgentScratchpadReorganization { session_id, .. }
            | Self::Artifact { session_id, .. }
            | Self::IterationStart { session_id, .. }
            | Self::IterationEnd { session_id, .. }
            | Self::SessionClosed { session_id, .. }
            | Self::AnchorChanged { session_id, .. }
            | Self::JudgeDecision { session_id, .. }
            | Self::StepJudgeDecision { session_id, .. }
            | Self::StructuralValidatorDecision { session_id, .. }
            | Self::ScopeClassifierVerdict { session_id, .. }
            | Self::MissingToolCallVerdict { session_id, .. }
            | Self::TypedCheckpoint { session_id, .. }
            | Self::FeedbackInjected { session_id, .. }
            | Self::BudgetExhausted { session_id, .. }
            | Self::BudgetCircuitBreaker { session_id, .. }
            | Self::LoopStuck { session_id, .. }
            | Self::DaemonWatchdogTripped { session_id, .. }
            | Self::SkillActivated { session_id, .. }
            | Self::SkillDeactivated { session_id, .. }
            | Self::SkillScopeTools { session_id, .. }
            | Self::SkillNarrow { session_id, .. }
            | Self::ToolSearchQuery { session_id, .. }
            | Self::ToolSearchResult { session_id, .. }
            | Self::TranscriptCompacted { session_id, .. }
            | Self::TranscriptProjected { session_id, .. }
            | Self::ReminderEmitted { session_id, .. }
            | Self::Handoff { session_id, .. }
            | Self::FsWatch { session_id, .. }
            | Self::StagedWritesPending { session_id, .. }
            | Self::SafeTextPatchResult { session_id, .. }
            | Self::ControlOutcome { session_id, .. }
            | Self::WorkerUpdate { session_id, .. }
            | Self::HitlRequested { session_id, .. }
            | Self::HitlResolved { session_id, .. }
            | Self::LoopControlDecision { session_id, .. }
            | Self::AgentLoopStallWarning { session_id, .. }
            | Self::CapabilityGap { session_id, .. }
            | Self::ToolFormatOverride { session_id, .. }
            | Self::ToolCallAudit { session_id, .. }
            | Self::CacheHit { session_id, .. }
            | Self::CacheMiss { session_id, .. }
            | Self::CompositionStart { session_id, .. }
            | Self::CompositionChildCall { session_id, .. }
            | Self::CompositionChildResult { session_id, .. }
            | Self::CompositionFinish { session_id, .. }
            | Self::CompositionError { session_id, .. }
            | Self::LoopCheckpoint { session_id, .. }
            | Self::McpNotification { session_id, .. }
            | Self::McpCatalogChanged { session_id, .. }
            | Self::McpAuthRequired { session_id, .. } => session_id,
        }
    }
}
