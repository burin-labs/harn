use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use time::format_description::well_known::Rfc3339;

use crate::event_log::{
    active_event_log, install_memory_for_current_thread, sanitize_topic_component, AnyEventLog,
    EventId, EventLog, LogEvent, Topic,
};
use crate::llm::vm_value_to_json;
use crate::value::{VmError, VmValue};

const CHANNEL_QUEUE_DEPTH: usize = 128;
const CHANNEL_EVENT_KIND: &str = "channel.emit";
const IDEMPOTENCY_HEADER: &str = "harn.channel.id";
const NAME_HEADER: &str = "harn.channel.name";
const SCOPE_HEADER: &str = "harn.channel.scope";
const SCOPE_ID_HEADER: &str = "harn.channel.scope_id";
const EMITTED_BY_HEADER: &str = "harn.channel.emitted_by";

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

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SignedTimestamp {
    at_ms: i64,
    at: String,
    algorithm: String,
    key_id: String,
    signature: String,
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

    let mut headers = BTreeMap::new();
    headers.insert(IDEMPOTENCY_HEADER.to_string(), event_id.clone());
    headers.insert(NAME_HEADER.to_string(), resolved.resolved_name.clone());
    headers.insert(
        SCOPE_HEADER.to_string(),
        resolved.scope.as_str().to_string(),
    );
    headers.insert(SCOPE_ID_HEADER.to_string(), resolved.scope_id.clone());
    headers.insert(EMITTED_BY_HEADER.to_string(), emitted_by);

    let log = log_for_scope(resolved.scope);
    let mut log_event = LogEvent::new(
        CHANNEL_EVENT_KIND,
        serde_json::to_value(record)
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

    fn session_id(&self, options: &ChannelOptions) -> String {
        options
            .session_id
            .clone()
            .or_else(|| self.agent_session_id.clone())
            .or_else(|| self.root_agent_session_id.clone())
            .or_else(|| self.scope_id.clone())
            .or_else(|| self.root_task_id.clone())
            .unwrap_or_else(|| "session".to_string())
    }

    fn pipeline_id(&self, options: &ChannelOptions) -> Result<String, ChannelError> {
        options
            .pipeline_id
            .clone()
            .or_else(|| self.workflow_id.clone())
            .or_else(|| self.run_id.clone())
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
        ChannelScope::Session => parsed
            .scope_id
            .clone()
            .unwrap_or_else(|| context.session_id(options)),
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
}
