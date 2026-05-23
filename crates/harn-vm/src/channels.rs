use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::format_description::well_known::Rfc3339;

use crate::event_log::{
    active_event_log, install_memory_for_current_thread, sanitize_topic_component, AnyEventLog,
    EventId, EventLog, LogEvent, Topic,
};
use crate::llm::vm_value_to_json;
use crate::runtime_limits::RuntimeLimits;
use crate::triggers::event::{ChannelEventPayload, KnownProviderPayload};
use crate::triggers::{ProviderId, ProviderPayload, SignatureStatus, TenantId, TriggerEvent};
use crate::value::{VmError, VmValue};

const CHANNEL_QUEUE_DEPTH: usize = RuntimeLimits::DEFAULT.default_event_log_queue_depth;
const CHANNEL_EVENT_KIND: &str = "channel.emit";
const IDEMPOTENCY_HEADER: &str = "harn.channel.id";
const NAME_HEADER: &str = "harn.channel.name";
const SCOPE_HEADER: &str = "harn.channel.scope";
const SCOPE_ID_HEADER: &str = "harn.channel.scope_id";
const EMITTED_BY_HEADER: &str = "harn.channel.emitted_by";

/// CH-06 (#1877): topic for `transcript.channel.emit` / `transcript.channel.match`
/// transcript events. Consumers subscribe to this topic to render channel
/// activity alongside reminder/suspension lifecycle events.
pub(crate) const CHANNEL_TRANSCRIPT_TOPIC: &str = "transcript.channel.lifecycle";
pub(crate) const CHANNEL_EMIT_TRANSCRIPT_KIND: &str = "transcript.channel.emit";
pub(crate) const CHANNEL_MATCH_TRANSCRIPT_KIND: &str = "transcript.channel.match";

/// CH-07 (#1878): durable audit topic for the replay-determinism receipts
/// emitted alongside every channel emit + every channel match. Consumers
/// drive replay/audit tooling off this topic instead of the lossy
/// `transcript.channel.lifecycle` summary topic — receipts carry the full
/// payload, signed timestamps, and cached match linkage so the
/// `replay_oracle` can byte-compare two runs of the same workload.
pub const CHANNEL_AUDIT_TOPIC: &str = "lifecycle.channel.audit";
pub(crate) const CHANNEL_EMIT_RECEIPT_KIND: &str = "channel_emit_receipt";
pub(crate) const CHANNEL_MATCH_RECEIPT_KIND: &str = "channel_match_receipt";
/// CH-07 (#1878): receipt schema header so downstream tooling can
/// version-gate parsers (mirrors `harn.pool_submit.v1` from PL-06 / #1891).
const CHANNEL_EMIT_RECEIPT_SCHEMA: &str = "harn.channel_emit_receipt.v1";
const CHANNEL_MATCH_RECEIPT_SCHEMA: &str = "harn.channel_match_receipt.v1";

/// CH-11 (#1911): event kinds + schema header for guardrail middleware
/// audit entries. Block + warn outcomes both write to the same
/// `lifecycle.channel.audit` topic so security review tooling reads a
/// single stream and discriminates on `kind`.
pub(crate) const CHANNEL_GUARDRAIL_BLOCKED_KIND: &str = "channel_guardrail_blocked";
pub(crate) const CHANNEL_GUARDRAIL_WARNING_KIND: &str = "channel_guardrail_warning";
const CHANNEL_GUARDRAIL_AUDIT_SCHEMA: &str = "harn.channel_guardrail_audit.v1";

/// CH-06 (#1877): per-event headers stamped on the `TriggerEvent` so the
/// channel-match dispatcher can link the `ChannelMatch` span back to the
/// originating `ChannelEmit` span across the async boundary (also through
/// the aggregation buffer for batched triggers).
const EMIT_TRACE_ID_HEADER: &str = "harn.channel.emit_trace_id";
const EMIT_SPAN_ID_HEADER: &str = "harn.channel.emit_span_id";

static SESSION_CHANNEL_LOG: OnceLock<Mutex<Option<Arc<AnyEventLog>>>> = OnceLock::new();
static SIGNING_SALT: OnceLock<Vec<u8>> = OnceLock::new();

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ChannelScope {
    Session,
    Pipeline,
    Tenant,
    Org,
}

impl ChannelScope {
    fn parse(value: &str) -> Result<Self, ChannelError> {
        match value.trim() {
            "session" => Ok(Self::Session),
            "pipeline" => Ok(Self::Pipeline),
            "tenant" => Ok(Self::Tenant),
            "org" => Ok(Self::Org),
            other => Err(ChannelError::malformed(format!(
                "HARN-CHN-003 malformed channel scope '{other}'"
            ))),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Pipeline => "pipeline",
            Self::Tenant => "tenant",
            Self::Org => "org",
        }
    }
}

#[derive(Clone, Debug, Default)]
struct ChannelContext {
    task_id: Option<String>,
    root_task_id: Option<String>,
    scope_id: Option<String>,
    workflow_id: Option<String>,
    run_id: Option<String>,
    worker_id: Option<String>,
    agent_session_id: Option<String>,
    root_agent_session_id: Option<String>,
    tenant_id: Option<String>,
}

#[derive(Clone, Debug, Default)]
struct ChannelOptions {
    scope: Option<ChannelScope>,
    id: Option<String>,
    tenant_id: Option<String>,
    session_id: Option<String>,
    pipeline_id: Option<String>,
    from_cursor: Option<EventId>,
    limit: Option<usize>,
    ttl_ms: Option<i64>,
}

#[derive(Clone, Debug)]
struct ResolvedChannel {
    scope: ChannelScope,
    scope_id: String,
    resolved_name: String,
    topic: Topic,
    retention: &'static str,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SignedTimestamp {
    pub at_ms: i64,
    pub at: String,
    pub algorithm: String,
    pub key_id: String,
    pub signature: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredChannelEvent {
    id: String,
    name: String,
    payload: serde_json::Value,
    emitted_at: SignedTimestamp,
    emitted_by: String,
    scope: String,
    scope_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pipeline_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tenant_id: Option<String>,
    retention: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    ttl_ms: Option<i64>,
}

/// CH-07 (#1878): durable replay-determinism receipt for a single
/// `emit_channel(...)` call. Pairs 1:1 with the `ChannelEmit` span (CH-06
/// / #1877) and with the durable journal append. Persisted to the
/// `lifecycle.channel.audit` event-log topic so the `replay_oracle` can
/// reproduce the entire emit chain across two runs of the same workload.
///
/// `payload_hash` lets the oracle detect producer-side drift
/// (`HARN-REP-CHN-002`) without having to canonicalize the full payload
/// during comparison; `payload` carries the verbatim value for byte
/// equality + replay reconstruction.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ChannelEmitReceipt {
    pub event_id: String,
    pub name_resolved: String,
    pub scope: String,
    pub scope_id: String,
    pub payload_hash: String,
    pub payload: serde_json::Value,
    pub emitted_at: SignedTimestamp,
    pub emitted_by: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pipeline_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
    pub topic: String,
    pub inserted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span_id: Option<u64>,
}

/// CH-07 (#1878): durable replay-determinism receipt for a single channel
/// match — one per `(binding, event)` pair the dispatcher fires. For
/// aggregation/batched triggers (CH-04 / #1875) the receipt carries
/// `batch.constituent_event_ids` so the replay oracle can verify the
/// full batch composition matches across runs (`HARN-REP-CHN-003`).
///
/// `event_id` doubles as the cached-match key: on replay the dispatcher
/// looks up the recorded match by `event_id` instead of re-evaluating
/// the filter spec, preserving "the journal IS the spec" determinism.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ChannelMatchReceipt {
    pub event_id: String,
    pub trigger_id: String,
    pub binding_key: String,
    pub name_resolved: String,
    pub scope: String,
    pub scope_id: String,
    pub matched_at: SignedTimestamp,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_in_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub batch: Option<ChannelMatchBatchInfo>,
    pub handler_kind: String,
    pub handler_result: ChannelMatchResultSummary,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span_id: Option<u64>,
}

/// CH-07 (#1878): batched-dispatch summary stamped onto
/// `ChannelMatchReceipt`. The `constituent_event_ids` list is the full
/// recorded composition; replay reconstructs the batch from those ids.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ChannelMatchBatchInfo {
    pub count: usize,
    pub constituent_event_ids: Vec<String>,
}

/// CH-07 (#1878): summary of the handler invocation outcome. Mirrors the
/// shape of the dispatcher's `DispatchOutcome` but kept lightweight so
/// the receipt stays compact and self-contained — replay tooling never
/// needs to cross-reference the dispatcher log to interpret a match.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ChannelMatchResultSummary {
    pub status: String,
    pub attempt_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dispatch_failed: Option<bool>,
}

impl ChannelMatchResultSummary {
    fn from_dispatch(
        outcome: &Result<crate::triggers::DispatchOutcome, crate::triggers::DispatchError>,
    ) -> Self {
        match outcome {
            Ok(outcome) => Self {
                status: outcome.status.as_str().to_string(),
                attempt_count: outcome.attempt_count,
                error: outcome.error.clone(),
                dispatch_failed: None,
            },
            Err(error) => Self {
                status: "dispatch_error".to_string(),
                attempt_count: 0,
                error: Some(error.to_string()),
                dispatch_failed: Some(true),
            },
        }
    }
}

#[derive(Debug)]
struct ChannelError(String);

impl ChannelError {
    fn missing_pipeline() -> Self {
        Self("HARN-CHN-001 missing pipeline context for pipeline-scoped channel".to_string())
    }

    fn cross_tenant(message: impl Into<String>) -> Self {
        Self(format!("HARN-CHN-002 {}", message.into()))
    }

    fn malformed(message: impl Into<String>) -> Self {
        Self(message.into())
    }

    fn scope_ambiguous(message: impl Into<String>) -> Self {
        Self(format!("HARN-CHN-004 {}", message.into()))
    }
}

impl From<ChannelError> for VmError {
    fn from(error: ChannelError) -> Self {
        VmError::Runtime(error.0)
    }
}

pub fn reset_channel_state() {
    if let Some(slot) = SESSION_CHANNEL_LOG.get() {
        *slot.lock().expect("channel session log poisoned") = None;
    }
    // CH-11 (#1911): clear the per-thread guardrail registry so the
    // next pipeline starts with a clean slate. Mirrors how
    // SESSION_CHANNEL_LOG is reset above.
    crate::channel_guardrails::clear();
}

pub(crate) async fn emit_channel_from_vm(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let name = required_string(args.first(), "emit_channel", "name")?;
    let payload = vm_value_to_json(
        args.get(1)
            .ok_or_else(|| VmError::TypeError("emit_channel: missing payload".to_string()))?,
    );
    let options = parse_options(args.get(2), "emit_channel")?;
    let context = ChannelContext::current();
    let resolved = resolve_channel(&name, &options, &context)?;
    let event_id = options
        .id
        .clone()
        .unwrap_or_else(|| format!("channel_evt_{}", uuid::Uuid::now_v7()));
    let emitted_by = emitted_by(&context);
    let emitted_at = signed_timestamp(&resolved, &event_id, &emitted_by);
    let occurred_at_ms = emitted_at.at_ms;

    // CH-11 (#1911): channel guardrails middleware. Runs BEFORE the
    // durable journal append so a blocked payload never gets persisted
    // (the block itself IS persisted on the lifecycle.channel.audit
    // topic so the audit trail is durable). Warn verdicts proceed but
    // record an audit. The aggregate decision is the worst verdict
    // across every registered guardrail.
    let guardrail_context = serde_json::json!({
        "name": name,
        "name_resolved": resolved.resolved_name,
        "scope": resolved.scope.as_str(),
        "scope_id": resolved.scope_id,
        "event_id": event_id,
        "emitted_by": emitted_by,
    });
    let decision =
        crate::channel_guardrails::evaluate(&payload, &guardrail_context, &resolved.resolved_name)
            .await?;
    if matches!(
        decision.verdict,
        crate::channel_guardrails::Verdict::Block { .. }
    ) {
        return handle_blocked_emit(
            &name,
            &resolved,
            &event_id,
            &emitted_by,
            &emitted_at,
            &payload,
            &decision,
        )
        .await;
    }
    record_guardrail_warnings(
        &resolved,
        &event_id,
        &emitted_by,
        &payload,
        decision.fired.as_slice(),
    )
    .await;
    let record = StoredChannelEvent {
        id: event_id.clone(),
        name: resolved.resolved_name.clone(),
        payload,
        emitted_at,
        emitted_by: emitted_by.clone(),
        scope: resolved.scope.as_str().to_string(),
        scope_id: resolved.scope_id.clone(),
        pipeline_id: context.pipeline_id_for_receipt(&resolved),
        session_id: context.session_id_for_receipt(&resolved),
        tenant_id: context.tenant_id_for_receipt(&resolved),
        retention: resolved.retention.to_string(),
        ttl_ms: options.ttl_ms,
    };

    // CH-06 (#1877): open the ChannelEmit span around the durable append +
    // trigger fan-out. The span captures emit-time metadata (scope, name,
    // payload summary) and its trace/span id is stashed on the trigger
    // event headers so the downstream ChannelMatch span can link back.
    let mut emit_span = ChannelSpanGuard::start(
        crate::tracing::SpanKind::ChannelEmit,
        format!("channel.emit {}", resolved.resolved_name),
        Vec::new(),
    );
    emit_span.set_metadata("event_id", serde_json::json!(record.id));
    emit_span.set_metadata("scope", serde_json::json!(resolved.scope.as_str()));
    emit_span.set_metadata("scope_id", serde_json::json!(resolved.scope_id));
    emit_span.set_metadata("name_resolved", serde_json::json!(resolved.resolved_name));
    emit_span.set_metadata("payload_summary", summarize_payload(&record.payload));
    let emit_span_id = crate::tracing::current_span_id().unwrap_or(0);
    let emit_link = emit_span.link();

    let mut headers = BTreeMap::new();
    headers.insert(IDEMPOTENCY_HEADER.to_string(), event_id.clone());
    headers.insert(NAME_HEADER.to_string(), resolved.resolved_name.clone());
    headers.insert(
        SCOPE_HEADER.to_string(),
        resolved.scope.as_str().to_string(),
    );
    headers.insert(SCOPE_ID_HEADER.to_string(), resolved.scope_id.clone());
    headers.insert(EMITTED_BY_HEADER.to_string(), emitted_by.clone());

    let log = log_for_scope(resolved.scope);
    let mut log_event = LogEvent::new(
        CHANNEL_EVENT_KIND,
        serde_json::to_value(&record)
            .map_err(|error| VmError::Runtime(format!("emit_channel: encode event: {error}")))?,
    )
    .with_headers(headers);
    log_event.occurred_at_ms = occurred_at_ms;
    let outcome = log
        .append_idempotent_by_header(&resolved.topic, IDEMPOTENCY_HEADER, &event_id, log_event)
        .await
        .map_err(channel_log_error)?;
    let receipt = receipt_value(
        &resolved.topic,
        outcome.event_id,
        &outcome.event,
        outcome.inserted,
    )?;
    // CH-06 (#1877): emit the transcript event whether the append was fresh
    // or idempotent — both outcomes are first-class observability signals.
    emit_channel_emit_transcript(&record, &resolved, outcome.inserted, emit_span_id);
    // CH-07 (#1878): persist the durable emit receipt on the
    // `lifecycle.channel.audit` topic so the replay oracle has the full
    // emit chain (payload hash, signed timestamp, scope correlation)
    // independently of the lossy transcript summary topic.
    record_channel_emit_receipt(&record, &resolved, outcome.inserted, emit_span_id).await;
    // CH-02 (#1872): fan out the emit to channel-source triggers. Only fresh
    // appends fan out; idempotent duplicates short-circuit because the producer
    // already saw the original delivery.
    if outcome.inserted {
        let payload_json = outcome
            .event
            .payload
            .get("payload")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let context_for_fanout = ChannelContext::current();
        let fanout_payload = ChannelEventPayload {
            id: event_id.clone(),
            name: parse_name(&name)
                .map(|parsed| parsed.name)
                .unwrap_or_else(|_| resolved.resolved_name.clone()),
            name_resolved: resolved.resolved_name.clone(),
            scope: resolved.scope.as_str().to_string(),
            scope_id: resolved.scope_id.clone(),
            payload: payload_json,
            emitted_by: emitted_by.clone(),
            tenant_id: context_for_fanout.tenant_id_for_receipt(&resolved),
            session_id: context_for_fanout.session_id_for_receipt(&resolved),
            pipeline_id: context_for_fanout.pipeline_id_for_receipt(&resolved),
        };
        dispatch_channel_emit_to_triggers(&resolved, fanout_payload, emit_link).await?;
    }
    emit_span.end();
    Ok(crate::stdlib::json_to_vm_value(&receipt))
}

pub(crate) async fn channel_events_from_vm(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let name = required_string(args.first(), "channel_events", "name")?;
    let options = parse_options(args.get(1), "channel_events")?;
    let context = ChannelContext::current();
    let resolved = resolve_channel(&name, &options, &context)?;
    let events = log_for_scope(resolved.scope)
        .read_range(
            &resolved.topic,
            options.from_cursor,
            options.limit.unwrap_or(usize::MAX),
        )
        .await
        .map_err(channel_log_error)?;
    let values = events
        .into_iter()
        .map(|(event_id, event)| event_value(&resolved.topic, event_id, event))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(crate::stdlib::json_to_vm_value(&serde_json::Value::Array(
        values,
    )))
}

impl ChannelContext {
    fn current() -> Self {
        let mut context = Self::default();
        if let Some(vm) = crate::vm::clone_async_builtin_child_vm() {
            context.task_id = Some(vm.runtime_context.task_id.clone());
            context.root_task_id = Some(vm.runtime_context.root_task_id.clone());
            context.scope_id = vm.runtime_context.scope_id.clone();
            if let VmValue::Dict(values) = crate::runtime_context::runtime_context_value(&vm) {
                context.workflow_id = dict_string(&values, "workflow_id");
                context.run_id = dict_string(&values, "run_id");
                context.worker_id = dict_string(&values, "worker_id");
                context.agent_session_id = dict_string(&values, "agent_session_id");
                context.root_agent_session_id = dict_string(&values, "root_agent_session_id");
                context.tenant_id = dict_string(&values, "tenant_id");
            }
        }
        context.agent_session_id = context
            .agent_session_id
            .or_else(crate::agent_sessions::current_session_id);
        context
    }

    fn session_id(&self, options: &ChannelOptions) -> Result<String, ChannelError> {
        // CH-03 (#1874): an explicit `options.session_id` that disagrees with the
        // active session is HARN-CHN-004 ambiguity, not a silent override. The
        // resolver MUST be deterministic for a given runtime context.
        if let Some(requested) = options.session_id.as_deref() {
            if let Some(active) = self.agent_session_id.as_deref() {
                if active != requested {
                    return Err(ChannelError::scope_ambiguous(format!(
                        "session scope ambiguous: options.session_id '{requested}' \
                         conflicts with active session '{active}'"
                    )));
                }
            }
        }
        Ok(options
            .session_id
            .clone()
            .or_else(|| self.agent_session_id.clone())
            .or_else(|| self.root_agent_session_id.clone())
            .or_else(|| self.scope_id.clone())
            .or_else(|| self.root_task_id.clone())
            .unwrap_or_else(|| "session".to_string()))
    }

    fn pipeline_id(&self, options: &ChannelOptions) -> Result<String, ChannelError> {
        // CH-03 (#1874): explicit `options.pipeline_id` that conflicts with the
        // active workflow/run is HARN-CHN-004. Two pipelines cannot share a
        // resolved channel without an explicit disambiguation.
        let active = self.workflow_id.clone().or_else(|| self.run_id.clone());
        if let (Some(requested), Some(active)) = (options.pipeline_id.as_deref(), active.as_deref())
        {
            if requested != active {
                return Err(ChannelError::scope_ambiguous(format!(
                    "pipeline scope ambiguous: options.pipeline_id '{requested}' \
                     conflicts with active pipeline '{active}'"
                )));
            }
        }
        options
            .pipeline_id
            .clone()
            .or(active)
            .ok_or_else(ChannelError::missing_pipeline)
    }

    fn tenant_id(
        &self,
        options: &ChannelOptions,
        requested: Option<&str>,
    ) -> Result<String, ChannelError> {
        let current = self.tenant_id.as_deref();
        let requested = requested
            .map(ToOwned::to_owned)
            .or_else(|| options.tenant_id.clone());
        if let (Some(current), Some(requested)) = (current, requested.as_deref()) {
            if current != requested {
                return Err(ChannelError::cross_tenant(format!(
                    "cross-tenant channel emit requires a grant: current tenant '{current}', requested tenant '{requested}'"
                )));
            }
        }
        Ok(requested
            .or_else(|| self.tenant_id.clone())
            .unwrap_or_else(|| "default".to_string()))
    }

    fn pipeline_id_for_receipt(&self, resolved: &ResolvedChannel) -> Option<String> {
        match resolved.scope {
            ChannelScope::Pipeline => Some(resolved.scope_id.clone()),
            _ => self.workflow_id.clone().or_else(|| self.run_id.clone()),
        }
    }

    fn session_id_for_receipt(&self, resolved: &ResolvedChannel) -> Option<String> {
        match resolved.scope {
            ChannelScope::Session => Some(resolved.scope_id.clone()),
            _ => self
                .agent_session_id
                .clone()
                .or_else(|| self.root_agent_session_id.clone()),
        }
    }

    fn tenant_id_for_receipt(&self, resolved: &ResolvedChannel) -> Option<String> {
        match resolved.scope {
            ChannelScope::Tenant => Some(resolved.scope_id.clone()),
            _ => self.tenant_id.clone(),
        }
    }
}

fn resolve_channel(
    raw_name: &str,
    options: &ChannelOptions,
    context: &ChannelContext,
) -> Result<ResolvedChannel, ChannelError> {
    let parsed = parse_name(raw_name)?;
    if let Some(option_scope) = options.scope {
        if let Some(prefix_scope) = parsed.scope {
            if prefix_scope != option_scope {
                return Err(ChannelError::malformed(format!(
                    "HARN-CHN-003 channel scope prefix '{}' conflicts with options.scope '{}'",
                    prefix_scope.as_str(),
                    option_scope.as_str()
                )));
            }
        }
    }

    let scope = parsed
        .scope
        .or(options.scope)
        .unwrap_or(ChannelScope::Tenant);
    if scope == ChannelScope::Org {
        return Err(ChannelError::cross_tenant(
            "org-scoped channels are disabled until org grants are available",
        ));
    }

    validate_channel_name(&parsed.name)?;
    let scope_id = match scope {
        ChannelScope::Session => match parsed.scope_id.clone() {
            Some(id) => id,
            None => context.session_id(options)?,
        },
        ChannelScope::Pipeline => context.pipeline_id(options)?,
        ChannelScope::Tenant => context.tenant_id(options, parsed.scope_id.as_deref())?,
        ChannelScope::Org => unreachable!("org scope returned above"),
    };
    validate_scope_id(scope, &scope_id)?;
    let resolved_name = format!("{}:{}:{}", scope.as_str(), scope_id, parsed.name);
    let topic = Topic::new(format!(
        "channels.{}.{}.{}",
        scope.as_str(),
        sanitize_topic_component(&scope_id),
        sanitize_topic_component(&parsed.name)
    ))
    .map_err(|error| ChannelError::malformed(format!("HARN-CHN-003 {error}")))?;
    Ok(ResolvedChannel {
        scope,
        scope_id,
        resolved_name,
        topic,
        retention: retention_for_scope(scope),
    })
}

#[derive(Clone, Debug)]
struct ParsedName {
    scope: Option<ChannelScope>,
    scope_id: Option<String>,
    name: String,
}

fn parse_name(raw_name: &str) -> Result<ParsedName, ChannelError> {
    let raw_name = raw_name.trim();
    if raw_name.is_empty() {
        return Err(ChannelError::malformed(
            "HARN-CHN-003 channel name cannot be empty",
        ));
    }
    let Some((prefix, rest)) = raw_name.split_once(':') else {
        return Ok(ParsedName {
            scope: None,
            scope_id: None,
            name: raw_name.to_string(),
        });
    };
    let scope = ChannelScope::parse(prefix)?;
    match scope {
        ChannelScope::Session | ChannelScope::Pipeline => {
            if rest.is_empty() || rest.contains(':') {
                return Err(ChannelError::malformed(format!(
                    "HARN-CHN-003 malformed {} channel name '{raw_name}'",
                    scope.as_str()
                )));
            }
            Ok(ParsedName {
                scope: Some(scope),
                scope_id: None,
                name: rest.to_string(),
            })
        }
        ChannelScope::Tenant => {
            if rest.is_empty() {
                return Err(ChannelError::malformed(
                    "HARN-CHN-003 tenant channel name cannot be empty",
                ));
            }
            let (scope_id, name) = match rest.split_once(':') {
                Some((tenant_id, name)) if !tenant_id.is_empty() && !name.is_empty() => {
                    (Some(tenant_id.to_string()), name.to_string())
                }
                Some(_) => {
                    return Err(ChannelError::malformed(format!(
                        "HARN-CHN-003 malformed tenant channel name '{raw_name}'"
                    )))
                }
                None => (None, rest.to_string()),
            };
            Ok(ParsedName {
                scope: Some(scope),
                scope_id,
                name,
            })
        }
        ChannelScope::Org => {
            let Some((org_id, name)) = rest.split_once(':') else {
                return Err(ChannelError::malformed(format!(
                    "HARN-CHN-003 org channel names must be org:<org_id>:<name>, got '{raw_name}'"
                )));
            };
            if org_id.is_empty() || name.is_empty() {
                return Err(ChannelError::malformed(format!(
                    "HARN-CHN-003 malformed org channel name '{raw_name}'"
                )));
            }
            Ok(ParsedName {
                scope: Some(scope),
                scope_id: Some(org_id.to_string()),
                name: name.to_string(),
            })
        }
    }
}

fn validate_channel_name(name: &str) -> Result<(), ChannelError> {
    if name.trim().is_empty()
        || name.contains(':')
        || name.chars().any(|ch| ch.is_control() || ch.is_whitespace())
    {
        return Err(ChannelError::malformed(format!(
            "HARN-CHN-003 malformed channel name '{name}'"
        )));
    }
    Ok(())
}

fn validate_scope_id(scope: ChannelScope, scope_id: &str) -> Result<(), ChannelError> {
    if scope_id.trim().is_empty()
        || scope_id
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace() || ch == ':')
    {
        return Err(ChannelError::malformed(format!(
            "HARN-CHN-003 malformed {} scope id '{scope_id}'",
            scope.as_str()
        )));
    }
    Ok(())
}

fn log_for_scope(scope: ChannelScope) -> Arc<AnyEventLog> {
    match scope {
        ChannelScope::Session => {
            let slot = SESSION_CHANNEL_LOG.get_or_init(|| Mutex::new(None));
            let mut guard = slot.lock().expect("channel session log poisoned");
            guard
                .get_or_insert_with(|| {
                    Arc::new(AnyEventLog::Memory(crate::event_log::MemoryEventLog::new(
                        CHANNEL_QUEUE_DEPTH,
                    )))
                })
                .clone()
        }
        ChannelScope::Pipeline | ChannelScope::Tenant => active_event_log()
            .unwrap_or_else(|| install_memory_for_current_thread(CHANNEL_QUEUE_DEPTH)),
        ChannelScope::Org => unreachable!("org-scoped channel log is disabled"),
    }
}

fn signed_timestamp(
    resolved: &ResolvedChannel,
    event_id: &str,
    emitted_by: &str,
) -> SignedTimestamp {
    let at = crate::clock_mock::now_utc();
    let at_ms = (at.unix_timestamp_nanos() / 1_000_000) as i64;
    let at_text = at.format(&Rfc3339).unwrap_or_else(|_| at.to_string());
    let material = format!(
        "harn.channel.timestamp.v1\nat_ms={at_ms}\nid={event_id}\nname={}\nscope={}\nscope_id={}\nemitted_by={emitted_by}\n",
        resolved.resolved_name,
        resolved.scope.as_str(),
        resolved.scope_id
    );
    let signature = hex::encode(crate::connectors::hmac::hmac_sha256(
        signing_salt(),
        material.as_bytes(),
    ));
    SignedTimestamp {
        at_ms,
        at: at_text,
        algorithm: "hmac-sha256".to_string(),
        key_id: "local-session".to_string(),
        signature: format!("sha256:{signature}"),
    }
}

fn signing_salt() -> &'static [u8] {
    SIGNING_SALT
        .get_or_init(|| {
            format!(
                "harn-channel-signing-salt:{}:{}",
                std::process::id(),
                uuid::Uuid::now_v7()
            )
            .into_bytes()
        })
        .as_slice()
}

/// CH-07 (#1878): signed timestamp for a channel match. Mirrors the emit
/// timestamp algorithm in `signed_timestamp` so the replay oracle can
/// verify match timestamps with the same `signing_salt()` key material.
/// Including `event_id` + `trigger_id` in the signing material binds the
/// timestamp to this specific (emit, binding) pair so a replayed match
/// can't be re-stamped onto a different binding.
fn signed_match_timestamp(
    resolved: &ResolvedChannel,
    event_id: &str,
    trigger_id: &str,
) -> SignedTimestamp {
    let at = crate::clock_mock::now_utc();
    let at_ms = (at.unix_timestamp_nanos() / 1_000_000) as i64;
    let at_text = at.format(&Rfc3339).unwrap_or_else(|_| at.to_string());
    let material = format!(
        "harn.channel.match_timestamp.v1\nat_ms={at_ms}\nevent_id={event_id}\ntrigger_id={trigger_id}\nname={}\nscope={}\nscope_id={}\n",
        resolved.resolved_name,
        resolved.scope.as_str(),
        resolved.scope_id
    );
    let signature = hex::encode(crate::connectors::hmac::hmac_sha256(
        signing_salt(),
        material.as_bytes(),
    ));
    SignedTimestamp {
        at_ms,
        at: at_text,
        algorithm: "hmac-sha256".to_string(),
        key_id: "local-session".to_string(),
        signature: format!("sha256:{signature}"),
    }
}

/// CH-07 (#1878): SHA-256 of the canonical JSON encoding of a channel
/// emit payload. `serde_json::to_string` sorts object keys
/// deterministically when the value originates from `serde_json::Value`
/// (BTreeMap-backed `Map` with `preserve_order` disabled — the default
/// for this crate). Used by the replay oracle to detect producer-side
/// drift (`HARN-REP-CHN-002`).
pub fn channel_payload_hash(payload: &serde_json::Value) -> String {
    let canonical = canonical_json_string(payload);
    let digest = Sha256::digest(canonical.as_bytes());
    format!("sha256:{}", hex::encode(digest))
}

/// CH-07 (#1878): canonicalize a JSON value to a deterministic string so
/// the SHA-256 hash on `ChannelEmitReceipt.payload_hash` is stable
/// across reruns. Recursively sorts object keys; arrays preserve their
/// (semantically meaningful) order. Mirrors the canonicalization rule
/// `replay_oracle::canonicalize_run` relies on for cross-run diffs.
fn canonical_json_string(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Object(map) => {
            let mut sorted: std::collections::BTreeMap<&String, &serde_json::Value> =
                std::collections::BTreeMap::new();
            for (key, value) in map {
                sorted.insert(key, value);
            }
            let parts: Vec<String> = sorted
                .iter()
                .map(|(key, value)| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(key).unwrap_or_else(|_| key.to_string()),
                        canonical_json_string(value)
                    )
                })
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        serde_json::Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(canonical_json_string).collect();
            format!("[{}]", parts.join(","))
        }
        other => serde_json::to_string(other).unwrap_or_else(|_| "null".to_string()),
    }
}

/// CH-07 (#1878): append a channel audit receipt to the durable
/// `lifecycle.channel.audit` topic. Mirrors `emit_pool_*_receipt`
/// (PL-06 / #1891). The audit log uses `active_event_log()` so receipts
/// inherit the pipeline's durability (in-memory in tests; durable
/// SQLite/etc. in production). Best-effort: receipt append errors do not
/// fail the emit/match — the emit's user-visible receipt is the source
/// of truth for caller behavior. Audit consumers learn about gaps via
/// the canonical event-log read API.
async fn append_channel_audit_event(
    kind: &'static str,
    schema: &'static str,
    payload: serde_json::Value,
) {
    let topic = match Topic::new(CHANNEL_AUDIT_TOPIC) {
        Ok(topic) => topic,
        Err(_) => return,
    };
    let log = active_event_log()
        .unwrap_or_else(|| install_memory_for_current_thread(CHANNEL_QUEUE_DEPTH));
    let mut headers = BTreeMap::new();
    headers.insert("schema".to_string(), schema.to_string());
    let _ = log
        .append(&topic, LogEvent::new(kind, payload).with_headers(headers))
        .await;
}

/// CH-11 (#1911): emit a guardrail-blocked / -warned audit entry to
/// the durable channel-audit topic AND to the in-process lifecycle
/// audit log. The lifecycle entry is what `pipeline_lifecycle_audit_log_take()`
/// surfaces to test fixtures; the event-log entry is what production
/// audit consumers tail. Both echo the verbatim payload (callers
/// concerned about PII should layer `redact::*` on top of the
/// guardrail). Best-effort; audit write errors do not propagate.
async fn record_guardrail_audit(
    kind: &'static str,
    resolved: &ResolvedChannel,
    event_id: &str,
    emitted_by: &str,
    payload: &serde_json::Value,
    fired: &[crate::channel_guardrails::FiredGuardrail],
) {
    let fired_json: Vec<serde_json::Value> = fired
        .iter()
        .map(|entry| {
            serde_json::json!({
                "id": entry.id,
                "kind": entry.kind,
                "verdict_label": entry.verdict_label,
                "reason": entry.reason,
            })
        })
        .collect();
    let audit_payload = serde_json::json!({
        "event_id": event_id,
        "name_resolved": resolved.resolved_name,
        "scope": resolved.scope.as_str(),
        "scope_id": resolved.scope_id,
        "emitted_by": emitted_by,
        "payload_hash": channel_payload_hash(payload),
        "payload": payload,
        "fired": fired_json,
    });
    append_channel_audit_event(kind, CHANNEL_GUARDRAIL_AUDIT_SCHEMA, audit_payload.clone()).await;
    // Mirror to the lifecycle audit log so tests using
    // `pipeline_lifecycle_audit_log_take()` see the entry without
    // having to scan the event log directly.
    crate::orchestration::record_lifecycle_audit(kind, audit_payload);
}

/// CH-11 (#1911): record a warning audit for every Warn verdict that
/// fired. A no-op when there were no Warn-level fires (the common
/// case). Called BEFORE the durable append so the warning order
/// matches the dispatch order.
async fn record_guardrail_warnings(
    resolved: &ResolvedChannel,
    event_id: &str,
    emitted_by: &str,
    payload: &serde_json::Value,
    fired: &[crate::channel_guardrails::FiredGuardrail],
) {
    if fired.is_empty() {
        return;
    }
    record_guardrail_audit(
        CHANNEL_GUARDRAIL_WARNING_KIND,
        resolved,
        event_id,
        emitted_by,
        payload,
        fired,
    )
    .await;
}

/// CH-11 (#1911): record a `channel_guardrail_blocked` audit and
/// return the synthetic "blocked" receipt so the caller can
/// distinguish a guardrail block from a successful append or an
/// idempotent dedupe. No durable journal append happens for a blocked
/// emit; the audit log entry IS the durable artifact.
async fn handle_blocked_emit(
    raw_name: &str,
    resolved: &ResolvedChannel,
    event_id: &str,
    emitted_by: &str,
    emitted_at: &SignedTimestamp,
    payload: &serde_json::Value,
    decision: &crate::channel_guardrails::GuardrailDecision,
) -> Result<VmValue, VmError> {
    record_guardrail_audit(
        CHANNEL_GUARDRAIL_BLOCKED_KIND,
        resolved,
        event_id,
        emitted_by,
        payload,
        decision.fired.as_slice(),
    )
    .await;
    let block_reason = decision
        .fired
        .iter()
        .rev()
        .find_map(|f| {
            if f.verdict_label == CHANNEL_GUARDRAIL_BLOCKED_KIND
                || f.verdict_label.contains("block")
            {
                Some(f.reason.clone())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "guardrail blocked".to_string());
    let fired_json: Vec<serde_json::Value> = decision
        .fired
        .iter()
        .map(|entry| {
            serde_json::json!({
                "id": entry.id,
                "kind": entry.kind,
                "verdict_label": entry.verdict_label,
                "reason": entry.reason,
            })
        })
        .collect();
    let receipt = serde_json::json!({
        "event_id": event_id,
        "cursor": serde_json::Value::Null,
        "id": event_id,
        "name": raw_name,
        "name_resolved": resolved.resolved_name,
        "scope": resolved.scope.as_str(),
        "scope_id": resolved.scope_id,
        "emitted_at": emitted_at,
        "emitted_by": emitted_by,
        "retention": resolved.retention,
        "topic": resolved.topic.as_str(),
        "inserted": false,
        "duplicate": false,
        "blocked": true,
        "block_reason": block_reason,
        "guardrail_fired": fired_json,
    });
    Ok(crate::stdlib::json_to_vm_value(&receipt))
}

/// CH-07 (#1878): build + persist the emit receipt on the audit topic.
/// Called from `emit_channel_from_vm` immediately after the durable
/// journal append succeeds (whether the append was fresh or
/// idempotent-suppressed — both outcomes are first-class audit signals).
async fn record_channel_emit_receipt(
    record: &StoredChannelEvent,
    resolved: &ResolvedChannel,
    inserted: bool,
    span_id: u64,
) {
    let receipt = ChannelEmitReceipt {
        event_id: record.id.clone(),
        name_resolved: resolved.resolved_name.clone(),
        scope: resolved.scope.as_str().to_string(),
        scope_id: resolved.scope_id.clone(),
        payload_hash: channel_payload_hash(&record.payload),
        payload: record.payload.clone(),
        emitted_at: record.emitted_at.clone(),
        emitted_by: record.emitted_by.clone(),
        pipeline_id: record.pipeline_id.clone(),
        session_id: record.session_id.clone(),
        tenant_id: record.tenant_id.clone(),
        topic: resolved.topic.as_str().to_string(),
        inserted,
        span_id: if span_id == 0 { None } else { Some(span_id) },
    };
    let payload = match serde_json::to_value(&receipt) {
        Ok(value) => value,
        Err(_) => return,
    };
    append_channel_audit_event(
        CHANNEL_EMIT_RECEIPT_KIND,
        CHANNEL_EMIT_RECEIPT_SCHEMA,
        payload,
    )
    .await;
}

/// CH-07 (#1878): build + persist the match receipt on the audit topic.
/// Called from `fire_channel_match` after the dispatcher returns, so
/// `handler_result` reflects the recorded outcome (succeeded / failed /
/// dlq / cancelled / ...) and replay tooling can verify the same handler
/// shape fires on every replay.
#[allow(clippy::too_many_arguments)]
async fn record_channel_match_receipt(
    trigger_id: &str,
    binding_key: &str,
    handler_kind: &str,
    resolved: &ResolvedChannel,
    event_id: &str,
    matched_in_session_id: Option<&str>,
    batch: Option<ChannelMatchBatchInfo>,
    span_id: u64,
    dispatch_outcome: &Result<crate::triggers::DispatchOutcome, crate::triggers::DispatchError>,
) {
    let receipt = ChannelMatchReceipt {
        event_id: event_id.to_string(),
        trigger_id: trigger_id.to_string(),
        binding_key: binding_key.to_string(),
        name_resolved: resolved.resolved_name.clone(),
        scope: resolved.scope.as_str().to_string(),
        scope_id: resolved.scope_id.clone(),
        matched_at: signed_match_timestamp(resolved, event_id, trigger_id),
        matched_in_session_id: matched_in_session_id.map(|s| s.to_string()),
        batch,
        handler_kind: handler_kind.to_string(),
        handler_result: ChannelMatchResultSummary::from_dispatch(dispatch_outcome),
        span_id: if span_id == 0 { None } else { Some(span_id) },
    };
    let payload = match serde_json::to_value(&receipt) {
        Ok(value) => value,
        Err(_) => return,
    };
    append_channel_audit_event(
        CHANNEL_MATCH_RECEIPT_KIND,
        CHANNEL_MATCH_RECEIPT_SCHEMA,
        payload,
    )
    .await;
}

/// CH-07 (#1878): extract `ChannelMatchBatchInfo` from the transcript
/// `batch_summary` JSON the dispatcher already builds for batched
/// triggers. Returns `None` for non-batched dispatch.
fn batch_info_from_summary(
    batch_summary: Option<&serde_json::Value>,
) -> Option<ChannelMatchBatchInfo> {
    let summary = batch_summary?.as_object()?;
    let count = summary
        .get("count")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)?;
    let constituent_event_ids = summary
        .get("constituent_event_ids")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    Some(ChannelMatchBatchInfo {
        count,
        constituent_event_ids,
    })
}

fn emitted_by(context: &ChannelContext) -> String {
    context
        .worker_id
        .clone()
        .or_else(|| context.agent_session_id.clone())
        .or_else(|| context.task_id.clone())
        .unwrap_or_else(|| "harn".to_string())
}

fn retention_for_scope(scope: ChannelScope) -> &'static str {
    match scope {
        ChannelScope::Session => "in_process_session",
        ChannelScope::Pipeline => "pipeline_event_log",
        ChannelScope::Tenant => "tenant_event_log",
        ChannelScope::Org => "org_event_log",
    }
}

fn receipt_value(
    topic: &Topic,
    event_id: EventId,
    event: &LogEvent,
    inserted: bool,
) -> Result<serde_json::Value, VmError> {
    let record = stored_record(event)?;
    Ok(serde_json::json!({
        "event_id": event_id,
        "cursor": event_id,
        "id": record.id,
        "name": record.name,
        "name_resolved": record.name,
        "scope": record.scope,
        "scope_id": record.scope_id,
        "payload": record.payload,
        "emitted_at": record.emitted_at,
        "emitted_by": record.emitted_by,
        "pipeline_id": record.pipeline_id,
        "session_id": record.session_id,
        "tenant_id": record.tenant_id,
        "retention": record.retention,
        "ttl_ms": record.ttl_ms,
        "topic": topic.as_str(),
        "inserted": inserted,
        "duplicate": !inserted,
    }))
}

fn event_value(
    topic: &Topic,
    event_id: EventId,
    event: LogEvent,
) -> Result<serde_json::Value, VmError> {
    let record = stored_record(&event)?;
    Ok(serde_json::json!({
        "event_id": event_id,
        "cursor": event_id,
        "topic": topic.as_str(),
        "kind": event.kind,
        "headers": event.headers,
        "occurred_at_ms": event.occurred_at_ms,
        "id": record.id,
        "name": record.name,
        "name_resolved": record.name,
        "scope": record.scope,
        "scope_id": record.scope_id,
        "payload": record.payload,
        "emitted_at": record.emitted_at,
        "emitted_by": record.emitted_by,
        "pipeline_id": record.pipeline_id,
        "session_id": record.session_id,
        "tenant_id": record.tenant_id,
        "retention": record.retention,
        "ttl_ms": record.ttl_ms,
    }))
}

fn stored_record(event: &LogEvent) -> Result<StoredChannelEvent, VmError> {
    serde_json::from_value(event.payload.clone()).map_err(|error| {
        VmError::Runtime(format!(
            "channel event store contained malformed channel payload: {error}"
        ))
    })
}

fn parse_options(value: Option<&VmValue>, builtin: &str) -> Result<ChannelOptions, VmError> {
    let Some(value) = value else {
        return Ok(ChannelOptions::default());
    };
    match value {
        VmValue::Nil => Ok(ChannelOptions::default()),
        VmValue::Dict(options) => Ok(ChannelOptions {
            scope: option_string(options, "scope", builtin)?
                .map(|scope| ChannelScope::parse(&scope))
                .transpose()
                .map_err(VmError::from)?,
            id: option_string(options, "id", builtin)?,
            tenant_id: option_string(options, "tenant_id", builtin)?,
            session_id: option_string(options, "session_id", builtin)?,
            pipeline_id: option_string(options, "pipeline_id", builtin)?,
            from_cursor: option_non_negative_int(options, "from_cursor", builtin)?
                .or(option_non_negative_int(options, "cursor", builtin)?)
                .map(|value| value as EventId),
            limit: option_non_negative_int(options, "limit", builtin)?.map(|value| value as usize),
            ttl_ms: option_duration_ms(options, "ttl", builtin)?,
        }),
        other => Err(VmError::TypeError(format!(
            "{builtin}: options must be a dict or nil, got {}",
            other.type_name()
        ))),
    }
}

fn required_string(value: Option<&VmValue>, builtin: &str, name: &str) -> Result<String, VmError> {
    match value {
        Some(VmValue::String(value)) => Ok(value.to_string()),
        Some(other) => Err(VmError::TypeError(format!(
            "{builtin}: {name} must be a string, got {}",
            other.type_name()
        ))),
        None => Err(VmError::TypeError(format!("{builtin}: missing {name}"))),
    }
}

fn option_string(
    options: &BTreeMap<String, VmValue>,
    key: &str,
    builtin: &str,
) -> Result<Option<String>, VmError> {
    match options.get(key) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::String(value)) if !value.trim().is_empty() => Ok(Some(value.to_string())),
        Some(VmValue::String(_)) => Err(VmError::TypeError(format!(
            "{builtin}: options.{key} cannot be empty"
        ))),
        Some(other) => Err(VmError::TypeError(format!(
            "{builtin}: options.{key} must be a string or nil, got {}",
            other.type_name()
        ))),
    }
}

fn option_non_negative_int(
    options: &BTreeMap<String, VmValue>,
    key: &str,
    builtin: &str,
) -> Result<Option<u64>, VmError> {
    match options.get(key) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::Int(value)) if *value >= 0 => Ok(Some(*value as u64)),
        Some(other) => Err(VmError::TypeError(format!(
            "{builtin}: options.{key} must be a non-negative int or nil, got {}",
            other.type_name()
        ))),
    }
}

fn option_duration_ms(
    options: &BTreeMap<String, VmValue>,
    key: &str,
    builtin: &str,
) -> Result<Option<i64>, VmError> {
    match options.get(key) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::Duration(value)) if *value >= 0 => Ok(Some(*value)),
        Some(VmValue::Int(value)) if *value >= 0 => Ok(Some(*value)),
        Some(other) => Err(VmError::TypeError(format!(
            "{builtin}: options.{key} must be a non-negative duration, int, or nil, got {}",
            other.type_name()
        ))),
    }
}

fn dict_string(values: &BTreeMap<String, VmValue>, key: &str) -> Option<String> {
    match values.get(key) {
        Some(VmValue::String(value)) if !value.is_empty() => Some(value.to_string()),
        _ => None,
    }
}

fn channel_log_error(error: crate::event_log::LogError) -> VmError {
    VmError::Runtime(format!("channel event log: {error}"))
}

/// CH-06 (#1877): RAII guard around channel emit/match tracing spans.
///
/// Mirrors `PoolSpanGuard` (PL-06 / #1891): opens both a thread-local
/// Harn span (visible to `trace_spans()`) and an OTel `tracing::Span`
/// (visible to the exporter), wires OTel span links via
/// `crate::observability::otel::set_span_link`, and closes them both on
/// `end()` / `Drop`. Disabled-tracing path is a no-op because
/// `crate::tracing::span_start_*` returns id 0 and short-circuits.
struct ChannelSpanGuard {
    span_id: u64,
    otel_span: tracing::Span,
}

impl ChannelSpanGuard {
    fn start(
        kind: crate::tracing::SpanKind,
        name: String,
        links: Vec<crate::tracing::SpanLink>,
    ) -> Self {
        Self::start_with_parenting(kind, name, links, true)
    }

    fn start_detached(
        kind: crate::tracing::SpanKind,
        name: String,
        links: Vec<crate::tracing::SpanLink>,
    ) -> Self {
        Self::start_with_parenting(kind, name, links, false)
    }

    fn start_with_parenting(
        kind: crate::tracing::SpanKind,
        name: String,
        links: Vec<crate::tracing::SpanLink>,
        inherit_parent: bool,
    ) -> Self {
        let span_id = if inherit_parent {
            crate::tracing::span_start_with_links(kind, name.clone(), links.clone())
        } else {
            crate::tracing::span_start_detached_with_links(kind, name.clone(), links.clone())
        };
        let otel_span = tracing::info_span!(
            target: "harn.vm.channel",
            "harn.channel",
            harn.kind = kind.as_str(),
            harn.name = %name,
        );
        for link in links {
            let trace_id = crate::TraceId(link.trace_id);
            let mut attributes: std::collections::HashMap<String, String> =
                link.attributes.into_iter().collect();
            attributes
                .entry("harn.link.kind".to_string())
                .or_insert_with(|| "channel_emit".to_string());
            let _ = crate::observability::otel::set_span_link(
                &otel_span,
                &trace_id,
                &link.span_id,
                Some(attributes),
            );
        }
        Self { span_id, otel_span }
    }

    fn link(&self) -> Option<crate::tracing::SpanLink> {
        crate::observability::otel::current_span_context_hex(&self.otel_span)
            .map(|(trace_id, span_id)| crate::tracing::SpanLink::new(trace_id, span_id))
            .or_else(|| crate::tracing::span_link(self.span_id))
    }

    fn set_metadata(&self, key: &str, value: serde_json::Value) {
        crate::tracing::span_set_metadata(self.span_id, key, value);
    }

    fn end(&mut self) {
        if self.span_id != 0 {
            crate::tracing::span_end(self.span_id);
            self.span_id = 0;
        }
    }
}

impl Drop for ChannelSpanGuard {
    fn drop(&mut self) {
        self.end();
    }
}

/// CH-06 (#1877): summarize an emit payload for the transcript event so
/// downstream renderers / audit feeds don't have to ingest the full
/// payload. Numbers/bools render verbatim; strings are truncated; dicts
/// and lists collapse to their top-level field count. Keeps the
/// transcript log compact while preserving enough context for human
/// inspection.
fn summarize_payload(payload: &serde_json::Value) -> serde_json::Value {
    const MAX_STRING_LEN: usize = 120;
    match payload {
        serde_json::Value::Null => serde_json::json!({"kind": "null"}),
        serde_json::Value::Bool(value) => serde_json::json!({"kind": "bool", "value": value}),
        serde_json::Value::Number(value) => serde_json::json!({"kind": "number", "value": value}),
        serde_json::Value::String(value) => {
            let truncated: String = value.chars().take(MAX_STRING_LEN).collect();
            let len = value.chars().count();
            serde_json::json!({
                "kind": "string",
                "value": truncated,
                "truncated": len > MAX_STRING_LEN,
                "length": len,
            })
        }
        serde_json::Value::Array(items) => {
            serde_json::json!({"kind": "array", "length": items.len()})
        }
        serde_json::Value::Object(map) => {
            let fields: Vec<&String> = map.keys().take(8).collect();
            serde_json::json!({
                "kind": "object",
                "field_count": map.len(),
                "fields": fields,
            })
        }
    }
}

/// CH-06 (#1877): append a channel transcript lifecycle event onto the
/// active event log. No-op when no log is installed (e.g. unit tests
/// running outside a VM context) so emission stays infallible from the
/// caller's perspective.
fn emit_channel_transcript_event(kind: &'static str, payload: serde_json::Value) {
    let Some(log) = active_event_log() else {
        return;
    };
    let Ok(topic) = Topic::new(CHANNEL_TRANSCRIPT_TOPIC) else {
        return;
    };
    let event = LogEvent::new(kind, payload);
    if tokio::runtime::Handle::try_current().is_ok() {
        if let Ok(join) = std::thread::Builder::new()
            .name("harn-channel-transcript".to_string())
            .spawn(move || {
                let _ = futures::executor::block_on(log.append(&topic, event));
            })
        {
            let _ = join.join();
        }
    } else {
        let _ = futures::executor::block_on(log.append(&topic, event));
    }
}

/// CH-06 (#1877): emit the `transcript.channel.emit` lifecycle event the
/// moment the durable append succeeds (whether the append was fresh or
/// idempotent). Carries the emit span id when tracing is on so the
/// transcript log can be stitched against the OTel trace.
fn emit_channel_emit_transcript(
    record: &StoredChannelEvent,
    resolved: &ResolvedChannel,
    inserted: bool,
    span_id: u64,
) {
    let payload = serde_json::json!({
        "event_id": record.id,
        "name": record.name,
        "name_resolved": resolved.resolved_name,
        "scope": record.scope,
        "scope_id": record.scope_id,
        "payload_summary": summarize_payload(&record.payload),
        "emitted_at": record.emitted_at,
        "emitted_at_ms": record.emitted_at.at_ms,
        "emitted_by": record.emitted_by,
        "session_id": record.session_id,
        "pipeline_id": record.pipeline_id,
        "tenant_id": record.tenant_id,
        "inserted": inserted,
        "duplicate": !inserted,
        "span_id": if span_id == 0 { serde_json::Value::Null } else { serde_json::json!(span_id) },
    });
    emit_channel_transcript_event(CHANNEL_EMIT_TRANSCRIPT_KIND, payload);
}

/// CH-06 (#1877): emit the `transcript.channel.match` lifecycle event
/// just before the dispatcher invokes the handler. Carries the match
/// span id and, for batched triggers, the constituent event ids so the
/// transcript can render the full batch context inline.
#[allow(clippy::too_many_arguments)]
fn emit_channel_match_transcript(
    trigger_id: &str,
    handler_kind: &str,
    resolved: &ResolvedChannel,
    event_id: &str,
    matched_at_ms: i64,
    matched_in_session_id: Option<&str>,
    span_id: u64,
    batch: Option<serde_json::Value>,
) {
    let mut payload = serde_json::json!({
        "event_id": event_id,
        "name_resolved": resolved.resolved_name,
        "scope": resolved.scope.as_str(),
        "scope_id": resolved.scope_id,
        "trigger_id": trigger_id,
        "handler_kind": handler_kind,
        "matched_at_ms": matched_at_ms,
        "matched_in_session_id": matched_in_session_id,
        "span_id": if span_id == 0 { serde_json::Value::Null } else { serde_json::json!(span_id) },
    });
    if let Some(batch) = batch {
        if let Some(map) = payload.as_object_mut() {
            map.insert("batch".to_string(), batch);
        }
    }
    emit_channel_transcript_event(CHANNEL_MATCH_TRANSCRIPT_KIND, payload);
}

/// CH-06 (#1877): collect the originating-emit span links stashed on a
/// batch of `TriggerEvent`s. The non-batched dispatch path uses the
/// single-event variant directly; the aggregated/batched path threads
/// the per-event headers through the buffer and rebuilds the link list
/// here so the resulting `ChannelMatch` span multi-links to every
/// constituent emit.
fn emit_links_from_event(event: &TriggerEvent) -> Vec<crate::tracing::SpanLink> {
    let mut links = Vec::new();
    if let (Some(trace_id), Some(span_id)) = (
        event.headers.get(EMIT_TRACE_ID_HEADER),
        event.headers.get(EMIT_SPAN_ID_HEADER),
    ) {
        links.push(
            crate::tracing::SpanLink::new(trace_id.clone(), span_id.clone()).with_attributes(
                BTreeMap::from([("harn.link.kind".to_string(), "channel_emit".to_string())]),
            ),
        );
    }
    links
}

fn emit_links_from_batch(events: &[TriggerEvent]) -> Vec<crate::tracing::SpanLink> {
    let mut links = Vec::new();
    for event in events {
        links.extend(emit_links_from_event(event));
    }
    links
}

fn batch_summary_for_transcript(events: &[TriggerEvent]) -> serde_json::Value {
    let constituent_ids: Vec<String> = events.iter().map(|event| event.id.0.clone()).collect();
    serde_json::json!({
        "count": events.len(),
        "constituent_event_ids": constituent_ids,
    })
}

/// Parsed channel-source trigger selector.
///
/// Trigger DSL strings look like `channel:<scope>:<scope-id>:<name>` with
/// shorthand forms for tenant-default and session/pipeline scopes. The
/// `scope_id_pattern` is `None` for "current" (e.g. `channel:foo` against
/// the trigger's tenant) or `Some("*")` for an explicit wildcard
/// (`channel:tenant:*:foo`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelSelector {
    scope: ChannelScope,
    scope_id_pattern: ScopeIdPattern,
    name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ScopeIdPattern {
    /// Match the current tenant/session/pipeline of the trigger registry
    /// (no explicit scope id supplied in the selector string).
    Current,
    /// Explicit scope id from the selector string.
    Exact(String),
    /// Wildcard, e.g. `tenant:*:foo` — match any scope id within the
    /// trigger's entitled boundary (today: the current tenant only).
    Wildcard,
}

impl ChannelSelector {
    /// Parse a `channel:...` trigger source string.
    ///
    /// Accepted shapes:
    /// - `channel:<name>` — tenant scope (current tenant), exact `<name>`
    /// - `channel:session:<name>` — session scope, exact `<name>`
    /// - `channel:pipeline:<name>` — pipeline scope, exact `<name>`
    /// - `channel:tenant:<tenant-id>:<name>` — explicit tenant
    /// - `channel:tenant:*:<name>` — tenant wildcard (within entitlement)
    /// - `channel:org:<org-id>:<name>` — explicit org (currently disabled)
    pub fn parse(input: &str) -> Result<Self, String> {
        let input = input.trim();
        let rest = input
            .strip_prefix("channel:")
            .ok_or_else(|| format!("channel selector must start with `channel:`, got `{input}`"))?;
        if rest.is_empty() {
            return Err("channel selector cannot be empty after `channel:` prefix".to_string());
        }

        let (head, tail_opt) = match rest.split_once(':') {
            Some((head, tail)) => (head, Some(tail)),
            None => (rest, None),
        };
        let parsed_scope = ChannelScope::parse(head).ok();
        match (parsed_scope, tail_opt) {
            // `channel:<name>` — tenant default.
            (None, _) => {
                let name = rest.to_string();
                validate_selector_name(&name)?;
                Ok(Self {
                    scope: ChannelScope::Tenant,
                    scope_id_pattern: ScopeIdPattern::Current,
                    name,
                })
            }
            (Some(scope @ (ChannelScope::Session | ChannelScope::Pipeline)), Some(name))
                if !name.is_empty() =>
            {
                if name.contains(':') {
                    return Err(format!(
                        "channel selector `{input}`: {} scope expects `<name>` with no extra colons",
                        scope.as_str()
                    ));
                }
                validate_selector_name(name)?;
                Ok(Self {
                    scope,
                    scope_id_pattern: ScopeIdPattern::Current,
                    name: name.to_string(),
                })
            }
            (Some(scope @ (ChannelScope::Tenant | ChannelScope::Org)), Some(tail))
                if !tail.is_empty() =>
            {
                let Some((scope_id, name)) = tail.split_once(':') else {
                    // `channel:tenant:foo` — treat as `<name>` in tenant default.
                    if matches!(scope, ChannelScope::Tenant) {
                        validate_selector_name(tail)?;
                        return Ok(Self {
                            scope,
                            scope_id_pattern: ScopeIdPattern::Current,
                            name: tail.to_string(),
                        });
                    }
                    return Err(format!(
                        "channel selector `{input}`: org scope requires `<org-id>:<name>`"
                    ));
                };
                if scope_id.is_empty() || name.is_empty() {
                    return Err(format!(
                        "channel selector `{input}`: scope id and name must be non-empty"
                    ));
                }
                validate_selector_name(name)?;
                let pattern = if scope_id == "*" {
                    ScopeIdPattern::Wildcard
                } else {
                    ScopeIdPattern::Exact(scope_id.to_string())
                };
                Ok(Self {
                    scope,
                    scope_id_pattern: pattern,
                    name: name.to_string(),
                })
            }
            (Some(scope), _) => Err(format!(
                "channel selector `{input}`: {} scope requires `<name>` segment",
                scope.as_str()
            )),
        }
    }

    pub fn scope(&self) -> &'static str {
        self.scope.as_str()
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns true if the supplied emit (scope, scope_id, name) matches this
    /// selector. `current_tenant` lets the matcher resolve the implicit
    /// "current tenant" boundary used by both `Current` and `Wildcard` modes.
    pub fn matches(&self, scope: &str, scope_id: &str, name: &str, current_tenant: &str) -> bool {
        if self.scope.as_str() != scope || self.name != name {
            return false;
        }
        match &self.scope_id_pattern {
            ScopeIdPattern::Current => match self.scope {
                ChannelScope::Tenant => scope_id == current_tenant,
                ChannelScope::Session | ChannelScope::Pipeline => {
                    // For session/pipeline, "current" means trigger and emit
                    // share a runtime context. In v1 (in-process registry)
                    // this is implicit: both producer and consumer run in the
                    // same VM, so any scope_id within this scope type matches.
                    true
                }
                ChannelScope::Org => false,
            },
            ScopeIdPattern::Exact(value) => scope_id == value,
            ScopeIdPattern::Wildcard => match self.scope {
                ChannelScope::Tenant => true,
                // Session/pipeline wildcards aren't entitled yet; org wildcards disabled.
                _ => false,
            },
        }
    }
}

fn validate_selector_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty()
        || name.contains(':')
        || name.chars().any(|ch| ch.is_control() || ch.is_whitespace())
    {
        return Err(format!("channel selector name `{name}` is malformed"));
    }
    Ok(())
}

/// Dispatch a freshly emitted channel event to any registered triggers whose
/// `channel:` source selector matches the (scope, scope_id, name) tuple.
///
/// This is the consumer side of the `emit_channel` ↔ trigger plumbing
/// (CH-02 / #1872). Errors from individual handlers do not abort the
/// emit; they surface in the dispatcher's DLQ + retry pipeline.
///
/// CH-04 (#1875): bindings with `batch { count, window, key, expire_action }`
/// route events through an aggregation buffer instead of dispatching one
/// event at a time. When the buffer hits `count`, the dispatcher fires
/// the handler with a batched event (`event.batch` populated). When the
/// `window` elapses with fewer than `count` events, the dispatcher
/// either fires a partial batch (default) or discards the buffer.
async fn dispatch_channel_emit_to_triggers(
    resolved: &ResolvedChannel,
    payload: ChannelEventPayload,
    emit_link: Option<crate::tracing::SpanLink>,
) -> Result<(), VmError> {
    // Snapshot matching bindings outside of any async work so the registry
    // borrow is short-lived.
    let bindings = crate::triggers::registry::channel_bindings_matching(
        resolved.scope.as_str(),
        &resolved.scope_id,
        &payload.name,
    );

    // CH-04 (#1875): flush any aggregation buffers whose window has
    // elapsed BEFORE this emit so old batches go out in order. The
    // implicit sweep runs even for emits that don't match any binding so
    // a stale buffer can't outlive the trigger lifecycle. Tests can also
    // call `flush_trigger_aggregations()` directly for deterministic
    // window-expire coverage.
    flush_expired_aggregations_inner().await;

    if bindings.is_empty() {
        return Ok(());
    }
    let Some(base_vm) = crate::vm::clone_async_builtin_child_vm() else {
        // No host VM (e.g. raw test path); nothing to dispatch.
        return Ok(());
    };
    let log = active_event_log()
        .unwrap_or_else(|| install_memory_for_current_thread(CHANNEL_QUEUE_DEPTH));
    let dispatcher = crate::triggers::Dispatcher::with_event_log(base_vm, log);
    for binding in bindings {
        // Filter (#1872 acceptance): JSON-path-equality on payload.
        // Applied BEFORE aggregation so the buffer only collects events
        // the handler actually cares about.
        if let Some(filter_str) = binding.filter.as_ref() {
            if !channel_filter_matches(filter_str, &payload.payload) {
                continue;
            }
        }
        let event = build_channel_trigger_event(&payload, emit_link.as_ref());

        // CH-04 (#1875): aggregation path. When the binding declared
        // `batch`, accumulate into the per-(binding, partition_key)
        // buffer; only dispatch once the threshold is reached.
        if let Some(aggregation_config) = binding.aggregation.as_ref() {
            let partition_key = crate::triggers::aggregation::partition_key_for_event(
                aggregation_config,
                &payload.payload,
            );
            let binding_key = binding.binding_key();
            let outcome = crate::triggers::aggregation::accumulate(
                &binding_key,
                aggregation_config,
                partition_key.as_deref(),
                event,
            );
            if let crate::triggers::aggregation::AccumulateOutcome::Ready(events) = outcome {
                // CH-06 (#1877): the ChannelMatch span for a batched
                // trigger multi-links to ALL constituent ChannelEmit
                // spans so the trace tree shows the aggregation fan-in.
                let links = emit_links_from_batch(&events);
                let batch_summary = batch_summary_for_transcript(&events);
                let batched = match crate::triggers::dispatcher::build_batched_event_public(events)
                {
                    Ok(batched) => batched,
                    Err(error) => {
                        return Err(VmError::Runtime(format!(
                            "emit_channel aggregation batch: {error}"
                        )));
                    }
                };
                fire_channel_match(
                    &dispatcher,
                    binding.clone(),
                    batched,
                    resolved,
                    links,
                    Some(batch_summary),
                )
                .await;
            }
            continue;
        }

        let links = emit_links_from_event(&event);
        fire_channel_match(&dispatcher, binding.clone(), event, resolved, links, None).await;
    }
    Ok(())
}

/// CH-06 (#1877): open a `ChannelMatch` span, emit the transcript
/// lifecycle event, and dispatch the trigger handler. The span links
/// back to the originating ChannelEmit span (multi-link for batched
/// triggers) via `set_span_link` from P-05 (#1858).
async fn fire_channel_match(
    dispatcher: &crate::triggers::Dispatcher,
    binding: std::sync::Arc<crate::triggers::registry::TriggerBinding>,
    event: TriggerEvent,
    resolved: &ResolvedChannel,
    links: Vec<crate::tracing::SpanLink>,
    batch_summary: Option<serde_json::Value>,
) {
    let trigger_id = binding.id.as_str().to_string();
    let handler_kind = binding.handler.kind().to_string();
    // CH-07 (#1878): use the channel emit's id (carried as the trigger
    // event's `dedupe_key`) so the match receipt links back to the
    // original `ChannelEmitReceipt.event_id`. `TriggerEvent::new` mints
    // a fresh `trigger_evt_*` id for its own `event.id` field that does
    // not correlate to the emit chain.
    let event_id = if event.dedupe_key.is_empty() {
        event.id.0.clone()
    } else {
        event.dedupe_key.clone()
    };
    let mut match_span = ChannelSpanGuard::start_detached(
        crate::tracing::SpanKind::ChannelMatch,
        format!("channel.match {}", resolved.resolved_name),
        links,
    );
    match_span.set_metadata("event_id", serde_json::json!(event_id));
    match_span.set_metadata("trigger_id", serde_json::json!(trigger_id));
    match_span.set_metadata("handler_kind", serde_json::json!(handler_kind));
    match_span.set_metadata("name_resolved", serde_json::json!(resolved.resolved_name));
    if let Some(summary) = batch_summary.as_ref() {
        match_span.set_metadata("batch", summary.clone());
    }
    let span_id = crate::tracing::current_span_id().unwrap_or(0);
    let matched_at_ms = crate::clock_mock::now_utc().unix_timestamp_nanos() as i64 / 1_000_000;
    let matched_in_session_id = crate::agent_sessions::current_session_id()
        .or_else(|| event.tenant_id.as_ref().map(|t| t.0.clone()));
    emit_channel_match_transcript(
        &trigger_id,
        &handler_kind,
        resolved,
        &event_id,
        matched_at_ms,
        matched_in_session_id.as_deref(),
        span_id,
        batch_summary.clone(),
    );
    // CH-07 (#1878): capture the dispatch outcome BEFORE writing the
    // match receipt so the receipt records the recorded handler result
    // (succeeded/failed/dlq/...) — replay tooling treats the receipt as
    // the cached match: on replay the dispatcher looks up the receipt
    // by `event_id` instead of re-evaluating the filter spec.
    let dispatch_outcome = dispatcher.dispatch(&binding, event).await;
    let binding_key = binding.binding_key();
    let batch_info = batch_info_from_summary(batch_summary.as_ref());
    record_channel_match_receipt(
        &trigger_id,
        &binding_key,
        &handler_kind,
        resolved,
        &event_id,
        matched_in_session_id.as_deref(),
        batch_info,
        span_id,
        &dispatch_outcome,
    )
    .await;
    // Pre-CH-07 the dispatch error was discarded with `let _ = ...`. The
    // CH-07 match receipt now records the failure with
    // `dispatch_failed: true` so audit consumers don't lose the signal —
    // we keep the same fire-and-forget callsite semantics here.
    drop(dispatch_outcome);
    match_span.end();
}

/// Drain all expired aggregation buffers and dispatch them. Exposed as
/// the `flush_trigger_aggregations()` builtin so Harn scripts (and the
/// production runtime) can deterministically advance window-expire
/// processing.
pub(crate) async fn flush_expired_aggregations_inner() {
    let expirations = crate::triggers::aggregation::drain_expired_aggregations();
    if expirations.is_empty() {
        return;
    }
    let Some(base_vm) = crate::vm::clone_async_builtin_child_vm() else {
        return;
    };
    let log = active_event_log()
        .unwrap_or_else(|| install_memory_for_current_thread(CHANNEL_QUEUE_DEPTH));
    let dispatcher = crate::triggers::Dispatcher::with_event_log(base_vm, log);
    for expired in expirations {
        if matches!(
            expired.action,
            crate::triggers::aggregation::ExpireAction::Discard
        ) {
            continue;
        }
        // Resolve the binding by parsing the binding_key (id@vN). Skip
        // if the binding has been terminated since the buffer was opened.
        let Some((trigger_id, version_str)) = expired.binding_key.rsplit_once("@v") else {
            continue;
        };
        let Ok(version) = version_str.parse::<u32>() else {
            continue;
        };
        let Ok(binding) =
            crate::triggers::registry::resolve_live_trigger_binding(trigger_id, Some(version))
        else {
            continue;
        };
        // CH-06 (#1877): rebuild the channel-resolved metadata from the
        // first buffered event so the ChannelMatch span carries the
        // right scope/name even for window-expire flushes.
        let resolved_for_match = resolved_from_first_event(&expired.events);
        let links = emit_links_from_batch(&expired.events);
        let batch_summary = batch_summary_for_transcript(&expired.events);
        let batched = match crate::triggers::dispatcher::build_batched_event_public(expired.events)
        {
            Ok(batched) => batched,
            Err(_) => continue,
        };
        match resolved_for_match {
            Some(resolved) => {
                fire_channel_match(
                    &dispatcher,
                    binding,
                    batched,
                    &resolved,
                    links,
                    Some(batch_summary),
                )
                .await;
            }
            None => {
                let _ = dispatcher.dispatch(&binding, batched).await;
            }
        }
    }
}

/// CH-06 (#1877): synthesize a `ResolvedChannel` from the first buffered
/// trigger event when a window-expire flush dispatches without going
/// back through `resolve_channel`. Returns `None` if the event payload
/// isn't a known channel payload (defensive — should not happen in
/// practice since the dispatcher only buffers channel events).
fn resolved_from_first_event(events: &[TriggerEvent]) -> Option<ResolvedChannel> {
    let first = events.first()?;
    let ProviderPayload::Known(KnownProviderPayload::Channel(payload)) = &first.provider_payload
    else {
        return None;
    };
    let scope = ChannelScope::parse(&payload.scope).ok()?;
    let topic = Topic::new(format!(
        "channels.{}.{}.{}",
        payload.scope,
        sanitize_topic_component(&payload.scope_id),
        sanitize_topic_component(&payload.name),
    ))
    .ok()?;
    Some(ResolvedChannel {
        scope,
        scope_id: payload.scope_id.clone(),
        resolved_name: payload.name_resolved.clone(),
        topic,
        retention: retention_for_scope(scope),
    })
}

fn build_channel_trigger_event(
    payload: &ChannelEventPayload,
    emit_link: Option<&crate::tracing::SpanLink>,
) -> TriggerEvent {
    let mut event = TriggerEvent::new(
        ProviderId::from("channel"),
        "channel.emit",
        None,
        payload.id.clone(),
        payload.tenant_id.clone().map(TenantId::new),
        BTreeMap::new(),
        ProviderPayload::Known(KnownProviderPayload::Channel(payload.clone())),
        SignatureStatus::Unsigned,
    );
    event.headers.insert(
        "harn_channel_name".to_string(),
        payload.name_resolved.clone(),
    );
    event
        .headers
        .insert("harn_channel_scope".to_string(), payload.scope.clone());
    event.headers.insert(
        "harn_channel_scope_id".to_string(),
        payload.scope_id.clone(),
    );
    // CH-06 (#1877): stash the ChannelEmit span coordinates on the trigger
    // event so the downstream ChannelMatch span can link back via
    // `set_span_link` — even after travelling through an aggregation
    // buffer or being serialized into a batched envelope.
    if let Some(link) = emit_link {
        event
            .headers
            .insert(EMIT_TRACE_ID_HEADER.to_string(), link.trace_id.clone());
        event
            .headers
            .insert(EMIT_SPAN_ID_HEADER.to_string(), link.span_id.clone());
    }
    event
}

/// Evaluate the trigger filter spec (CH-02 / #1872) against the channel payload.
///
/// Supported syntax v1: JSON dict (`{"repo": "harn"}`) — each key is a
/// dot-path into the payload that must equality-match the value. Missing
/// path = no match. Non-dict filter strings are treated as no-op (return
/// true) so we don't regress pre-existing trigger `filter:` semantics that
/// reuse this field for other purposes.
fn channel_filter_matches(filter_raw: &str, payload: &serde_json::Value) -> bool {
    let trimmed = filter_raw.trim();
    if trimmed.is_empty() {
        return true;
    }
    let parsed: serde_json::Value = match serde_json::from_str(trimmed) {
        Ok(value) => value,
        Err(_) => return true,
    };
    let Some(map) = parsed.as_object() else {
        return true;
    };
    map.iter()
        .all(|(key, expected)| match payload_path(payload, key) {
            Some(actual) => actual == expected,
            None => false,
        })
}

fn payload_path<'a>(value: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    let mut current = value;
    for segment in path.split('.') {
        if segment.is_empty() {
            return None;
        }
        current = match current {
            serde_json::Value::Object(map) => map.get(segment)?,
            _ => return None,
        };
    }
    Some(current)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> ChannelContext {
        ChannelContext {
            task_id: Some("task".to_string()),
            root_task_id: Some("root".to_string()),
            ..ChannelContext::default()
        }
    }

    #[test]
    fn resolves_bare_name_to_default_tenant() {
        let resolved =
            resolve_channel("pr.merged", &ChannelOptions::default(), &context()).unwrap();
        assert_eq!(resolved.scope, ChannelScope::Tenant);
        assert_eq!(resolved.resolved_name, "tenant:default:pr.merged");
        assert_eq!(resolved.topic.as_str(), "channels.tenant.default.pr.merged");
    }

    #[test]
    fn resolves_session_prefix_from_context() {
        let resolved =
            resolve_channel("session:agent.done", &ChannelOptions::default(), &context()).unwrap();
        assert_eq!(resolved.scope, ChannelScope::Session);
        assert_eq!(resolved.resolved_name, "session:root:agent.done");
    }

    #[test]
    fn missing_pipeline_context_reports_channel_error() {
        let err = resolve_channel(
            "pipeline:stage.done",
            &ChannelOptions::default(),
            &context(),
        )
        .unwrap_err();
        assert!(err.0.contains("HARN-CHN-001"));
    }

    #[test]
    fn org_scope_is_disabled() {
        let err = resolve_channel(
            "org:burin-labs:pr.merged",
            &ChannelOptions::default(),
            &context(),
        )
        .unwrap_err();
        assert!(err.0.contains("HARN-CHN-002"));
    }

    #[test]
    fn explicit_session_id_matching_context_resolves() {
        let ctx = ChannelContext {
            agent_session_id: Some("sess-A".to_string()),
            ..context()
        };
        let options = ChannelOptions {
            session_id: Some("sess-A".to_string()),
            ..ChannelOptions::default()
        };
        let resolved = resolve_channel("session:agent.done", &options, &ctx).unwrap();
        assert_eq!(resolved.scope, ChannelScope::Session);
        assert_eq!(resolved.resolved_name, "session:sess-A:agent.done");
    }

    #[test]
    fn explicit_session_id_conflict_reports_ambiguity() {
        let ctx = ChannelContext {
            agent_session_id: Some("sess-A".to_string()),
            ..context()
        };
        let options = ChannelOptions {
            session_id: Some("sess-B".to_string()),
            ..ChannelOptions::default()
        };
        let err = resolve_channel("session:agent.done", &options, &ctx).unwrap_err();
        assert!(
            err.0.contains("HARN-CHN-004"),
            "expected HARN-CHN-004, got: {}",
            err.0
        );
    }

    #[test]
    fn explicit_pipeline_id_conflict_reports_ambiguity() {
        let ctx = ChannelContext {
            workflow_id: Some("pipe-A".to_string()),
            ..context()
        };
        let options = ChannelOptions {
            pipeline_id: Some("pipe-B".to_string()),
            ..ChannelOptions::default()
        };
        let err = resolve_channel("pipeline:stage.done", &options, &ctx).unwrap_err();
        assert!(
            err.0.contains("HARN-CHN-004"),
            "expected HARN-CHN-004, got: {}",
            err.0
        );
    }

    #[test]
    fn explicit_tenant_mismatch_reports_cross_tenant() {
        let ctx = ChannelContext {
            tenant_id: Some("tenant-A".to_string()),
            ..context()
        };
        let options = ChannelOptions {
            tenant_id: Some("tenant-B".to_string()),
            ..ChannelOptions::default()
        };
        let err = resolve_channel("pr.merged", &options, &ctx).unwrap_err();
        assert!(
            err.0.contains("HARN-CHN-002"),
            "expected HARN-CHN-002, got: {}",
            err.0
        );
    }

    #[test]
    fn explicit_tenant_in_name_matching_context_resolves() {
        let ctx = ChannelContext {
            tenant_id: Some("tenant-A".to_string()),
            ..context()
        };
        let resolved = resolve_channel(
            "tenant:tenant-A:pr.merged",
            &ChannelOptions::default(),
            &ctx,
        )
        .unwrap();
        assert_eq!(resolved.scope, ChannelScope::Tenant);
        assert_eq!(resolved.resolved_name, "tenant:tenant-A:pr.merged");
    }

    #[test]
    fn cross_tenant_via_name_prefix_is_rejected() {
        let ctx = ChannelContext {
            tenant_id: Some("tenant-A".to_string()),
            ..context()
        };
        let err = resolve_channel(
            "tenant:tenant-B:pr.merged",
            &ChannelOptions::default(),
            &ctx,
        )
        .unwrap_err();
        assert!(err.0.contains("HARN-CHN-002"));
    }

    #[test]
    fn channel_selector_parses_tenant_default_shorthand() {
        let selector = ChannelSelector::parse("channel:pr.merged").expect("parses");
        assert_eq!(selector.scope(), "tenant");
        assert_eq!(selector.name(), "pr.merged");
        assert!(selector.matches("tenant", "default", "pr.merged", "default"));
        assert!(!selector.matches("tenant", "default", "other.event", "default"));
        assert!(!selector.matches("session", "default", "pr.merged", "default"));
    }

    #[test]
    fn channel_selector_parses_session_scope() {
        let selector = ChannelSelector::parse("channel:session:my-event").expect("parses");
        assert_eq!(selector.scope(), "session");
        assert_eq!(selector.name(), "my-event");
        assert!(selector.matches("session", "any-session-id", "my-event", "any-session-id"));
        assert!(!selector.matches("tenant", "any", "my-event", "any"));
    }

    #[test]
    fn channel_selector_parses_explicit_tenant() {
        let selector =
            ChannelSelector::parse("channel:tenant:burin-labs:pr.merged").expect("parses");
        assert!(selector.matches("tenant", "burin-labs", "pr.merged", "default"));
        assert!(!selector.matches("tenant", "other-tenant", "pr.merged", "default"));
    }

    #[test]
    fn channel_selector_parses_tenant_wildcard() {
        let selector = ChannelSelector::parse("channel:tenant:*:pr.merged").expect("parses");
        assert!(selector.matches("tenant", "burin-labs", "pr.merged", "default"));
        assert!(selector.matches("tenant", "other-tenant", "pr.merged", "default"));
        assert!(!selector.matches("tenant", "any", "different-event", "default"));
        assert!(!selector.matches("session", "any", "pr.merged", "default"));
    }

    #[test]
    fn channel_selector_rejects_malformed_inputs() {
        assert!(ChannelSelector::parse("not-a-channel").is_err());
        assert!(ChannelSelector::parse("channel:").is_err());
        assert!(ChannelSelector::parse("channel:session:").is_err());
        assert!(ChannelSelector::parse("channel:session:has:extra:colons").is_err());
        assert!(ChannelSelector::parse("channel:org:no-name").is_err());
        assert!(ChannelSelector::parse("channel:tenant::missing-id").is_err());
        assert!(ChannelSelector::parse("channel:tenant:foo:").is_err());
    }

    #[test]
    fn channel_filter_matches_equality_paths() {
        let payload = serde_json::json!({"repo": "harn", "nested": {"k": "v"}});
        assert!(channel_filter_matches("{\"repo\": \"harn\"}", &payload));
        assert!(!channel_filter_matches("{\"repo\": \"other\"}", &payload));
        assert!(channel_filter_matches("{\"nested.k\": \"v\"}", &payload));
        assert!(!channel_filter_matches("{\"nested.k\": \"x\"}", &payload));
        // Missing path → no match.
        assert!(!channel_filter_matches("{\"missing\": \"x\"}", &payload));
        // Empty filter → permissive.
        assert!(channel_filter_matches("", &payload));
        // Non-dict filter → permissive (back-compat with legacy `filter:` field).
        assert!(channel_filter_matches("just-a-string", &payload));
    }

    // CH-07 (#1878): `channel_payload_hash` is the byte signal the
    // replay-oracle's `HARN-REP-CHN-002` diagnostic compares across
    // runs. The hash MUST be deterministic across object-key iteration
    // order (so the test in `replay_oracle` and the conformance fixture
    // both rely on canonical ordering) and MUST change when the payload
    // changes.
    #[test]
    fn channel_payload_hash_is_deterministic_across_key_order() {
        let a = serde_json::json!({"a": 1, "b": 2, "nested": {"x": 10, "y": 20}});
        let b = serde_json::json!({"nested": {"y": 20, "x": 10}, "b": 2, "a": 1});
        assert_eq!(channel_payload_hash(&a), channel_payload_hash(&b));
    }

    #[test]
    fn channel_payload_hash_changes_with_value_drift() {
        let baseline = serde_json::json!({"repo": "harn", "attempt": 1});
        let drifted = serde_json::json!({"repo": "harn", "attempt": 2});
        assert_ne!(
            channel_payload_hash(&baseline),
            channel_payload_hash(&drifted)
        );
    }

    #[test]
    fn channel_payload_hash_is_sha256_prefixed_hex() {
        let value = serde_json::json!({"k": "v"});
        let hash = channel_payload_hash(&value);
        assert!(hash.starts_with("sha256:"));
        assert_eq!(hash.len(), "sha256:".len() + 64);
    }
}
