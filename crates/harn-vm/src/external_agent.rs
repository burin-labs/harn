use std::collections::{BTreeMap, HashMap};
use std::sync::{LazyLock, Mutex};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::sync::broadcast;

use crate::a2a::{A2aClientError, ResolvedA2aEndpoint};
use crate::orchestration::{
    extract_handoffs_from_json_value, handoff_artifact_record, ArtifactRecord, HandoffArtifact,
    HandoffBudgetRemainingRecord, HandoffTargetRecord,
};

pub const EXTERNAL_AGENT_SCHEMA_ID: &str = "harn.external_agent.v1";
pub const EXTERNAL_AGENT_HANDOFF_SCHEMA_ID: &str = "harn.external_agent.handoff.v1";
pub const A2A_PLAN_METHOD: &str = "_harn/externalAgent.plan";
pub const A2A_DISPATCH_METHOD: &str = "_harn/externalAgent.dispatch";

static IDEMPOTENCY_CACHE: LazyLock<Mutex<HashMap<String, ExternalAgentDelegationEnvelope>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[async_trait]
pub trait ExternalAgent: Send + Sync {
    async fn capabilities(
        &self,
        request: &ExternalAgentDelegationRequest,
        cancel_rx: &mut broadcast::Receiver<()>,
    ) -> Result<ExternalAgentPeer, ExternalAgentError>;

    async fn plan(
        &self,
        peer: &ExternalAgentPeer,
        request: &ExternalAgentDelegationRequest,
        cancel_rx: &mut broadcast::Receiver<()>,
    ) -> Result<Option<ExternalAgentPlanCheckpoint>, ExternalAgentError>;

    async fn dispatch(
        &self,
        peer: &ExternalAgentPeer,
        request: &ExternalAgentDelegationRequest,
        checkpoint: &ExternalAgentPlanCheckpoint,
        cancel_rx: &mut broadcast::Receiver<()>,
    ) -> Result<Value, ExternalAgentError>;
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExternalAgentTransport {
    #[default]
    A2a,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ExternalAgentBudget {
    pub max_usd: Option<f64>,
    pub max_tokens: Option<u64>,
    pub max_seconds: Option<u64>,
    pub max_tool_calls: Option<u64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ExternalAgentBudgetUsage {
    #[serde(alias = "max_usd", alias = "dollars", alias = "cost_usd")]
    pub usd: Option<f64>,
    #[serde(alias = "max_tokens", alias = "token_count")]
    pub tokens: Option<u64>,
    #[serde(alias = "max_seconds", alias = "duration_seconds")]
    pub seconds: Option<u64>,
    #[serde(alias = "max_tool_calls")]
    pub tool_calls: Option<u64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ExternalAgentCheckpointPolicy {
    pub approved: bool,
    pub approved_by: Option<String>,
    pub allow_local_fallback: bool,
    pub local_plan: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ExternalAgentDelegationRequest {
    pub transport: ExternalAgentTransport,
    pub target: String,
    pub allow_cleartext: bool,
    pub task: String,
    pub budget: ExternalAgentBudget,
    pub checkpoint: ExternalAgentCheckpointPolicy,
    pub idempotency_key: Option<String>,
    pub expected_scope: Vec<String>,
    pub context: Value,
    pub metadata: BTreeMap<String, Value>,
}

impl Default for ExternalAgentDelegationRequest {
    fn default() -> Self {
        Self {
            transport: ExternalAgentTransport::A2a,
            target: String::new(),
            allow_cleartext: false,
            task: String::new(),
            budget: ExternalAgentBudget::default(),
            checkpoint: ExternalAgentCheckpointPolicy::default(),
            idempotency_key: None,
            expected_scope: Vec::new(),
            context: Value::Null,
            metadata: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ExternalAgentCapabilities {
    pub schema: Option<String>,
    pub pre_dispatch_checkpoint: bool,
    pub budget_cap: bool,
    pub idempotency: bool,
    pub reviewable_handoff: bool,
    pub dispatch: bool,
    pub operations: Vec<String>,
    pub raw: Option<Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ExternalAgentPeer {
    pub transport: ExternalAgentTransport,
    pub target: String,
    pub card_url: String,
    pub rpc_url: String,
    pub agent_id: Option<String>,
    pub target_agent: String,
    pub capabilities: ExternalAgentCapabilities,
    pub card: Value,
}

impl Default for ExternalAgentPeer {
    fn default() -> Self {
        Self {
            transport: ExternalAgentTransport::A2a,
            target: String::new(),
            card_url: String::new(),
            rpc_url: String::new(),
            agent_id: None,
            target_agent: String::new(),
            capabilities: ExternalAgentCapabilities::default(),
            card: Value::Null,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ExternalAgentPlanCheckpoint {
    #[serde(rename = "_type")]
    pub type_name: String,
    pub schema: String,
    pub checkpoint_id: String,
    pub source: String,
    pub plan: String,
    pub expected_scope: Vec<String>,
    pub budget: ExternalAgentBudget,
    pub evidence_refs: Vec<Value>,
    pub metadata: BTreeMap<String, Value>,
}

impl Default for ExternalAgentPlanCheckpoint {
    fn default() -> Self {
        Self {
            type_name: "external_agent_checkpoint".to_string(),
            schema: EXTERNAL_AGENT_SCHEMA_ID.to_string(),
            checkpoint_id: new_id("external_checkpoint"),
            source: "remote".to_string(),
            plan: String::new(),
            expected_scope: Vec::new(),
            budget: ExternalAgentBudget::default(),
            evidence_refs: Vec::new(),
            metadata: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ExternalAgentDelegationEnvelope {
    #[serde(rename = "_type")]
    pub type_name: String,
    pub schema: String,
    pub id: String,
    pub status: String,
    pub error: Option<String>,
    pub transport: ExternalAgentTransport,
    pub target: String,
    pub allow_cleartext: bool,
    pub task: String,
    pub budget: ExternalAgentBudget,
    pub budget_used: Option<ExternalAgentBudgetUsage>,
    pub idempotency_key: Option<String>,
    pub capabilities: Option<ExternalAgentCapabilities>,
    pub checkpoint: Option<ExternalAgentPlanCheckpoint>,
    pub handoff: Option<Value>,
    pub artifacts: Vec<Value>,
    pub receipts: Vec<Value>,
    pub evidence_refs: Vec<Value>,
    pub result: Option<Value>,
    pub replayed: bool,
    pub replay_of: Option<String>,
    pub metadata: BTreeMap<String, Value>,
}

impl Default for ExternalAgentDelegationEnvelope {
    fn default() -> Self {
        Self {
            type_name: "external_agent_delegation".to_string(),
            schema: EXTERNAL_AGENT_HANDOFF_SCHEMA_ID.to_string(),
            id: new_id("external_delegate"),
            status: "created".to_string(),
            error: None,
            transport: ExternalAgentTransport::A2a,
            target: String::new(),
            allow_cleartext: false,
            task: String::new(),
            budget: ExternalAgentBudget::default(),
            budget_used: None,
            idempotency_key: None,
            capabilities: None,
            checkpoint: None,
            handoff: None,
            artifacts: Vec::new(),
            receipts: Vec::new(),
            evidence_refs: Vec::new(),
            result: None,
            replayed: false,
            replay_of: None,
            metadata: BTreeMap::new(),
        }
    }
}

#[derive(Debug)]
pub enum ExternalAgentError {
    InvalidRequest(String),
    Transport(String),
    Protocol(String),
}

impl std::fmt::Display for ExternalAgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidRequest(message) | Self::Transport(message) | Self::Protocol(message) => {
                f.write_str(message)
            }
        }
    }
}

impl std::error::Error for ExternalAgentError {}

impl From<A2aClientError> for ExternalAgentError {
    fn from(error: A2aClientError) -> Self {
        match error {
            A2aClientError::InvalidTarget(message) => Self::InvalidRequest(message),
            A2aClientError::Discovery(message)
            | A2aClientError::Denied(message)
            | A2aClientError::Timeout(message)
            | A2aClientError::Cancelled(message) => Self::Transport(message),
            A2aClientError::Protocol(message) => Self::Protocol(message),
        }
    }
}

pub fn reset_external_agent_state() {
    idempotency_cache().clear();
}

pub async fn delegate_external_agent(
    request: ExternalAgentDelegationRequest,
    cancel_rx: &mut broadcast::Receiver<()>,
) -> Result<ExternalAgentDelegationEnvelope, ExternalAgentError> {
    validate_request(&request)?;
    let idempotency_key = request.idempotency_key.as_deref().unwrap_or_default();
    if let Some(cached) = cached_response(&request, idempotency_key) {
        return Ok(cached);
    }

    match request.transport {
        ExternalAgentTransport::A2a => {
            let agent = A2aExternalAgent;
            delegate_with_agent(&agent, request, cancel_rx).await
        }
    }
}

async fn delegate_with_agent<A: ExternalAgent>(
    agent: &A,
    request: ExternalAgentDelegationRequest,
    cancel_rx: &mut broadcast::Receiver<()>,
) -> Result<ExternalAgentDelegationEnvelope, ExternalAgentError> {
    let idempotency_key = request.idempotency_key.as_deref().unwrap_or_default();
    let cached_checkpoint = cached_checkpoint(idempotency_key);
    let peer = agent.capabilities(&request, cancel_rx).await?;
    let mut base = base_envelope(&request, &peer);

    let missing = missing_dispatch_capabilities(&peer.capabilities);
    if !missing.is_empty() {
        return refuse_and_cache(
            base,
            &format!(
                "external agent is missing required delegation capabilities: {}",
                missing.join(", ")
            ),
        );
    }

    let checkpoint = if let Some(checkpoint) = cached_checkpoint {
        checkpoint
    } else if peer.capabilities.pre_dispatch_checkpoint {
        match agent.plan(&peer, &request, cancel_rx).await? {
            Some(checkpoint) => checkpoint,
            None => {
                return refuse_and_cache(
                    base,
                    "external agent did not return a pre-dispatch checkpoint",
                );
            }
        }
    } else if request.checkpoint.allow_local_fallback {
        synthesize_local_checkpoint(&request)
    } else {
        return refuse_and_cache(
            base,
            "external agent does not advertise pre-dispatch checkpoint support",
        );
    };

    base.checkpoint = Some(checkpoint.clone());
    if !request.checkpoint.approved {
        base.status = "checkpoint_required".to_string();
        cache_envelope(idempotency_key, &base);
        return Ok(base);
    }

    let result = agent
        .dispatch(&peer, &request, &checkpoint, cancel_rx)
        .await?;
    let budget_used = parse_budget_usage(&result);
    let exceeded = budget_used
        .as_ref()
        .is_some_and(|used| budget_exceeded(&request.budget, used));
    base.status = if exceeded {
        "budget_exceeded".to_string()
    } else {
        "completed".to_string()
    };
    if exceeded {
        base.error = Some("external agent reported usage over the approved budget cap".to_string());
    }
    base.budget_used = budget_used;
    base.result = Some(result.clone());
    base.receipts = extract_value_array(&result, &["receipts", "receipt_links"]);
    base.evidence_refs = extract_value_array(&result, &["evidence_refs", "evidence"]);
    let handoff = first_handoff_or_synthesize(&request, &peer, &checkpoint, &result);
    let handoff_json = serde_json::to_value(&handoff).unwrap_or(Value::Null);
    base.handoff = Some(handoff_json);
    base.artifacts = reviewable_artifacts(&handoff, &result);
    cache_envelope(idempotency_key, &base);
    Ok(base)
}

pub struct A2aExternalAgent;

#[async_trait]
impl ExternalAgent for A2aExternalAgent {
    async fn capabilities(
        &self,
        request: &ExternalAgentDelegationRequest,
        cancel_rx: &mut broadcast::Receiver<()>,
    ) -> Result<ExternalAgentPeer, ExternalAgentError> {
        let target = normalize_a2a_target(&request.target);
        let resolved =
            crate::a2a::resolve_agent(&target, request.allow_cleartext, cancel_rx).await?;
        Ok(peer_from_a2a(
            &request.target,
            resolved.endpoint,
            resolved.card,
        ))
    }

    async fn plan(
        &self,
        peer: &ExternalAgentPeer,
        request: &ExternalAgentDelegationRequest,
        cancel_rx: &mut broadcast::Receiver<()>,
    ) -> Result<Option<ExternalAgentPlanCheckpoint>, ExternalAgentError> {
        let request_id = format!(
            "{}.plan",
            request
                .idempotency_key
                .as_deref()
                .unwrap_or("external-agent")
        );
        let response = send_a2a_rpc(
            peer,
            request_id,
            A2A_PLAN_METHOD,
            serde_json::json!({
                "schema": EXTERNAL_AGENT_SCHEMA_ID,
                "target_agent": peer.target_agent,
                "task": request.task,
                "budget": request.budget,
                "idempotency_key": request.idempotency_key,
                "expected_scope": request.expected_scope,
                "context": request.context,
                "metadata": request.metadata,
            }),
            cancel_rx,
        )
        .await?;
        Ok(parse_remote_checkpoint(&response, request))
    }

    async fn dispatch(
        &self,
        peer: &ExternalAgentPeer,
        request: &ExternalAgentDelegationRequest,
        checkpoint: &ExternalAgentPlanCheckpoint,
        cancel_rx: &mut broadcast::Receiver<()>,
    ) -> Result<Value, ExternalAgentError> {
        let request_id = format!(
            "{}.dispatch",
            request
                .idempotency_key
                .as_deref()
                .unwrap_or("external-agent")
        );
        send_a2a_rpc(
            peer,
            request_id,
            A2A_DISPATCH_METHOD,
            serde_json::json!({
                "schema": EXTERNAL_AGENT_SCHEMA_ID,
                "target_agent": peer.target_agent,
                "task": request.task,
                "budget": request.budget,
                "idempotency_key": request.idempotency_key,
                "checkpoint": checkpoint,
                "expected_scope": request.expected_scope,
                "context": request.context,
                "metadata": request.metadata,
            }),
            cancel_rx,
        )
        .await
    }
}

fn validate_request(request: &ExternalAgentDelegationRequest) -> Result<(), ExternalAgentError> {
    if request.target.trim().is_empty() {
        return Err(ExternalAgentError::InvalidRequest(
            "external_agent_delegate: target is required".to_string(),
        ));
    }
    if request.task.trim().is_empty() {
        return Err(ExternalAgentError::InvalidRequest(
            "external_agent_delegate: task is required".to_string(),
        ));
    }
    let Some(idempotency_key) = request.idempotency_key.as_deref() else {
        return Err(ExternalAgentError::InvalidRequest(
            "external_agent_delegate: idempotency_key is required".to_string(),
        ));
    };
    if idempotency_key.trim().is_empty() {
        return Err(ExternalAgentError::InvalidRequest(
            "external_agent_delegate: idempotency_key is required".to_string(),
        ));
    }
    if !budget_has_cap(&request.budget) {
        return Err(ExternalAgentError::InvalidRequest(
            "external_agent_delegate: budget must include at least one positive cap".to_string(),
        ));
    }
    Ok(())
}

fn budget_has_cap(budget: &ExternalAgentBudget) -> bool {
    budget.max_usd.is_some_and(|value| value > 0.0)
        || budget.max_tokens.is_some_and(|value| value > 0)
        || budget.max_seconds.is_some_and(|value| value > 0)
        || budget.max_tool_calls.is_some_and(|value| value > 0)
}

fn cached_response(
    request: &ExternalAgentDelegationRequest,
    idempotency_key: &str,
) -> Option<ExternalAgentDelegationEnvelope> {
    let cached = idempotency_cache().get(idempotency_key).cloned()?;
    if cached.status == "checkpoint_required" && request.checkpoint.approved {
        return None;
    }
    if cached.status == "checkpoint_required" {
        return Some(cached);
    }
    Some(replay_envelope(cached))
}

fn cached_checkpoint(idempotency_key: &str) -> Option<ExternalAgentPlanCheckpoint> {
    idempotency_cache()
        .get(idempotency_key)
        .filter(|envelope| envelope.status == "checkpoint_required")
        .and_then(|envelope| envelope.checkpoint.clone())
}

fn cache_envelope(idempotency_key: &str, envelope: &ExternalAgentDelegationEnvelope) {
    idempotency_cache().insert(idempotency_key.to_string(), envelope.clone());
}

fn idempotency_cache(
) -> std::sync::MutexGuard<'static, HashMap<String, ExternalAgentDelegationEnvelope>> {
    IDEMPOTENCY_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn replay_envelope(original: ExternalAgentDelegationEnvelope) -> ExternalAgentDelegationEnvelope {
    let mut replayed = original.clone();
    replayed.id = new_id("external_delegate");
    replayed.status = "replayed".to_string();
    replayed.replayed = true;
    replayed.replay_of = Some(original.id);
    replayed
}

fn base_envelope(
    request: &ExternalAgentDelegationRequest,
    peer: &ExternalAgentPeer,
) -> ExternalAgentDelegationEnvelope {
    let mut metadata = request.metadata.clone();
    metadata.insert("card_url".to_string(), Value::String(peer.card_url.clone()));
    metadata.insert("rpc_url".to_string(), Value::String(peer.rpc_url.clone()));
    if let Some(agent_id) = peer.agent_id.as_ref() {
        metadata.insert("agent_id".to_string(), Value::String(agent_id.clone()));
    }
    ExternalAgentDelegationEnvelope {
        status: "ready".to_string(),
        transport: request.transport.clone(),
        target: request.target.clone(),
        allow_cleartext: request.allow_cleartext,
        task: request.task.clone(),
        budget: request.budget.clone(),
        idempotency_key: request.idempotency_key.clone(),
        capabilities: Some(peer.capabilities.clone()),
        metadata,
        ..ExternalAgentDelegationEnvelope::default()
    }
}

fn refuse_and_cache(
    mut envelope: ExternalAgentDelegationEnvelope,
    reason: &str,
) -> Result<ExternalAgentDelegationEnvelope, ExternalAgentError> {
    envelope.status = "refused".to_string();
    envelope.error = Some(reason.to_string());
    if let Some(key) = envelope.idempotency_key.clone() {
        cache_envelope(&key, &envelope);
    }
    Ok(envelope)
}

fn missing_dispatch_capabilities(capabilities: &ExternalAgentCapabilities) -> Vec<&'static str> {
    let mut missing = Vec::new();
    if !capabilities.dispatch {
        missing.push("dispatch");
    }
    if !capabilities.budget_cap {
        missing.push("budget_cap");
    }
    if !capabilities.idempotency {
        missing.push("idempotency");
    }
    if !capabilities.reviewable_handoff {
        missing.push("reviewable_handoff");
    }
    missing
}

fn peer_from_a2a(target: &str, endpoint: ResolvedA2aEndpoint, card: Value) -> ExternalAgentPeer {
    ExternalAgentPeer {
        transport: ExternalAgentTransport::A2a,
        target: target.to_string(),
        card_url: endpoint.card_url,
        rpc_url: endpoint.rpc_url,
        agent_id: endpoint.agent_id,
        target_agent: endpoint.target_agent,
        capabilities: capabilities_from_card(&card),
        card,
    }
}

fn capabilities_from_card(card: &Value) -> ExternalAgentCapabilities {
    let mut capabilities = ExternalAgentCapabilities::default();
    let raw = external_agent_metadata(card);
    if let Some(raw) = raw {
        capabilities.raw = Some(Value::Object(raw.clone()));
        capabilities.schema = string_field(raw, &["schema", "schema_id"])
            .or_else(|| Some(EXTERNAL_AGENT_SCHEMA_ID.to_string()));
        capabilities.pre_dispatch_checkpoint = bool_field(
            raw,
            &[
                "pre_dispatch_checkpoint",
                "preDispatchCheckpoint",
                "checkpoint",
            ],
        );
        capabilities.budget_cap = bool_field(raw, &["budget_cap", "budgetCap"]);
        capabilities.idempotency = bool_field(raw, &["idempotency", "idempotency_key"]);
        capabilities.reviewable_handoff =
            bool_field(raw, &["reviewable_handoff", "reviewableHandoff", "handoff"]);
        capabilities.dispatch = bool_field(raw, &["dispatch"]);
        capabilities.operations =
            strings_field(raw, &["operations", "methods", "extensionMethods"]);
    }

    for operation in extension_operations(card) {
        if !capabilities.operations.contains(&operation) {
            capabilities.operations.push(operation);
        }
    }
    if capabilities
        .operations
        .iter()
        .any(|op| op == A2A_PLAN_METHOD)
    {
        capabilities.pre_dispatch_checkpoint = true;
    }
    if capabilities
        .operations
        .iter()
        .any(|op| op == A2A_DISPATCH_METHOD)
    {
        capabilities.dispatch = true;
    }
    if card_contains_schema(card, EXTERNAL_AGENT_SCHEMA_ID) {
        capabilities.schema = Some(EXTERNAL_AGENT_SCHEMA_ID.to_string());
    }
    capabilities
}

fn external_agent_metadata(card: &Value) -> Option<&Map<String, Value>> {
    object_at(card, &["_meta", "harn", "externalAgent"])
        .or_else(|| object_at(card, &["_meta", "harn", "external_agent"]))
        .or_else(|| object_at(card, &["capabilities", "_meta", "harn", "externalAgent"]))
        .or_else(|| object_at(card, &["capabilities", "_meta", "harn", "external_agent"]))
        .or_else(|| object_at(card, &["capabilities", "externalAgent"]))
        .or_else(|| object_at(card, &["capabilities", "external_agent"]))
}

fn object_at<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Map<String, Value>> {
    let mut cursor = value;
    for key in path {
        cursor = cursor.get(*key)?;
    }
    cursor.as_object()
}

fn bool_field(object: &Map<String, Value>, keys: &[&str]) -> bool {
    keys.iter()
        .any(|key| object.get(*key).and_then(Value::as_bool).unwrap_or(false))
}

fn string_field(object: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        object
            .get(*key)
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|value| !value.is_empty())
    })
}

fn strings_field(object: &Map<String, Value>, keys: &[&str]) -> Vec<String> {
    keys.iter()
        .find_map(|key| value_to_strings(object.get(*key)?))
        .unwrap_or_default()
}

fn value_to_strings(value: &Value) -> Option<Vec<String>> {
    match value {
        Value::Array(items) => Some(
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .filter(|value| !value.is_empty())
                .collect(),
        ),
        Value::String(value) if !value.is_empty() => Some(vec![value.clone()]),
        _ => None,
    }
}

fn extension_operations(card: &Value) -> Vec<String> {
    let mut operations = Vec::new();
    for path in [
        &["capabilities", "extensions"][..],
        &["_meta", "harn", "extensions"][..],
        &["extensions"][..],
    ] {
        let Some(items) = value_at(card, path).and_then(Value::as_array) else {
            continue;
        };
        for item in items {
            match item {
                Value::String(value) if value == EXTERNAL_AGENT_SCHEMA_ID => {
                    operations.push(A2A_PLAN_METHOD.to_string());
                    operations.push(A2A_DISPATCH_METHOD.to_string());
                }
                Value::String(value)
                    if value == A2A_PLAN_METHOD || value == A2A_DISPATCH_METHOD =>
                {
                    operations.push(value.clone());
                }
                Value::Object(object) => {
                    if object
                        .values()
                        .filter_map(Value::as_str)
                        .any(|value| value == EXTERNAL_AGENT_SCHEMA_ID)
                    {
                        operations.push(A2A_PLAN_METHOD.to_string());
                        operations.push(A2A_DISPATCH_METHOD.to_string());
                    }
                    operations.extend(strings_field(object, &["operations", "methods"]));
                }
                _ => {}
            }
        }
    }
    operations.sort();
    operations.dedup();
    operations
}

fn value_at<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut cursor = value;
    for key in path {
        cursor = cursor.get(*key)?;
    }
    Some(cursor)
}

fn card_contains_schema(card: &Value, schema: &str) -> bool {
    match card {
        Value::String(value) => value == schema,
        Value::Array(items) => items
            .iter()
            .any(|value| card_contains_schema(value, schema)),
        Value::Object(object) => object
            .values()
            .any(|value| card_contains_schema(value, schema)),
        _ => false,
    }
}

fn normalize_a2a_target(target: &str) -> String {
    target
        .trim()
        .strip_prefix("a2a://")
        .unwrap_or_else(|| target.trim())
        .to_string()
}

async fn send_a2a_rpc(
    peer: &ExternalAgentPeer,
    request_id: String,
    method: &str,
    params: Value,
    cancel_rx: &mut broadcast::Receiver<()>,
) -> Result<Value, ExternalAgentError> {
    let request = crate::jsonrpc::request(request_id.clone(), method, params);
    let body =
        crate::a2a::send_jsonrpc_request(&peer.rpc_url, &request, &request_id, cancel_rx).await?;
    if let Some(error) = body.get("error") {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown external agent error");
        return Err(ExternalAgentError::Protocol(format!(
            "{method} failed: {message}"
        )));
    }
    body.get("result")
        .cloned()
        .ok_or_else(|| ExternalAgentError::Protocol(format!("{method} response missing result")))
}

fn parse_remote_checkpoint(
    result: &Value,
    request: &ExternalAgentDelegationRequest,
) -> Option<ExternalAgentPlanCheckpoint> {
    let checkpoint_value = result.get("checkpoint").unwrap_or(result);
    let plan = checkpoint_value
        .get("plan")
        .or_else(|| checkpoint_value.get("summary"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    let expected_scope = string_array_field(checkpoint_value, &["expected_scope", "scope"])
        .filter(|items| !items.is_empty())
        .unwrap_or_else(|| request.expected_scope.clone());
    let checkpoint_id = checkpoint_value
        .get("checkpoint_id")
        .or_else(|| checkpoint_value.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| new_id("external_checkpoint"));
    let evidence_refs = extract_value_array(checkpoint_value, &["evidence_refs", "evidence"]);
    let metadata = checkpoint_value
        .get("metadata")
        .and_then(Value::as_object)
        .map(|object| {
            object
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect()
        })
        .unwrap_or_default();
    Some(ExternalAgentPlanCheckpoint {
        checkpoint_id,
        plan: plan.to_string(),
        expected_scope,
        budget: request.budget.clone(),
        evidence_refs,
        metadata,
        ..ExternalAgentPlanCheckpoint::default()
    })
}

fn synthesize_local_checkpoint(
    request: &ExternalAgentDelegationRequest,
) -> ExternalAgentPlanCheckpoint {
    let plan = request.checkpoint.local_plan.clone().unwrap_or_else(|| {
        let scope = if request.expected_scope.is_empty() {
            "Remote agent must state any files or entities before mutating them.".to_string()
        } else {
            format!(
                "Remote agent may work only within: {}.",
                request.expected_scope.join(", ")
            )
        };
        format!(
            "Delegate task after local approval. Task: {} {scope}",
            request.task
        )
    });
    ExternalAgentPlanCheckpoint {
        source: "local_fallback".to_string(),
        plan,
        expected_scope: request.expected_scope.clone(),
        budget: request.budget.clone(),
        ..ExternalAgentPlanCheckpoint::default()
    }
}

fn parse_budget_usage(result: &Value) -> Option<ExternalAgentBudgetUsage> {
    let value = result
        .get("budget_used")
        .or_else(|| result.pointer("/budget/used"))
        .or_else(|| result.get("usage"))?;
    serde_json::from_value(value.clone()).ok()
}

fn budget_exceeded(budget: &ExternalAgentBudget, used: &ExternalAgentBudgetUsage) -> bool {
    budget
        .max_usd
        .zip(used.usd)
        .is_some_and(|(cap, used)| used > cap)
        || budget
            .max_tokens
            .zip(used.tokens)
            .is_some_and(|(cap, used)| used > cap)
        || budget
            .max_seconds
            .zip(used.seconds)
            .is_some_and(|(cap, used)| used > cap)
        || budget
            .max_tool_calls
            .zip(used.tool_calls)
            .is_some_and(|(cap, used)| used > cap)
}

fn first_handoff_or_synthesize(
    request: &ExternalAgentDelegationRequest,
    peer: &ExternalAgentPeer,
    checkpoint: &ExternalAgentPlanCheckpoint,
    result: &Value,
) -> HandoffArtifact {
    extract_handoffs_from_json_value(result)
        .into_iter()
        .next()
        .unwrap_or_else(|| synthesize_handoff(request, peer, checkpoint, result))
}

fn synthesize_handoff(
    request: &ExternalAgentDelegationRequest,
    peer: &ExternalAgentPeer,
    checkpoint: &ExternalAgentPlanCheckpoint,
    result: &Value,
) -> HandoffArtifact {
    let files = string_array_field(result, &["files_or_entities_touched", "files", "paths"])
        .filter(|items| !items.is_empty())
        .unwrap_or_else(|| checkpoint.expected_scope.clone());
    let confidence = result.get("confidence").and_then(Value::as_f64);
    let mut metadata = BTreeMap::new();
    metadata.insert(
        "schema".to_string(),
        Value::String(EXTERNAL_AGENT_HANDOFF_SCHEMA_ID.to_string()),
    );
    metadata.insert("card_url".to_string(), Value::String(peer.card_url.clone()));
    metadata.insert("rpc_url".to_string(), Value::String(peer.rpc_url.clone()));
    metadata.insert(
        "checkpoint_id".to_string(),
        Value::String(checkpoint.checkpoint_id.clone()),
    );
    if let Some(key) = request.idempotency_key.as_ref() {
        metadata.insert("idempotency_key".to_string(), Value::String(key.clone()));
    }
    HandoffArtifact {
        type_name: "handoff_artifact".to_string(),
        kind: "external_agent_delegation".to_string(),
        id: new_id("handoff"),
        source_persona: "external_agent".to_string(),
        target_persona_or_human: HandoffTargetRecord {
            kind: "a2a".to_string(),
            id: peer.agent_id.clone().or_else(|| Some(peer.target.clone())),
            label: Some(peer.target_agent.clone()).filter(|value| !value.is_empty()),
            uri: Some(peer.card_url.clone()),
        },
        task: request.task.clone(),
        reason: "External agent returned delegated work for review.".to_string(),
        files_or_entities_touched: files,
        requested_capabilities: peer.capabilities.operations.clone(),
        allowed_side_effects: vec![
            "reviewable_handoff".to_string(),
            "reviewable_diff".to_string(),
        ],
        budget_remaining: budget_remaining(&request.budget, parse_budget_usage(result).as_ref()),
        confidence,
        metadata,
        ..HandoffArtifact::default()
    }
    .normalize()
}

fn budget_remaining(
    budget: &ExternalAgentBudget,
    used: Option<&ExternalAgentBudgetUsage>,
) -> Option<HandoffBudgetRemainingRecord> {
    let used = used?;
    Some(HandoffBudgetRemainingRecord {
        tokens: budget
            .max_tokens
            .zip(used.tokens)
            .map(|(cap, used)| cap as i64 - used as i64),
        tool_calls: budget
            .max_tool_calls
            .zip(used.tool_calls)
            .map(|(cap, used)| cap as i64 - used as i64),
        dollars: budget.max_usd.zip(used.usd).map(|(cap, used)| cap - used),
    })
}

fn reviewable_artifacts(handoff: &HandoffArtifact, result: &Value) -> Vec<Value> {
    let mut artifacts = Vec::new();
    artifacts
        .push(serde_json::to_value(handoff_artifact_record(handoff, None)).unwrap_or(Value::Null));
    artifacts.extend(
        result
            .get("artifacts")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .cloned(),
    );
    if let Some(diff) = result
        .get("diff")
        .or_else(|| result.get("patch"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        let artifact = ArtifactRecord {
            kind: "diff".to_string(),
            title: Some("External agent diff".to_string()),
            text: Some(diff.to_string()),
            data: Some(serde_json::json!({
                "format": "unified",
                "diff": diff,
            })),
            source: Some("external_agent".to_string()),
            freshness: Some("fresh".to_string()),
            relevance: handoff.confidence,
            ..ArtifactRecord::default()
        }
        .normalize();
        artifacts.push(serde_json::to_value(artifact).unwrap_or(Value::Null));
    }
    artifacts
}

fn extract_value_array(value: &Value, keys: &[&str]) -> Vec<Value> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_array))
        .cloned()
        .unwrap_or_default()
}

fn string_array_field(value: &Value, keys: &[&str]) -> Option<Vec<String>> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(value_to_strings))
}

fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::now_v7())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::thread;

    struct MockA2aServer {
        addr: String,
        state: Arc<MockState>,
        shutdown: Arc<AtomicBool>,
        handle: Option<thread::JoinHandle<()>>,
    }

    struct MockState {
        card: Value,
        plan_response: Value,
        dispatch_response: Value,
        plan_count: AtomicUsize,
        dispatch_count: AtomicUsize,
        requests: Mutex<Vec<Value>>,
    }

    impl MockA2aServer {
        fn new(card_capabilities: Value, plan_response: Value, dispatch_response: Value) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock A2A server");
            let addr = listener.local_addr().expect("mock A2A addr");
            let url = format!("http://localhost:{}/rpc", addr.port());
            let card = serde_json::json!({
                "name": "mock external agent",
                "id": "mock-agent",
                "protocolVersion": "0.3.0",
                "url": url,
                "preferredTransport": "JSONRPC",
                "capabilities": card_capabilities,
            });
            let state = Arc::new(MockState {
                card,
                plan_response,
                dispatch_response,
                plan_count: AtomicUsize::new(0),
                dispatch_count: AtomicUsize::new(0),
                requests: Mutex::new(Vec::new()),
            });
            let shutdown = Arc::new(AtomicBool::new(false));
            let thread_state = Arc::clone(&state);
            let thread_shutdown = Arc::clone(&shutdown);
            let handle = thread::spawn(move || {
                while !thread_shutdown.load(Ordering::SeqCst) {
                    let Ok((mut stream, _)) = listener.accept() else {
                        break;
                    };
                    if thread_shutdown.load(Ordering::SeqCst) {
                        break;
                    }
                    handle_connection(&mut stream, &thread_state);
                }
            });
            Self {
                addr: format!("127.0.0.1:{}", addr.port()),
                state,
                shutdown,
                handle: Some(handle),
            }
        }

        fn target(&self) -> String {
            self.addr.clone()
        }

        fn plan_count(&self) -> usize {
            self.state.plan_count.load(Ordering::SeqCst)
        }

        fn dispatch_count(&self) -> usize {
            self.state.dispatch_count.load(Ordering::SeqCst)
        }

        fn requests(&self) -> Vec<Value> {
            self.state
                .requests
                .lock()
                .expect("mock request log poisoned")
                .clone()
        }
    }

    impl Drop for MockA2aServer {
        fn drop(&mut self) {
            self.shutdown.store(true, Ordering::SeqCst);
            let _ = TcpStream::connect(&self.addr);
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    fn handle_connection(stream: &mut TcpStream, state: &MockState) {
        let Some((method, path, body)) = read_request(stream) else {
            return;
        };
        if method == "GET" && path == "/.well-known/agent-card.json" {
            write_response(stream, 200, &state.card);
            return;
        }
        if method == "POST" && path == "/rpc" {
            let request: Value =
                serde_json::from_slice(&body).expect("parse mock JSON-RPC request");
            state
                .requests
                .lock()
                .expect("mock request log poisoned")
                .push(request.clone());
            let response = match request.get("method").and_then(Value::as_str) {
                Some(A2A_PLAN_METHOD) => {
                    state.plan_count.fetch_add(1, Ordering::SeqCst);
                    state.plan_response.clone()
                }
                Some(A2A_DISPATCH_METHOD) => {
                    state.dispatch_count.fetch_add(1, Ordering::SeqCst);
                    state.dispatch_response.clone()
                }
                Some(method) => serde_json::json!({
                    "error": {"code": -32601, "message": format!("unknown method {method}")},
                }),
                None => serde_json::json!({
                    "error": {"code": -32600, "message": "missing method"},
                }),
            };
            let body = if response.get("error").is_some() {
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": request.get("id").cloned().unwrap_or(Value::Null),
                    "error": response["error"].clone(),
                })
            } else {
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": request.get("id").cloned().unwrap_or(Value::Null),
                    "result": response,
                })
            };
            write_response(stream, 200, &body);
            return;
        }
        write_response(stream, 404, &serde_json::json!({"error": "not found"}));
    }

    fn read_request(stream: &mut TcpStream) -> Option<(String, String, Vec<u8>)> {
        let mut first = [0_u8; 1];
        stream.read_exact(&mut first).ok()?;
        if first[0] != b'G' && first[0] != b'P' {
            return None;
        }
        let mut header = vec![first[0]];
        while !header.ends_with(b"\r\n\r\n") {
            let mut byte = [0_u8; 1];
            stream.read_exact(&mut byte).ok()?;
            header.push(byte[0]);
        }
        let header_text = String::from_utf8_lossy(&header);
        let mut lines = header_text.lines();
        let request_line = lines.next()?;
        let mut parts = request_line.split_whitespace();
        let method = parts.next()?.to_string();
        let path = parts.next()?.to_string();
        let content_length = lines
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .and_then(|(_, value)| value.trim().parse::<usize>().ok())
            .unwrap_or(0);
        let mut body = vec![0_u8; content_length];
        if content_length > 0 {
            stream.read_exact(&mut body).ok()?;
        }
        Some((method, path, body))
    }

    fn write_response(stream: &mut TcpStream, status: u16, body: &Value) {
        let body = serde_json::to_vec(body).expect("serialize mock response");
        let status_text = if status == 200 { "OK" } else { "Not Found" };
        let header = format!(
            "HTTP/1.1 {status} {status_text}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        );
        stream
            .write_all(header.as_bytes())
            .expect("write response header");
        stream.write_all(&body).expect("write response body");
        stream.flush().expect("flush response");
    }

    fn full_capabilities() -> Value {
        serde_json::json!({
            "externalAgent": {
                "schema": EXTERNAL_AGENT_SCHEMA_ID,
                "pre_dispatch_checkpoint": true,
                "budget_cap": true,
                "idempotency": true,
                "reviewable_handoff": true,
                "dispatch": true,
                "operations": [A2A_PLAN_METHOD, A2A_DISPATCH_METHOD],
            },
        })
    }

    fn dispatch_only_capabilities() -> Value {
        serde_json::json!({
            "externalAgent": {
                "schema": EXTERNAL_AGENT_SCHEMA_ID,
                "budget_cap": true,
                "idempotency": true,
                "reviewable_handoff": true,
                "dispatch": true,
                "operations": [A2A_DISPATCH_METHOD],
            },
        })
    }

    fn request(
        server: &MockA2aServer,
        approved: bool,
        key: &str,
    ) -> ExternalAgentDelegationRequest {
        ExternalAgentDelegationRequest {
            target: server.target(),
            allow_cleartext: true,
            task: "edit src/lib.rs".to_string(),
            budget: ExternalAgentBudget {
                max_usd: Some(0.05),
                max_tokens: Some(500),
                ..ExternalAgentBudget::default()
            },
            checkpoint: ExternalAgentCheckpointPolicy {
                approved,
                allow_local_fallback: false,
                ..ExternalAgentCheckpointPolicy::default()
            },
            idempotency_key: Some(key.to_string()),
            expected_scope: vec!["src/lib.rs".to_string()],
            ..ExternalAgentDelegationRequest::default()
        }
    }

    fn cancel_channel() -> (broadcast::Sender<()>, broadcast::Receiver<()>) {
        broadcast::channel(1)
    }

    #[tokio::test]
    async fn checkpoint_is_required_before_dispatch_then_replayed() {
        reset_external_agent_state();
        let server = MockA2aServer::new(
            full_capabilities(),
            serde_json::json!({
                "checkpoint_id": "chk_1",
                "plan": "Inspect src/lib.rs, edit only the requested area, and return a reviewable diff.",
                "expected_scope": ["src/lib.rs"],
            }),
            serde_json::json!({
                "status": "completed",
                "diff": "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@\n-ok\n+better\n",
                "budget_used": {"usd": 0.02, "tokens": 120},
                "confidence": 0.91,
            }),
        );

        let (_tx, mut rx) = cancel_channel();
        let checkpoint = delegate_external_agent(request(&server, false, "idem-1"), &mut rx)
            .await
            .expect("checkpoint response");
        assert_eq!(checkpoint.status, "checkpoint_required");
        assert!(checkpoint.allow_cleartext);
        assert_eq!(server.plan_count(), 1);
        assert_eq!(server.dispatch_count(), 0);

        let (_tx, mut rx) = cancel_channel();
        let completed = delegate_external_agent(request(&server, true, "idem-1"), &mut rx)
            .await
            .expect("approved dispatch");
        assert_eq!(completed.status, "completed");
        assert_eq!(
            completed.budget_used.as_ref().and_then(|used| used.tokens),
            Some(120)
        );
        assert_eq!(server.plan_count(), 1);
        assert_eq!(server.dispatch_count(), 1);
        assert!(completed.handoff.is_some());
        assert!(completed
            .artifacts
            .iter()
            .any(|artifact| { artifact.get("kind").and_then(Value::as_str) == Some("diff") }));

        let (_tx, mut rx) = cancel_channel();
        let replayed = delegate_external_agent(request(&server, true, "idem-1"), &mut rx)
            .await
            .expect("idempotent replay");
        assert_eq!(replayed.status, "replayed");
        assert!(replayed.replayed);
        assert_eq!(server.dispatch_count(), 1);

        let dispatch_request = server
            .requests()
            .into_iter()
            .find(|request| {
                request.get("method").and_then(Value::as_str) == Some(A2A_DISPATCH_METHOD)
            })
            .expect("dispatch request recorded");
        assert_eq!(
            dispatch_request
                .pointer("/params/budget/max_usd")
                .and_then(Value::as_f64),
            Some(0.05)
        );
        assert_eq!(
            dispatch_request
                .pointer("/params/idempotency_key")
                .and_then(Value::as_str),
            Some("idem-1")
        );
    }

    #[tokio::test]
    async fn refuses_when_checkpoint_capability_is_missing() {
        reset_external_agent_state();
        let server = MockA2aServer::new(
            dispatch_only_capabilities(),
            serde_json::json!({"plan": "should not be called"}),
            serde_json::json!({"status": "completed"}),
        );

        let (_tx, mut rx) = cancel_channel();
        let envelope = delegate_external_agent(request(&server, false, "idem-2"), &mut rx)
            .await
            .expect("refusal envelope");
        assert_eq!(envelope.status, "refused");
        assert!(envelope
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("pre-dispatch checkpoint"));
        assert_eq!(server.plan_count(), 0);
        assert_eq!(server.dispatch_count(), 0);
    }

    #[tokio::test]
    async fn refuses_before_plan_when_dispatch_capabilities_are_missing() {
        reset_external_agent_state();
        let server = MockA2aServer::new(
            serde_json::json!({
                "externalAgent": {
                    "pre_dispatch_checkpoint": true,
                    "dispatch": true,
                    "operations": [A2A_PLAN_METHOD, A2A_DISPATCH_METHOD],
                },
            }),
            serde_json::json!({"plan": "should not be called"}),
            serde_json::json!({"status": "completed"}),
        );

        let (_tx, mut rx) = cancel_channel();
        let envelope =
            delegate_external_agent(request(&server, false, "idem-missing-caps"), &mut rx)
                .await
                .expect("refusal envelope");
        assert_eq!(envelope.status, "refused");
        assert!(envelope
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("budget_cap"));
        assert_eq!(server.plan_count(), 0);
        assert_eq!(server.dispatch_count(), 0);
    }

    #[tokio::test]
    async fn local_checkpoint_fallback_still_requires_explicit_approval() {
        reset_external_agent_state();
        let server = MockA2aServer::new(
            dispatch_only_capabilities(),
            serde_json::json!({"plan": "should not be called"}),
            serde_json::json!({
                "status": "completed",
                "budget_used": {"usd": 0.01},
                "files": ["src/lib.rs"],
            }),
        );
        let mut unapproved = request(&server, false, "idem-3");
        unapproved.checkpoint.allow_local_fallback = true;
        unapproved.checkpoint.local_plan = Some("Local plan approved by host.".to_string());

        let (_tx, mut rx) = cancel_channel();
        let checkpoint = delegate_external_agent(unapproved.clone(), &mut rx)
            .await
            .expect("local checkpoint");
        assert_eq!(checkpoint.status, "checkpoint_required");
        assert_eq!(
            checkpoint
                .checkpoint
                .as_ref()
                .map(|value| value.source.as_str()),
            Some("local_fallback")
        );
        assert_eq!(server.plan_count(), 0);
        assert_eq!(server.dispatch_count(), 0);

        let mut approved = unapproved;
        approved.checkpoint.approved = true;
        let (_tx, mut rx) = cancel_channel();
        let completed = delegate_external_agent(approved, &mut rx)
            .await
            .expect("approved local fallback dispatch");
        assert_eq!(completed.status, "completed");
        assert_eq!(server.plan_count(), 0);
        assert_eq!(server.dispatch_count(), 1);
    }

    #[tokio::test]
    async fn over_budget_result_is_reviewable_but_marked() {
        reset_external_agent_state();
        let server = MockA2aServer::new(
            full_capabilities(),
            serde_json::json!({"checkpoint_id": "chk_budget", "plan": "Spend less than cap."}),
            serde_json::json!({
                "status": "completed",
                "diff": "diff --git a/file b/file\n",
                "budget_used": {"usd": 0.10, "tokens": 100},
            }),
        );
        let mut first = request(&server, false, "idem-4");
        first.budget.max_usd = Some(0.05);
        let (_tx, mut rx) = cancel_channel();
        delegate_external_agent(first, &mut rx)
            .await
            .expect("checkpoint");

        let mut approved = request(&server, true, "idem-4");
        approved.budget.max_usd = Some(0.05);
        let (_tx, mut rx) = cancel_channel();
        let envelope = delegate_external_agent(approved, &mut rx)
            .await
            .expect("budget-marked envelope");
        assert_eq!(envelope.status, "budget_exceeded");
        assert!(envelope
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("budget"));
        assert!(envelope.handoff.is_some());
        assert_eq!(server.dispatch_count(), 1);
    }
}
