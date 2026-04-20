use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration as StdDuration;

use async_trait::async_trait;
use reqwest::StatusCode;
use serde::Deserialize;
use serde_json::{json, Map as JsonMap, Value as JsonValue};
use time::{Duration, OffsetDateTime};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::connectors::{
    ActivationHandle, ClientError, Connector, ConnectorClient, ConnectorCtx, ConnectorError,
    HmacSignatureStyle, ProviderPayloadSchema, RawInbound, TriggerBinding, TriggerKind,
};
use crate::secrets::{SecretId, SecretVersion};
use crate::triggers::{
    redact_headers, HeaderRedactionPolicy, ProviderId, ProviderPayload, SignatureStatus, TraceId,
    TriggerEvent, TriggerEventId,
};

#[cfg(test)]
mod tests;

pub const LINEAR_PROVIDER_ID: &str = "linear";
const DEFAULT_API_BASE_URL: &str = "https://api.linear.app/graphql";
const DEFAULT_REPLAY_WINDOW_SECS: i64 = 60;
const DEFAULT_REPLAY_GRACE_SECS: i64 = 15;
const DEFAULT_WEBHOOK_MONITOR_PROBE_INTERVAL_SECS: u64 = 60;
const DEFAULT_WEBHOOK_MONITOR_SUCCESS_THRESHOLD: u32 = 5;
const COMPLEXITY_WARNING_THRESHOLD: i64 = 5_000;

pub struct LinearConnector {
    provider_id: ProviderId,
    kinds: Vec<TriggerKind>,
    state: Arc<LinearConnectorState>,
    client: Arc<LinearClient>,
}

#[derive(Default)]
struct LinearConnectorState {
    ctx: RwLock<Option<ConnectorCtx>>,
    bindings: RwLock<HashMap<String, ActivatedLinearBinding>>,
    monitor_tasks: Mutex<Vec<JoinHandle<()>>>,
    monitor_shutdown: Mutex<Option<watch::Sender<bool>>>,
}

#[derive(Clone, Debug)]
struct ActivatedLinearBinding {
    #[allow(dead_code)]
    binding_id: String,
    path: Option<String>,
    signing_secret: SecretId,
    replay_grace_secs: i64,
    monitor: Option<LinearWebhookMonitor>,
}

struct LinearClient {
    provider_id: ProviderId,
    state: Arc<LinearConnectorState>,
    http: reqwest::Client,
}

#[derive(Debug, Default, Deserialize)]
struct LinearBindingConfig {
    #[serde(default, rename = "match")]
    match_config: LinearMatchConfig,
    #[serde(default)]
    secrets: LinearSecretsConfig,
    #[serde(default)]
    security: LinearSecurityConfig,
    #[serde(default)]
    replay_grace_secs: Option<i64>,
    #[serde(default)]
    monitor: Option<LinearWebhookMonitorConfig>,
}

#[derive(Debug, Default, Deserialize)]
struct LinearMatchConfig {
    path: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct LinearSecretsConfig {
    signing_secret: Option<String>,
    access_token: Option<String>,
    api_key: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct LinearSecurityConfig {
    replay_grace_secs: Option<i64>,
    timestamp_grace_secs: Option<i64>,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct LinearClientConfigArgs {
    api_base_url: Option<String>,
    api_key: Option<String>,
    api_key_secret: Option<String>,
    access_token: Option<String>,
    access_token_secret: Option<String>,
    #[serde(default)]
    secrets: LinearSecretsConfig,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct LinearWebhookMonitorConfig {
    #[serde(default)]
    enabled: Option<bool>,
    webhook_id: Option<String>,
    health_url: Option<String>,
    #[serde(default)]
    probe_interval_secs: Option<u64>,
    #[serde(default)]
    success_threshold: Option<u32>,
    #[serde(flatten)]
    client: LinearClientConfigArgs,
}

#[derive(Clone, Debug)]
struct ResolvedLinearClientConfig {
    api_base_url: String,
    auth: LinearAuthSource,
}

#[derive(Clone, Debug)]
enum LinearAuthSource {
    ApiKeyInline(String),
    ApiKeySecret(SecretId),
    AccessTokenInline(String),
    AccessTokenSecret(SecretId),
}

#[derive(Clone, Debug)]
struct LinearWebhookMonitor {
    webhook_id: String,
    health_url: String,
    probe_interval_secs: u64,
    success_threshold: u32,
    client_config: ResolvedLinearClientConfig,
}

#[derive(Debug, Deserialize)]
struct ListIssuesArgs {
    #[serde(flatten)]
    config: LinearClientConfigArgs,
    #[serde(default)]
    filter: Option<JsonValue>,
    #[serde(default)]
    first: Option<i64>,
    #[serde(default)]
    after: Option<String>,
    #[serde(default)]
    include_archived: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct UpdateIssueArgs {
    #[serde(flatten)]
    config: LinearClientConfigArgs,
    id: String,
    changes: JsonValue,
}

#[derive(Debug, Deserialize)]
struct CreateCommentArgs {
    #[serde(flatten)]
    config: LinearClientConfigArgs,
    issue_id: String,
    body: String,
}

#[derive(Debug, Deserialize)]
struct SearchArgs {
    #[serde(flatten)]
    config: LinearClientConfigArgs,
    query: String,
    #[serde(default)]
    first: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct GraphqlArgs {
    #[serde(flatten)]
    config: LinearClientConfigArgs,
    query: String,
    #[serde(default)]
    variables: Option<JsonValue>,
    #[serde(default)]
    operation_name: Option<String>,
}

impl LinearConnector {
    pub fn new() -> Self {
        let state = Arc::new(LinearConnectorState::default());
        let client = Arc::new(LinearClient {
            provider_id: ProviderId::from(LINEAR_PROVIDER_ID),
            state: state.clone(),
            http: reqwest::Client::builder()
                .user_agent("harn-linear-connector")
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
        });
        Self {
            provider_id: ProviderId::from(LINEAR_PROVIDER_ID),
            kinds: vec![TriggerKind::from("webhook")],
            state,
            client,
        }
    }

    fn binding_for_raw(&self, raw: &RawInbound) -> Result<ActivatedLinearBinding, ConnectorError> {
        let bindings = self
            .state
            .bindings
            .read()
            .expect("linear connector bindings poisoned");
        if let Some(binding_id) = raw.metadata.get("binding_id").and_then(JsonValue::as_str) {
            return bindings.get(binding_id).cloned().ok_or_else(|| {
                ConnectorError::Unsupported(format!(
                    "linear connector has no active binding `{binding_id}`"
                ))
            });
        }
        if bindings.len() == 1 {
            return bindings
                .values()
                .next()
                .cloned()
                .ok_or_else(|| ConnectorError::Activation("linear bindings missing".to_string()));
        }
        Err(ConnectorError::Unsupported(
            "linear connector requires raw.metadata.binding_id when multiple bindings are active"
                .to_string(),
        ))
    }

    fn ctx(&self) -> Result<ConnectorCtx, ConnectorError> {
        self.state
            .ctx
            .read()
            .expect("linear connector ctx poisoned")
            .clone()
            .ok_or_else(|| {
                ConnectorError::Activation(
                    "linear connector must be initialized before use".to_string(),
                )
            })
    }

    fn stop_monitors(&self) {
        if let Some(shutdown) = self
            .state
            .monitor_shutdown
            .lock()
            .expect("linear connector monitor shutdown poisoned")
            .take()
        {
            let _ = shutdown.send(true);
        }
        let mut tasks = self
            .state
            .monitor_tasks
            .lock()
            .expect("linear connector monitor tasks poisoned");
        for task in tasks.drain(..) {
            task.abort();
        }
    }
}

impl Default for LinearConnector {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for LinearConnector {
    fn drop(&mut self) {
        self.stop_monitors();
    }
}

#[async_trait]
impl Connector for LinearConnector {
    fn provider_id(&self) -> &ProviderId {
        &self.provider_id
    }

    fn kinds(&self) -> &[TriggerKind] {
        &self.kinds
    }

    async fn init(&mut self, ctx: ConnectorCtx) -> Result<(), ConnectorError> {
        *self
            .state
            .ctx
            .write()
            .expect("linear connector ctx poisoned") = Some(ctx);
        Ok(())
    }

    async fn activate(
        &self,
        bindings: &[TriggerBinding],
    ) -> Result<ActivationHandle, ConnectorError> {
        self.stop_monitors();
        let mut configured = HashMap::new();
        let mut paths = BTreeSet::new();
        for binding in bindings {
            let activated = ActivatedLinearBinding::from_binding(binding)?;
            if let Some(path) = &activated.path {
                if !paths.insert(path.clone()) {
                    return Err(ConnectorError::Activation(format!(
                        "linear connector path `{path}` is configured by multiple bindings"
                    )));
                }
            }
            configured.insert(binding.binding_id.clone(), activated);
        }
        let (monitor_shutdown, _) = watch::channel(false);
        *self
            .state
            .bindings
            .write()
            .expect("linear connector bindings poisoned") = configured;
        *self
            .state
            .monitor_shutdown
            .lock()
            .expect("linear connector monitor shutdown poisoned") = Some(monitor_shutdown.clone());
        let tasks = {
            let bindings = self
                .state
                .bindings
                .read()
                .expect("linear connector bindings poisoned");
            bindings
                .values()
                .filter_map(|binding| {
                    binding.monitor.clone().map(|monitor| {
                        let client = self.client.clone();
                        let shutdown = monitor_shutdown.subscribe();
                        tokio::spawn(async move {
                            run_webhook_monitor(client, monitor, shutdown).await;
                        })
                    })
                })
                .collect::<Vec<_>>()
        };
        *self
            .state
            .monitor_tasks
            .lock()
            .expect("linear connector monitor tasks poisoned") = tasks;
        Ok(ActivationHandle::new(
            self.provider_id.clone(),
            bindings.len(),
        ))
    }

    async fn shutdown(&self, deadline: StdDuration) -> Result<(), ConnectorError> {
        if let Some(shutdown) = self
            .state
            .monitor_shutdown
            .lock()
            .expect("linear connector monitor shutdown poisoned")
            .take()
        {
            let _ = shutdown.send(true);
        }
        let pending = self
            .state
            .monitor_tasks
            .lock()
            .expect("linear connector monitor tasks poisoned")
            .drain(..)
            .collect::<Vec<_>>();
        if pending.is_empty() {
            return Ok(());
        }
        let wait_all = async {
            for task in pending {
                let _ = task.await;
            }
        };
        tokio::time::timeout(deadline, wait_all)
            .await
            .map_err(|_| {
                ConnectorError::Activation(format!(
                    "linear connector shutdown exceeded {}s",
                    deadline.as_secs()
                ))
            })?;
        Ok(())
    }

    async fn normalize_inbound(&self, raw: RawInbound) -> Result<TriggerEvent, ConnectorError> {
        let ctx = self.ctx()?;
        let binding = self.binding_for_raw(&raw)?;
        let provider = self.provider_id.clone();
        let received_at = raw.received_at;
        let headers = effective_headers(&raw.headers);
        let secret = load_secret_text_blocking(&ctx, &binding.signing_secret)?;
        let timestamp_window =
            Duration::seconds(DEFAULT_REPLAY_WINDOW_SECS + binding.replay_grace_secs.max(0));
        let verify = futures::executor::block_on(crate::connectors::hmac::verify_hmac_signed(
            ctx.event_log.as_ref(),
            &provider,
            HmacSignatureStyle::linear(),
            &raw.body,
            &headers,
            secret.as_str(),
            Some(timestamp_window),
            received_at,
        ));
        if let Err(error) = verify {
            if matches!(error, ConnectorError::TimestampOutOfWindow { .. }) {
                ctx.metrics.record_linear_timestamp_rejection();
            }
            return Err(error);
        }

        let payload = raw.json_body()?;
        let dedupe_key = header_value(&headers, "linear-delivery")
            .map(ToString::to_string)
            .or_else(|| {
                payload
                    .get("webhookId")
                    .and_then(JsonValue::as_str)
                    .map(ToString::to_string)
            })
            .unwrap_or_else(|| fallback_body_digest(&raw.body));
        let kind = linear_trigger_kind(&payload);
        let provider_payload = ProviderPayload::normalize(&provider, &kind, &headers, payload)
            .map_err(|error| ConnectorError::Unsupported(error.to_string()))?;
        Ok(TriggerEvent {
            id: TriggerEventId::new(),
            provider,
            kind,
            received_at,
            occurred_at: raw
                .occurred_at
                .or_else(|| infer_occurred_at(&provider_payload)),
            dedupe_key,
            trace_id: TraceId::new(),
            tenant_id: raw.tenant_id.clone(),
            headers: redact_headers(&headers, &HeaderRedactionPolicy::default()),
            batch: None,
            raw_body: Some(raw.body.clone()),
            provider_payload,
            signature_status: SignatureStatus::Verified,
            dedupe_claimed: false,
        })
    }

    fn payload_schema(&self) -> ProviderPayloadSchema {
        ProviderPayloadSchema::named("LinearEventPayload")
    }

    fn client(&self) -> Arc<dyn ConnectorClient> {
        self.client.clone()
    }
}

#[async_trait]
impl ConnectorClient for LinearClient {
    async fn call(&self, method: &str, args: JsonValue) -> Result<JsonValue, ClientError> {
        match method {
            "list_issues" => {
                let args: ListIssuesArgs = parse_args(args)?;
                let config = self.resolve_client_config(&args.config)?;
                let envelope = self
                    .request_graphql(
                        &config,
                        "query ListIssues($filter: IssueFilter, $first: Int, $after: String, $includeArchived: Boolean) { issues(filter: $filter, first: $first, after: $after, includeArchived: $includeArchived) { nodes { id identifier title priority estimate dueDate url createdAt updatedAt state { id name type } team { id key name } assignee { id name } project { id name } cycle { id name } labels { nodes { id name } } } pageInfo { hasNextPage endCursor } } }",
                        json!({
                            "filter": args.filter,
                            "first": args.first,
                            "after": args.after,
                            "includeArchived": args.include_archived,
                        }),
                        Some("ListIssues"),
                    )
                    .await?;
                extract_graphql_field(envelope, "issues")
            }
            "update_issue" => {
                let args: UpdateIssueArgs = parse_args(args)?;
                let config = self.resolve_client_config(&args.config)?;
                let envelope = self
                    .request_graphql(
                        &config,
                        "mutation UpdateIssue($id: String!, $input: IssueUpdateInput!) { issueUpdate(id: $id, input: $input) { success issue { id identifier title priority estimate dueDate url updatedAt state { id name type } assignee { id name } project { id name } cycle { id name } labels { nodes { id name } } } } }",
                        json!({
                            "id": args.id,
                            "input": args.changes,
                        }),
                        Some("UpdateIssue"),
                    )
                    .await?;
                extract_graphql_field(envelope, "issueUpdate")
            }
            "create_comment" => {
                let args: CreateCommentArgs = parse_args(args)?;
                let config = self.resolve_client_config(&args.config)?;
                let envelope = self
                    .request_graphql(
                        &config,
                        "mutation CreateComment($input: CommentCreateInput!) { commentCreate(input: $input) { success comment { id body url createdAt user { id name } issue { id identifier title } } } }",
                        json!({
                            "input": {
                                "issueId": args.issue_id,
                                "body": args.body,
                            }
                        }),
                        Some("CreateComment"),
                    )
                    .await?;
                extract_graphql_field(envelope, "commentCreate")
            }
            "search" => {
                let args: SearchArgs = parse_args(args)?;
                let config = self.resolve_client_config(&args.config)?;
                self.search_issues(&config, &args.query, args.first).await
            }
            "graphql" => {
                let args: GraphqlArgs = parse_args(args)?;
                let config = self.resolve_client_config(&args.config)?;
                self.request_graphql(
                    &config,
                    &args.query,
                    args.variables.unwrap_or(JsonValue::Null),
                    args.operation_name.as_deref(),
                )
                .await
            }
            other => Err(ClientError::MethodNotFound(format!(
                "linear connector does not implement outbound method `{other}`"
            ))),
        }
    }
}

impl LinearClient {
    fn resolve_client_config(
        &self,
        args: &LinearClientConfigArgs,
    ) -> Result<ResolvedLinearClientConfig, ClientError> {
        resolve_client_config_args(args).map_err(ClientError::InvalidArgs)
    }

    fn ctx(&self) -> Result<ConnectorCtx, ClientError> {
        self.state
            .ctx
            .read()
            .expect("linear connector ctx poisoned")
            .clone()
            .ok_or_else(|| ClientError::Other("linear connector must be initialized".to_string()))
    }

    async fn request_graphql(
        &self,
        config: &ResolvedLinearClientConfig,
        query: &str,
        variables: JsonValue,
        operation_name: Option<&str>,
    ) -> Result<JsonValue, ClientError> {
        let ctx = self.ctx()?;
        ctx.rate_limiter
            .scoped(&self.provider_id, "graphql")
            .acquire()
            .await;

        let auth = self.auth_header(config).await?;
        let mut request_body = JsonMap::new();
        request_body.insert("query".to_string(), JsonValue::String(query.to_string()));
        if !variables.is_null() {
            request_body.insert("variables".to_string(), variables);
        }
        if let Some(operation_name) = operation_name {
            request_body.insert(
                "operationName".to_string(),
                JsonValue::String(operation_name.to_string()),
            );
        }

        let response = self
            .http
            .post(config.api_base_url.clone())
            .header("Content-Type", "application/json")
            .header("Authorization", auth)
            .json(&JsonValue::Object(request_body))
            .send()
            .await
            .map_err(|error| ClientError::Transport(error.to_string()))?;

        let status = response.status();
        let meta = graphql_meta(response.headers(), query);
        let payload = response
            .json::<JsonValue>()
            .await
            .map_err(|error| ClientError::Transport(error.to_string()))?;
        let errors = payload.get("errors").cloned();
        if is_rate_limited(status, errors.as_ref()) {
            return Err(ClientError::RateLimited(graphql_error_message(
                status,
                errors.as_ref(),
            )));
        }
        if !status.is_success() {
            return Err(ClientError::Transport(graphql_error_message(
                status,
                errors.as_ref(),
            )));
        }

        Ok(json!({
            "data": payload.get("data").cloned().unwrap_or(JsonValue::Null),
            "errors": errors.unwrap_or(JsonValue::Null),
            "meta": meta,
        }))
    }

    async fn auth_header(
        &self,
        config: &ResolvedLinearClientConfig,
    ) -> Result<String, ClientError> {
        match &config.auth {
            LinearAuthSource::ApiKeyInline(value) => Ok(value.clone()),
            LinearAuthSource::AccessTokenInline(value) => Ok(format!("Bearer {value}")),
            LinearAuthSource::ApiKeySecret(secret_id) => {
                let secret = self.secret_text(secret_id).await?;
                Ok(secret)
            }
            LinearAuthSource::AccessTokenSecret(secret_id) => {
                let secret = self.secret_text(secret_id).await?;
                Ok(format!("Bearer {secret}"))
            }
        }
    }

    async fn secret_text(&self, secret_id: &SecretId) -> Result<String, ClientError> {
        let ctx = self.ctx()?;
        let secret = ctx
            .secrets
            .get(secret_id)
            .await
            .map_err(|error| ClientError::Other(error.to_string()))?;
        Ok(secret.with_exposed(|bytes| String::from_utf8_lossy(bytes).to_string()))
    }

    async fn search_issues(
        &self,
        config: &ResolvedLinearClientConfig,
        query: &str,
        first: Option<i64>,
    ) -> Result<JsonValue, ClientError> {
        let searches = [
            (
                "query SearchIssues($query: String!, $first: Int) { searchIssues(query: $query, first: $first) { nodes { id identifier title url priority state { id name type } team { id key name } } } }",
                "SearchIssues",
            ),
            (
                "query SearchIssues($term: String!, $first: Int) { searchIssues(term: $term, first: $first) { nodes { id identifier title url priority state { id name type } team { id key name } } } }",
                "SearchIssues",
            ),
        ];

        for (index, (document, operation_name)) in searches.into_iter().enumerate() {
            let variables = if document.contains("$query") {
                json!({ "query": query, "first": first })
            } else {
                json!({ "term": query, "first": first })
            };
            match self
                .request_graphql(config, document, variables, Some(operation_name))
                .await
            {
                Ok(envelope) => return extract_graphql_field(envelope, "searchIssues"),
                Err(ClientError::Transport(message))
                    if index == 0
                        && (message.contains("Unknown argument")
                            || message.contains("GRAPHQL_VALIDATION_FAILED")) => {}
                Err(error) => return Err(error),
            }
        }

        Err(ClientError::Transport(
            "linear connector searchIssues query failed".to_string(),
        ))
    }

    async fn probe_health(&self, health_url: &str) -> Result<bool, ClientError> {
        let response = self
            .http
            .get(health_url)
            .send()
            .await
            .map_err(|error| ClientError::Transport(error.to_string()))?;
        Ok(response.status() == StatusCode::OK)
    }

    async fn reenable_webhook(
        &self,
        config: &ResolvedLinearClientConfig,
        webhook_id: &str,
    ) -> Result<(), ClientError> {
        let envelope = self
            .request_graphql(
                config,
                "mutation ReenableWebhook($id: String!, $input: WebhookUpdateInput!) { webhookUpdate(id: $id, input: $input) { success webhook { id enabled } } }",
                json!({
                    "id": webhook_id,
                    "input": { "enabled": true },
                }),
                Some("ReenableWebhook"),
            )
            .await?;
        let payload = envelope
            .get("data")
            .and_then(|value| value.get("webhookUpdate"))
            .ok_or_else(|| {
                ClientError::Transport(
                    "linear connector response missing `webhookUpdate` GraphQL field".to_string(),
                )
            })?;
        if payload
            .get("success")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false)
        {
            Ok(())
        } else {
            Err(ClientError::Transport(
                "linear connector webhook re-enable returned success = false".to_string(),
            ))
        }
    }
}

impl ActivatedLinearBinding {
    fn from_binding(binding: &TriggerBinding) -> Result<Self, ConnectorError> {
        let config: LinearBindingConfig =
            serde_json::from_value(binding.config.clone()).map_err(|error| {
                ConnectorError::Activation(format!(
                    "linear binding `{}` has invalid config: {error}",
                    binding.binding_id
                ))
            })?;
        let signing_secret =
            parse_secret_id(config.secrets.signing_secret.as_deref()).ok_or_else(|| {
                ConnectorError::Activation(format!(
                    "linear binding `{}` requires secrets.signing_secret",
                    binding.binding_id
                ))
            })?;
        let replay_grace_secs = config
            .replay_grace_secs
            .or(config.security.replay_grace_secs)
            .or(config.security.timestamp_grace_secs)
            .unwrap_or(DEFAULT_REPLAY_GRACE_SECS);
        let monitor = config
            .monitor
            .map(|monitor| {
                LinearWebhookMonitor::from_config(&binding.binding_id, monitor, &config.secrets)
            })
            .transpose()
            .map_err(ConnectorError::Activation)?
            .flatten();
        Ok(Self {
            binding_id: binding.binding_id.clone(),
            path: config.match_config.path,
            signing_secret,
            replay_grace_secs,
            monitor,
        })
    }
}

impl LinearWebhookMonitor {
    fn from_config(
        binding_id: &str,
        config: LinearWebhookMonitorConfig,
        binding_secrets: &LinearSecretsConfig,
    ) -> Result<Option<Self>, String> {
        if config.enabled == Some(false) {
            return Ok(None);
        }
        let webhook_id = config
            .webhook_id
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| format!("linear binding `{binding_id}` monitor requires webhook_id"))?;
        let health_url = config
            .health_url
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| format!("linear binding `{binding_id}` monitor requires health_url"))?;
        let mut client = config.client;
        if client.access_token.is_none()
            && client.access_token_secret.is_none()
            && client.api_key.is_none()
            && client.api_key_secret.is_none()
            && client.secrets.access_token.is_none()
            && client.secrets.api_key.is_none()
        {
            client.secrets.access_token = binding_secrets.access_token.clone();
            client.secrets.api_key = binding_secrets.api_key.clone();
        }
        let client_config = resolve_client_config_args(&client).map_err(|error| {
            format!("linear binding `{binding_id}` monitor auth is invalid: {error}")
        })?;
        Ok(Some(Self {
            webhook_id,
            health_url,
            probe_interval_secs: config
                .probe_interval_secs
                .unwrap_or(DEFAULT_WEBHOOK_MONITOR_PROBE_INTERVAL_SECS)
                .max(1),
            success_threshold: config
                .success_threshold
                .unwrap_or(DEFAULT_WEBHOOK_MONITOR_SUCCESS_THRESHOLD)
                .max(1),
            client_config,
        }))
    }
}

async fn run_webhook_monitor(
    client: Arc<LinearClient>,
    monitor: LinearWebhookMonitor,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut consecutive_successes = 0u32;
    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
            _ = tokio::time::sleep(StdDuration::from_secs(monitor.probe_interval_secs)) => {}
        }
        if *shutdown.borrow() {
            break;
        }
        match client.probe_health(&monitor.health_url).await {
            Ok(true) => {
                consecutive_successes = consecutive_successes.saturating_add(1);
                if consecutive_successes >= monitor.success_threshold
                    && client
                        .reenable_webhook(&monitor.client_config, &monitor.webhook_id)
                        .await
                        .is_ok()
                {
                    consecutive_successes = 0;
                }
            }
            Ok(false) | Err(_) => {
                consecutive_successes = 0;
            }
        }
    }
}

fn resolve_client_config_args(
    args: &LinearClientConfigArgs,
) -> Result<ResolvedLinearClientConfig, String> {
    let auth = if let Some(secret_id) = args
        .access_token_secret
        .as_deref()
        .or(args.secrets.access_token.as_deref())
        .and_then(|value| parse_secret_id(Some(value)))
    {
        LinearAuthSource::AccessTokenSecret(secret_id)
    } else if let Some(secret_id) = args
        .api_key_secret
        .as_deref()
        .or(args.secrets.api_key.as_deref())
        .and_then(|value| parse_secret_id(Some(value)))
    {
        LinearAuthSource::ApiKeySecret(secret_id)
    } else if let Some(token) = args.access_token.clone() {
        LinearAuthSource::AccessTokenInline(token)
    } else if let Some(api_key) = args.api_key.clone() {
        LinearAuthSource::ApiKeyInline(api_key)
    } else {
        return Err(
            "linear connector requires access_token, access_token_secret, api_key, or api_key_secret"
                .to_string(),
        );
    };

    Ok(ResolvedLinearClientConfig {
        api_base_url: args
            .api_base_url
            .clone()
            .unwrap_or_else(|| DEFAULT_API_BASE_URL.to_string()),
        auth,
    })
}

fn effective_headers(headers: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut effective = headers.clone();
    for (raw, canonical) in [
        ("content-type", "Content-Type"),
        ("linear-signature", "Linear-Signature"),
        ("linear-delivery", "Linear-Delivery"),
        ("linear-event", "Linear-Event"),
    ] {
        if let Some(value) = header_value(headers, raw) {
            effective
                .entry(canonical.to_string())
                .or_insert_with(|| value.to_string());
        }
    }
    effective
}

fn linear_trigger_kind(payload: &JsonValue) -> String {
    let event = payload
        .get("type")
        .and_then(JsonValue::as_str)
        .map(|kind| {
            let lower = kind.to_ascii_lowercase();
            match lower.as_str() {
                "issue" => "issue".to_string(),
                "comment" | "issuecomment" | "issue_comment" => "comment".to_string(),
                "issuelabel" | "issue_label" => "issue_label".to_string(),
                "project" | "projectupdate" | "project_update" => "project".to_string(),
                "cycle" => "cycle".to_string(),
                "customer" => "customer".to_string(),
                "customerrequest" | "customer_request" => "customer_request".to_string(),
                _ => lower,
            }
        })
        .unwrap_or_else(|| "other".to_string());
    let action = payload
        .get("action")
        .and_then(JsonValue::as_str)
        .unwrap_or("update");
    format!("{event}.{action}")
}

fn infer_occurred_at(payload: &ProviderPayload) -> Option<OffsetDateTime> {
    let ProviderPayload::Known(crate::triggers::event::KnownProviderPayload::Linear(payload)) =
        payload
    else {
        return None;
    };
    let timestamp = match payload {
        crate::triggers::event::LinearEventPayload::Issue(payload) => {
            payload.common.webhook_timestamp
        }
        crate::triggers::event::LinearEventPayload::IssueComment(payload) => {
            payload.common.webhook_timestamp
        }
        crate::triggers::event::LinearEventPayload::IssueLabel(payload) => {
            payload.common.webhook_timestamp
        }
        crate::triggers::event::LinearEventPayload::Project(payload) => {
            payload.common.webhook_timestamp
        }
        crate::triggers::event::LinearEventPayload::Cycle(payload) => {
            payload.common.webhook_timestamp
        }
        crate::triggers::event::LinearEventPayload::Customer(payload) => {
            payload.common.webhook_timestamp
        }
        crate::triggers::event::LinearEventPayload::CustomerRequest(payload) => {
            payload.common.webhook_timestamp
        }
        crate::triggers::event::LinearEventPayload::Other(payload) => payload.webhook_timestamp,
    }?;
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(timestamp) * 1_000_000).ok()
}

fn parse_args<T: for<'de> Deserialize<'de>>(args: JsonValue) -> Result<T, ClientError> {
    serde_json::from_value(args).map_err(|error| ClientError::InvalidArgs(error.to_string()))
}

fn load_secret_text_blocking(
    ctx: &ConnectorCtx,
    secret_id: &SecretId,
) -> Result<String, ConnectorError> {
    let secret = futures::executor::block_on(ctx.secrets.get(secret_id))?;
    secret.with_exposed(|bytes| {
        std::str::from_utf8(bytes)
            .map(|value| value.to_string())
            .map_err(|error| {
                ConnectorError::Secret(format!(
                    "secret '{}' is not valid UTF-8: {error}",
                    secret_id
                ))
            })
    })
}

fn parse_secret_id(raw: Option<&str>) -> Option<SecretId> {
    let trimmed = raw?.trim();
    if trimmed.is_empty() {
        return None;
    }
    let (base, version) = match trimmed.rsplit_once('@') {
        Some((base, version_text)) => {
            let version = version_text.parse::<u64>().ok()?;
            (base, SecretVersion::Exact(version))
        }
        None => (trimmed, SecretVersion::Latest),
    };
    let (namespace, name) = base.split_once('/')?;
    if namespace.is_empty() || name.is_empty() {
        return None;
    }
    Some(SecretId::new(namespace, name).with_version(version))
}

fn header_value<'a>(headers: &'a BTreeMap<String, String>, name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

fn fallback_body_digest(body: &[u8]) -> String {
    use sha2::Digest;

    let digest = sha2::Sha256::digest(body);
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        encoded.push_str(&format!("{byte:02x}"));
    }
    format!("sha256:{encoded}")
}

fn graphql_meta(headers: &reqwest::header::HeaderMap, query: &str) -> JsonValue {
    let estimated = estimate_query_complexity(query);
    let observed = header_i64(headers, "x-complexity");
    let complexity = observed.or(estimated);
    let complexity_warning = complexity.is_some_and(|value| value >= COMPLEXITY_WARNING_THRESHOLD);
    json!({
        "complexity_estimate": estimated,
        "observed_complexity": observed,
        "complexity_warning": complexity_warning,
        "rate_limit": {
            "requests_limit": header_i64(headers, "x-ratelimit-requests-limit"),
            "requests_remaining": header_i64(headers, "x-ratelimit-requests-remaining"),
            "requests_reset": header_i64(headers, "x-ratelimit-requests-reset"),
            "complexity_limit": header_i64(headers, "x-ratelimit-complexity-limit"),
            "complexity_remaining": header_i64(headers, "x-ratelimit-complexity-remaining"),
            "complexity_reset": header_i64(headers, "x-ratelimit-complexity-reset"),
        },
    })
}

fn header_i64(headers: &reqwest::header::HeaderMap, name: &str) -> Option<i64> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<i64>().ok())
}

fn estimate_query_complexity(query: &str) -> Option<i64> {
    let normalized = query.trim();
    if normalized.is_empty() {
        return None;
    }
    let mut score = 1i64;
    let field_count =
        normalized.matches('{').count() as i64 + normalized.matches('}').count() as i64;
    score += field_count.max(1);
    for window in ["first:", "last:"] {
        let mut rest = normalized;
        while let Some(index) = rest.find(window) {
            let after = &rest[index + window.len()..];
            let digits: String = after
                .chars()
                .skip_while(|ch| ch.is_whitespace())
                .take_while(|ch| ch.is_ascii_digit())
                .collect();
            if let Ok(value) = digits.parse::<i64>() {
                score += value;
            } else {
                score += 50;
            }
            rest = after;
        }
    }
    score += (normalized.matches("nodes").count() as i64) * 50;
    Some(score)
}

fn extract_graphql_field(mut envelope: JsonValue, field: &str) -> Result<JsonValue, ClientError> {
    let meta = envelope.get("meta").cloned().unwrap_or(JsonValue::Null);
    let Some(data) = envelope.get_mut("data").and_then(JsonValue::as_object_mut) else {
        return Err(ClientError::Transport(
            "linear connector response missing GraphQL data".to_string(),
        ));
    };
    let mut extracted = data.remove(field).ok_or_else(|| {
        ClientError::Transport(format!(
            "linear connector response missing `{field}` GraphQL field"
        ))
    })?;
    if let Some(object) = extracted.as_object_mut() {
        object.insert("meta".to_string(), meta);
    }
    Ok(extracted)
}

fn is_rate_limited(status: StatusCode, errors: Option<&JsonValue>) -> bool {
    if status == StatusCode::TOO_MANY_REQUESTS {
        return true;
    }
    if status == StatusCode::BAD_REQUEST {
        return graphql_error_codes(errors)
            .into_iter()
            .any(|code| code.eq_ignore_ascii_case("RATELIMITED"));
    }
    false
}

fn graphql_error_codes(errors: Option<&JsonValue>) -> Vec<String> {
    errors
        .and_then(JsonValue::as_array)
        .map(|errors| {
            errors
                .iter()
                .filter_map(|error| {
                    error
                        .get("extensions")
                        .and_then(JsonValue::as_object)
                        .and_then(|extensions| extensions.get("code"))
                        .and_then(JsonValue::as_str)
                        .map(ToString::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn graphql_error_message(status: StatusCode, errors: Option<&JsonValue>) -> String {
    let messages = errors
        .and_then(JsonValue::as_array)
        .map(|errors| {
            errors
                .iter()
                .filter_map(|error| {
                    error
                        .get("message")
                        .and_then(JsonValue::as_str)
                        .map(ToString::to_string)
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if messages.is_empty() {
        format!("linear GraphQL request failed with status {status}")
    } else {
        format!(
            "linear GraphQL request failed with status {status}: {}",
            messages.join("; ")
        )
    }
}
