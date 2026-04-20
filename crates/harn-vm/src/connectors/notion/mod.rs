use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration as StdDuration;

use async_trait::async_trait;
use reqwest::{Method, Response, StatusCode, Url};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map as JsonMap, Value as JsonValue};
use sha2::{Digest, Sha256};
use time::{Duration, OffsetDateTime};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use crate::connectors::hmac::HmacSignatureStyle;
use crate::connectors::{
    ActivationHandle, ClientError, Connector, ConnectorClient, ConnectorCtx, ConnectorError,
    ProviderPayloadSchema, RawInbound, TriggerBinding, TriggerKind,
};
use crate::event_log::{EventLog, LogEvent, Topic};
use crate::secrets::{SecretId, SecretVersion};
use crate::triggers::dispatcher::{DispatchError, InboxEnvelope};
use crate::triggers::event::{KnownProviderPayload, NotionPolledChangeEvent};
use crate::triggers::{
    redact_headers, HeaderRedactionPolicy, ProviderId, ProviderPayload, SignatureStatus, TraceId,
    TriggerEvent, TriggerEventId, TRIGGER_INBOX_ENVELOPES_TOPIC,
};

#[cfg(test)]
mod tests;

pub const NOTION_PROVIDER_ID: &str = "notion";
pub const DEFAULT_NOTION_API_BASE_URL: &str = "https://api.notion.com/v1";
pub const DEFAULT_NOTION_API_VERSION: &str = "2026-03-11";
const NOTION_HANDSHAKE_TOPIC: &str = "connectors.notion.webhook.handshake";
const NOTION_POLL_STATE_TOPIC: &str = "connectors.notion.poll.state";
const NOTION_POLL_CACHE_TOPIC: &str = "connectors.notion.poll.cache";
const NOTION_RATE_LIMIT_TOPIC: &str = "connectors.notion.rate_limit";
const RECENT_BUCKET_RETENTION: Duration = Duration::minutes(15);

pub struct NotionConnector {
    provider_id: ProviderId,
    kinds: Vec<TriggerKind>,
    state: Arc<NotionConnectorState>,
    client: Arc<NotionClient>,
}

#[derive(Default)]
struct NotionConnectorState {
    ctx: RwLock<Option<ConnectorCtx>>,
    webhook_bindings: RwLock<HashMap<String, ActivatedNotionWebhookBinding>>,
    poll_bindings: RwLock<HashMap<String, ActivatedNotionPollBinding>>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
    shutdown: Mutex<Arc<ShutdownSignal>>,
    recent_buckets: Mutex<HashMap<String, OffsetDateTime>>,
}

#[derive(Clone, Debug)]
struct ActivatedNotionWebhookBinding {
    binding_id: String,
    path: Option<String>,
    verification_token: Option<SecretId>,
}

#[derive(Clone, Debug)]
struct ActivatedNotionPollBinding {
    binding_id: String,
    resource: NotionPollResource,
    resource_id: String,
    interval: StdDuration,
    filter: Option<JsonValue>,
    sorts: Vec<JsonValue>,
    high_water_mark: String,
    page_size: usize,
    api_token: SecretId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NotionPollResource {
    DataSource,
    Database,
}

impl fmt::Display for NotionPollResource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DataSource => f.write_str("data_source"),
            Self::Database => f.write_str("database"),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct NotionBindingConfig {
    #[serde(default, rename = "match")]
    match_config: NotionMatchConfig,
    #[serde(default)]
    secrets: NotionSecretsConfig,
    #[serde(default)]
    #[allow(dead_code)]
    webhook: NotionWebhookConfig,
    #[serde(default)]
    poll: NotionPollConfig,
}

#[derive(Debug, Default, Deserialize)]
struct NotionMatchConfig {
    path: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct NotionSecretsConfig {
    verification_token: Option<String>,
    signing_secret: Option<String>,
    api_token: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct NotionWebhookConfig {}

#[derive(Debug, Default, Deserialize)]
struct NotionPollConfig {
    resource: Option<String>,
    data_source_id: Option<String>,
    database_id: Option<String>,
    interval_secs: Option<u64>,
    #[serde(default)]
    filter: Option<JsonValue>,
    #[serde(default)]
    sorts: Vec<JsonValue>,
    high_water_mark: Option<String>,
    page_size: Option<usize>,
}

struct NotionClient {
    provider_id: ProviderId,
    state: Arc<NotionConnectorState>,
    http: reqwest::Client,
}

#[derive(Debug, Default, Deserialize)]
struct NotionClientConfigArgs {
    api_base_url: Option<String>,
    notion_version: Option<String>,
    api_token: Option<String>,
    api_token_secret: Option<String>,
    #[serde(default)]
    secrets: NotionSecretsConfig,
}

#[derive(Clone, Debug)]
struct ResolvedNotionClientConfig {
    api_base_url: String,
    notion_version: String,
    api_token: NotionTokenSource,
}

#[derive(Clone, Debug)]
enum NotionTokenSource {
    Inline(String),
    Secret(SecretId),
}

#[derive(Debug, Deserialize)]
struct GetPageArgs {
    #[serde(flatten)]
    config: NotionClientConfigArgs,
    id: String,
}

#[derive(Debug, Deserialize)]
struct UpdatePageArgs {
    #[serde(flatten)]
    config: NotionClientConfigArgs,
    id: String,
    #[serde(default)]
    properties: JsonValue,
    #[serde(default)]
    icon: Option<JsonValue>,
    #[serde(default)]
    cover: Option<JsonValue>,
    #[serde(default)]
    archived: Option<bool>,
    #[serde(default)]
    in_trash: Option<bool>,
    #[serde(default)]
    is_locked: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct AppendBlocksArgs {
    #[serde(flatten)]
    config: NotionClientConfigArgs,
    page_id: String,
    blocks: Vec<JsonValue>,
}

#[derive(Debug, Deserialize)]
struct QueryDatabaseArgs {
    #[serde(flatten)]
    config: NotionClientConfigArgs,
    id: String,
    #[serde(default)]
    filter: Option<JsonValue>,
    #[serde(default)]
    sorts: Option<Vec<JsonValue>>,
    #[serde(default)]
    start_cursor: Option<String>,
    #[serde(default)]
    page_size: Option<usize>,
    #[serde(default)]
    resource: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SearchArgs {
    #[serde(flatten)]
    config: NotionClientConfigArgs,
    query: String,
    #[serde(default)]
    filter: Option<JsonValue>,
    #[serde(default)]
    sort: Option<JsonValue>,
    #[serde(default)]
    start_cursor: Option<String>,
    #[serde(default)]
    page_size: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct CreateCommentArgs {
    #[serde(flatten)]
    config: NotionClientConfigArgs,
    page_id: String,
    rich_text: Vec<JsonValue>,
}

#[derive(Debug, Deserialize)]
struct ApiCallArgs {
    #[serde(flatten)]
    config: NotionClientConfigArgs,
    path: String,
    method: String,
    #[serde(default)]
    body: Option<JsonValue>,
}

#[derive(Debug, Deserialize)]
struct NotionListResponse {
    #[serde(default)]
    results: Vec<JsonValue>,
    #[serde(default)]
    has_more: bool,
    #[serde(default)]
    next_cursor: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PersistedNotionWebhookHandshake {
    pub binding_id: String,
    pub path: Option<String>,
    pub verification_token: String,
    #[serde(with = "time::serde::rfc3339")]
    pub captured_at: OffsetDateTime,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PersistedNotionPollState {
    pub binding_id: String,
    #[serde(with = "time::serde::rfc3339")]
    pub high_water: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PersistedNotionPollCacheEntry {
    pub binding_id: String,
    pub entity_id: String,
    #[serde(with = "time::serde::rfc3339")]
    pub last_edited_time: OffsetDateTime,
    pub snapshot: JsonValue,
}

#[derive(Debug, Default)]
struct ShutdownSignal {
    stopped: AtomicBool,
    notify: Notify,
}

impl ShutdownSignal {
    fn request_stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    async fn cancelled(&self) {
        self.notify.notified().await;
    }
}

impl NotionConnector {
    pub fn new() -> Self {
        let state = Arc::new(NotionConnectorState {
            shutdown: Mutex::new(Arc::new(ShutdownSignal::default())),
            ..NotionConnectorState::default()
        });
        let client = Arc::new(NotionClient {
            provider_id: ProviderId::from(NOTION_PROVIDER_ID),
            state: state.clone(),
            http: reqwest::Client::builder()
                .user_agent("harn-notion-connector")
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
        });
        Self {
            provider_id: ProviderId::from(NOTION_PROVIDER_ID),
            kinds: vec![TriggerKind::from("webhook"), TriggerKind::from("poll")],
            state,
            client,
        }
    }

    fn binding_for_raw(
        &self,
        raw: &RawInbound,
    ) -> Result<ActivatedNotionWebhookBinding, ConnectorError> {
        let bindings = self
            .state
            .webhook_bindings
            .read()
            .expect("notion webhook bindings poisoned");
        if let Some(binding_id) = raw.metadata.get("binding_id").and_then(JsonValue::as_str) {
            return bindings.get(binding_id).cloned().ok_or_else(|| {
                ConnectorError::Unsupported(format!(
                    "notion connector has no active webhook binding `{binding_id}`"
                ))
            });
        }
        if bindings.len() == 1 {
            return bindings
                .values()
                .next()
                .cloned()
                .ok_or_else(|| ConnectorError::Activation("notion bindings missing".to_string()));
        }
        Err(ConnectorError::Unsupported(
            "notion connector requires raw.metadata.binding_id when multiple webhook bindings are active".to_string(),
        ))
    }

    fn ctx(&self) -> Result<ConnectorCtx, ConnectorError> {
        self.state
            .ctx
            .read()
            .expect("notion connector ctx poisoned")
            .clone()
            .ok_or_else(|| {
                ConnectorError::Activation(
                    "notion connector must be initialized before use".to_string(),
                )
            })
    }

    fn take_tasks(&self) -> Vec<JoinHandle<()>> {
        self.state
            .tasks
            .lock()
            .expect("notion connector tasks poisoned")
            .drain(..)
            .collect()
    }
}

impl Default for NotionConnector {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for NotionConnector {
    fn drop(&mut self) {
        self.state
            .shutdown
            .lock()
            .expect("notion connector shutdown poisoned")
            .request_stop();
        let mut tasks = self
            .state
            .tasks
            .lock()
            .expect("notion connector tasks poisoned");
        for task in tasks.drain(..) {
            task.abort();
        }
    }
}

#[async_trait]
impl Connector for NotionConnector {
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
            .expect("notion connector ctx poisoned") = Some(ctx);
        Ok(())
    }

    async fn activate(
        &self,
        bindings: &[TriggerBinding],
    ) -> Result<ActivationHandle, ConnectorError> {
        let ctx = self.ctx()?;
        let shutdown = Arc::new(ShutdownSignal::default());
        *self
            .state
            .shutdown
            .lock()
            .expect("notion connector shutdown poisoned") = shutdown.clone();

        for task in self.take_tasks() {
            task.abort();
        }

        let mut webhook_bindings = HashMap::new();
        let mut poll_bindings = HashMap::new();
        let mut paths = BTreeSet::new();

        for binding in bindings {
            match binding.kind.as_str() {
                "webhook" => {
                    let activated = ActivatedNotionWebhookBinding::from_binding(binding)?;
                    if let Some(path) = &activated.path {
                        if !paths.insert(path.clone()) {
                            return Err(ConnectorError::Activation(format!(
                                "notion connector path `{path}` is configured by multiple bindings"
                            )));
                        }
                    }
                    webhook_bindings.insert(binding.binding_id.clone(), activated);
                }
                "poll" => {
                    let activated = ActivatedNotionPollBinding::from_binding(binding)?;
                    poll_bindings.insert(binding.binding_id.clone(), activated);
                }
                other => {
                    return Err(ConnectorError::Activation(format!(
                        "notion connector does not support trigger kind `{other}`"
                    )));
                }
            }
        }

        *self
            .state
            .webhook_bindings
            .write()
            .expect("notion webhook bindings poisoned") = webhook_bindings;
        *self
            .state
            .poll_bindings
            .write()
            .expect("notion poll bindings poisoned") = poll_bindings.clone();

        let mut tasks = self
            .state
            .tasks
            .lock()
            .expect("notion connector tasks poisoned");
        for binding in poll_bindings.into_values() {
            let client = self.client.clone();
            let state = self.state.clone();
            let shutdown = shutdown.clone();
            let ctx = ctx.clone();
            tasks.push(tokio::spawn(async move {
                let _ = run_poll_loop(client, state, ctx, binding, shutdown).await;
            }));
        }

        Ok(ActivationHandle::new(
            self.provider_id.clone(),
            bindings.len(),
        ))
    }

    async fn shutdown(&self, deadline: StdDuration) -> Result<(), ConnectorError> {
        self.state
            .shutdown
            .lock()
            .expect("notion connector shutdown poisoned")
            .request_stop();
        let pending = self.take_tasks();
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
                    "notion connector shutdown exceeded {}s",
                    deadline.as_secs()
                ))
            })?;
        Ok(())
    }

    async fn normalize_inbound(&self, raw: RawInbound) -> Result<TriggerEvent, ConnectorError> {
        let ctx = self.ctx()?;
        let binding = self.binding_for_raw(&raw)?;
        let provider = self.provider_id.clone();
        let headers = effective_headers(&raw.headers);
        let payload = raw.json_body()?;

        if let Some(verification_token) = payload
            .get("verification_token")
            .and_then(JsonValue::as_str)
            .filter(|value| !value.trim().is_empty())
        {
            let handshake = PersistedNotionWebhookHandshake {
                binding_id: binding.binding_id.clone(),
                path: binding.path.clone(),
                verification_token: verification_token.to_string(),
                captured_at: raw.received_at,
            };
            persist_handshake(ctx.event_log.as_ref(), &handshake)?;
            let provider_payload = ProviderPayload::normalize(
                &provider,
                "subscription.verification",
                &headers,
                payload,
            )
            .map_err(|error| ConnectorError::Unsupported(error.to_string()))?;
            return Ok(TriggerEvent {
                id: TriggerEventId::new(),
                provider,
                kind: "subscription.verification".to_string(),
                received_at: raw.received_at,
                occurred_at: Some(raw.received_at),
                dedupe_key: format!("notion-handshake:{}", binding.binding_id),
                trace_id: TraceId::new(),
                tenant_id: raw.tenant_id.clone(),
                headers: redact_headers(&headers, &HeaderRedactionPolicy::default()),
                batch: None,
                raw_body: Some(raw.body.clone()),
                provider_payload,
                signature_status: SignatureStatus::Unsigned,
                dedupe_claimed: false,
            });
        }

        let verification_token_id = binding.verification_token.as_ref().ok_or_else(|| {
            ConnectorError::Activation(format!(
                "notion webhook binding `{}` requires secrets.verification_token after the setup handshake",
                binding.binding_id
            ))
        })?;
        let verification_token =
            load_secret_text_blocking(&ctx, verification_token_id, "notion verification token")?;
        futures::executor::block_on(crate::connectors::hmac::verify_hmac_signed(
            ctx.event_log.as_ref(),
            &provider,
            HmacSignatureStyle::notion(),
            &raw.body,
            &headers,
            verification_token.as_str(),
            None,
            raw.received_at,
        ))?;

        let kind = payload
            .get("type")
            .and_then(JsonValue::as_str)
            .map(ToString::to_string)
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "notion.webhook".to_string());
        let occurred_at = payload
            .get("timestamp")
            .and_then(JsonValue::as_str)
            .and_then(parse_rfc3339);
        let entity_id = payload
            .get("entity")
            .and_then(|value| value.get("id"))
            .and_then(JsonValue::as_str);
        let dedupe_key = notion_dedupe_key(
            entity_id,
            occurred_at,
            payload.get("id").and_then(JsonValue::as_str),
            &raw.body,
        );
        remember_recent_bucket(&self.state, &dedupe_key, raw.received_at);
        let provider_payload =
            ProviderPayload::normalize(&provider, kind.as_str(), &headers, payload)
                .map_err(|error| ConnectorError::Unsupported(error.to_string()))?;

        Ok(TriggerEvent {
            id: TriggerEventId::new(),
            provider,
            kind,
            received_at: raw.received_at,
            occurred_at,
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
        ProviderPayloadSchema::named("NotionEventPayload")
    }

    fn client(&self) -> Arc<dyn ConnectorClient> {
        self.client.clone()
    }
}

#[async_trait]
impl ConnectorClient for NotionClient {
    async fn call(&self, method: &str, args: JsonValue) -> Result<JsonValue, ClientError> {
        match method {
            "get_page" => {
                let args: GetPageArgs = parse_args(args)?;
                let config = self.resolve_client_config(&args.config)?;
                self.request_json(&config, Method::GET, &format!("/pages/{}", args.id), None)
                    .await
            }
            "update_page" => {
                let args: UpdatePageArgs = parse_args(args)?;
                let config = self.resolve_client_config(&args.config)?;
                let mut body = JsonMap::new();
                if !args.properties.is_null() {
                    body.insert("properties".to_string(), args.properties);
                }
                if let Some(icon) = args.icon {
                    body.insert("icon".to_string(), icon);
                }
                if let Some(cover) = args.cover {
                    body.insert("cover".to_string(), cover);
                }
                if let Some(archived) = args.archived {
                    body.insert("archived".to_string(), JsonValue::Bool(archived));
                }
                if let Some(in_trash) = args.in_trash {
                    body.insert("in_trash".to_string(), JsonValue::Bool(in_trash));
                }
                if let Some(is_locked) = args.is_locked {
                    body.insert("is_locked".to_string(), JsonValue::Bool(is_locked));
                }
                self.request_json(
                    &config,
                    Method::PATCH,
                    &format!("/pages/{}", args.id),
                    Some(JsonValue::Object(body)),
                )
                .await
            }
            "append_blocks" => {
                let args: AppendBlocksArgs = parse_args(args)?;
                let config = self.resolve_client_config(&args.config)?;
                self.request_json(
                    &config,
                    Method::PATCH,
                    &format!("/blocks/{}/children", args.page_id),
                    Some(json!({ "children": args.blocks })),
                )
                .await
            }
            "query_database" => {
                let args: QueryDatabaseArgs = parse_args(args)?;
                let config = self.resolve_client_config(&args.config)?;
                let resource = parse_poll_resource(args.resource.as_deref())
                    .map_err(|error| ClientError::InvalidArgs(error.to_string()))?;
                let path = match resource {
                    NotionPollResource::DataSource => format!("/data_sources/{}/query", args.id),
                    NotionPollResource::Database => format!("/databases/{}/query", args.id),
                };
                let mut body = JsonMap::new();
                if let Some(filter) = args.filter {
                    body.insert("filter".to_string(), filter);
                }
                if let Some(sorts) = args.sorts {
                    body.insert("sorts".to_string(), JsonValue::Array(sorts));
                }
                if let Some(cursor) = args.start_cursor {
                    body.insert("start_cursor".to_string(), JsonValue::String(cursor));
                }
                if let Some(page_size) = args.page_size {
                    body.insert("page_size".to_string(), JsonValue::from(page_size));
                }
                self.request_json(&config, Method::POST, &path, Some(JsonValue::Object(body)))
                    .await
            }
            "search" => {
                let args: SearchArgs = parse_args(args)?;
                let config = self.resolve_client_config(&args.config)?;
                let mut body = JsonMap::new();
                body.insert("query".to_string(), JsonValue::String(args.query));
                if let Some(filter) = args.filter {
                    body.insert("filter".to_string(), filter);
                }
                if let Some(sort) = args.sort {
                    body.insert("sort".to_string(), sort);
                }
                if let Some(cursor) = args.start_cursor {
                    body.insert("start_cursor".to_string(), JsonValue::String(cursor));
                }
                if let Some(page_size) = args.page_size {
                    body.insert("page_size".to_string(), JsonValue::from(page_size));
                }
                self.request_json(
                    &config,
                    Method::POST,
                    "/search",
                    Some(JsonValue::Object(body)),
                )
                .await
            }
            "create_comment" => {
                let args: CreateCommentArgs = parse_args(args)?;
                let config = self.resolve_client_config(&args.config)?;
                self.request_json(
                    &config,
                    Method::POST,
                    "/comments",
                    Some(json!({
                        "parent": { "page_id": args.page_id },
                        "rich_text": args.rich_text,
                    })),
                )
                .await
            }
            "api_call" => {
                let args: ApiCallArgs = parse_args(args)?;
                let config = self.resolve_client_config(&args.config)?;
                let method = Method::from_bytes(args.method.as_bytes())
                    .map_err(|error| ClientError::InvalidArgs(error.to_string()))?;
                self.request_json(&config, method, &args.path, args.body)
                    .await
            }
            other => Err(ClientError::MethodNotFound(format!(
                "notion connector does not implement outbound method `{other}`"
            ))),
        }
    }
}

impl NotionClient {
    fn resolve_client_config(
        &self,
        args: &NotionClientConfigArgs,
    ) -> Result<ResolvedNotionClientConfig, ClientError> {
        let api_token = if let Some(secret_id) = args
            .api_token_secret
            .as_deref()
            .or(args.secrets.api_token.as_deref())
            .and_then(|value| parse_secret_id(Some(value)))
        {
            NotionTokenSource::Secret(secret_id)
        } else if let Some(token) = args.api_token.clone() {
            NotionTokenSource::Inline(token)
        } else {
            return Err(ClientError::InvalidArgs(
                "notion connector requires api_token or api_token_secret".to_string(),
            ));
        };
        Ok(ResolvedNotionClientConfig {
            api_base_url: args
                .api_base_url
                .clone()
                .unwrap_or_else(|| DEFAULT_NOTION_API_BASE_URL.to_string()),
            notion_version: args
                .notion_version
                .clone()
                .unwrap_or_else(|| DEFAULT_NOTION_API_VERSION.to_string()),
            api_token,
        })
    }

    fn ctx(&self) -> Result<ConnectorCtx, ClientError> {
        self.state
            .ctx
            .read()
            .expect("notion connector ctx poisoned")
            .clone()
            .ok_or_else(|| ClientError::Other("notion connector must be initialized".to_string()))
    }

    async fn request_json(
        &self,
        config: &ResolvedNotionClientConfig,
        method: Method,
        path: &str,
        body: Option<JsonValue>,
    ) -> Result<JsonValue, ClientError> {
        let response = self.request_response(config, method, path, body).await?;
        response
            .json::<JsonValue>()
            .await
            .map_err(|error| ClientError::Transport(error.to_string()))
    }

    async fn request_response(
        &self,
        config: &ResolvedNotionClientConfig,
        method: Method,
        path: &str,
        body: Option<JsonValue>,
    ) -> Result<Response, ClientError> {
        let mut retried_rate_limit = false;
        loop {
            let ctx = self.ctx()?;
            ctx.rate_limiter
                .scoped(&self.provider_id, "api")
                .acquire()
                .await;
            let token = self.api_token(config).await?;
            let url = absolute_api_url(&config.api_base_url, path)?;
            let mut request = self
                .http
                .request(method.clone(), url)
                .header("Authorization", format!("Bearer {token}"))
                .header("Notion-Version", &config.notion_version);
            if let Some(payload) = body.clone() {
                request = request.json(&payload);
            }
            let response = request
                .send()
                .await
                .map_err(|error| ClientError::Transport(error.to_string()))?;
            self.record_rate_limit_observation(&response).await;
            if response.status() == StatusCode::TOO_MANY_REQUESTS && !retried_rate_limit {
                tokio::time::sleep(rate_limit_backoff(&response)).await;
                retried_rate_limit = true;
                continue;
            }
            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                let message = if body.trim().is_empty() {
                    format!("notion API request failed with status {status}")
                } else {
                    format!("notion API request failed with status {status}: {body}")
                };
                return Err(if status == StatusCode::TOO_MANY_REQUESTS {
                    ClientError::RateLimited(message)
                } else {
                    ClientError::Transport(message)
                });
            }
            return Ok(response);
        }
    }

    async fn api_token(&self, config: &ResolvedNotionClientConfig) -> Result<String, ClientError> {
        match &config.api_token {
            NotionTokenSource::Inline(token) => Ok(token.clone()),
            NotionTokenSource::Secret(secret_id) => {
                let ctx = self.ctx()?;
                let secret = ctx
                    .secrets
                    .get(secret_id)
                    .await
                    .map_err(|error| ClientError::Other(error.to_string()))?;
                Ok(secret.with_exposed(|bytes| String::from_utf8_lossy(bytes).to_string()))
            }
        }
    }

    async fn record_rate_limit_observation(&self, response: &Response) {
        if response.status() != StatusCode::TOO_MANY_REQUESTS {
            return;
        }
        let Ok(ctx) = self.ctx() else {
            return;
        };
        let Ok(topic) = Topic::new(NOTION_RATE_LIMIT_TOPIC) else {
            return;
        };
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok());
        let _ = ctx
            .event_log
            .append(
                &topic,
                LogEvent::new(
                    "notion.rate_limit",
                    json!({
                        "retry_after": retry_after,
                        "status": response.status().as_u16(),
                    }),
                ),
            )
            .await;
    }
}

impl ActivatedNotionWebhookBinding {
    fn from_binding(binding: &TriggerBinding) -> Result<Self, ConnectorError> {
        let config: NotionBindingConfig =
            serde_json::from_value(binding.config.clone()).map_err(|error| {
                ConnectorError::Activation(format!(
                    "notion binding `{}` has invalid config: {error}",
                    binding.binding_id
                ))
            })?;
        Ok(Self {
            binding_id: binding.binding_id.clone(),
            path: config.match_config.path,
            verification_token: parse_secret_id(
                config
                    .secrets
                    .verification_token
                    .as_deref()
                    .or(config.secrets.signing_secret.as_deref()),
            ),
        })
    }
}

impl ActivatedNotionPollBinding {
    fn from_binding(binding: &TriggerBinding) -> Result<Self, ConnectorError> {
        let config: NotionBindingConfig =
            serde_json::from_value(binding.config.clone()).map_err(|error| {
                ConnectorError::Activation(format!(
                    "notion binding `{}` has invalid config: {error}",
                    binding.binding_id
                ))
            })?;
        let resource = parse_poll_resource(config.poll.resource.as_deref())?;
        let resource_id = match resource {
            NotionPollResource::DataSource => {
                config.poll.data_source_id.or(config.poll.database_id)
            }
            NotionPollResource::Database => config.poll.database_id.or(config.poll.data_source_id),
        }
        .ok_or_else(|| {
            ConnectorError::Activation(format!(
                "notion poll binding `{}` requires a {} id",
                binding.binding_id, resource
            ))
        })?;
        let interval_secs = config.poll.interval_secs.unwrap_or(300);
        if interval_secs == 0 {
            return Err(ConnectorError::Activation(format!(
                "notion poll binding `{}` requires interval_secs > 0",
                binding.binding_id
            )));
        }
        let high_water_mark = config
            .poll
            .high_water_mark
            .unwrap_or_else(|| "last_edited_time".to_string());
        if high_water_mark != "last_edited_time" {
            return Err(ConnectorError::Activation(format!(
                "notion poll binding `{}` only supports high_water_mark = \"last_edited_time\" on this branch",
                binding.binding_id
            )));
        }
        let api_token = parse_secret_id(config.secrets.api_token.as_deref()).ok_or_else(|| {
            ConnectorError::Activation(format!(
                "notion poll binding `{}` requires secrets.api_token",
                binding.binding_id
            ))
        })?;
        Ok(Self {
            binding_id: binding.binding_id.clone(),
            resource,
            resource_id,
            interval: StdDuration::from_secs(interval_secs),
            filter: config.poll.filter,
            sorts: config.poll.sorts,
            high_water_mark,
            page_size: config.poll.page_size.unwrap_or(100).clamp(1, 100),
            api_token,
        })
    }
}

async fn run_poll_loop(
    client: Arc<NotionClient>,
    state: Arc<NotionConnectorState>,
    ctx: ConnectorCtx,
    binding: ActivatedNotionPollBinding,
    shutdown: Arc<ShutdownSignal>,
) -> Result<(), ConnectorError> {
    let mut high_water = load_poll_state(ctx.event_log.as_ref(), &binding.binding_id).await?;
    let mut cache = load_poll_cache(ctx.event_log.as_ref(), &binding.binding_id).await?;
    let config = ResolvedNotionClientConfig {
        api_base_url: std::env::var("HARN_TEST_NOTION_API_BASE_URL")
            .unwrap_or_else(|_| DEFAULT_NOTION_API_BASE_URL.to_string()),
        notion_version: DEFAULT_NOTION_API_VERSION.to_string(),
        api_token: NotionTokenSource::Secret(binding.api_token.clone()),
    };

    loop {
        if shutdown.is_stopped() {
            return Ok(());
        }

        let pages = query_pages_since(&client, &config, &binding, high_water).await?;
        let mut max_seen = high_water;
        for page in pages {
            let Some(entity_id) = page
                .get("id")
                .and_then(JsonValue::as_str)
                .map(ToString::to_string)
            else {
                continue;
            };
            let Some(last_edited_time) = page
                .get("last_edited_time")
                .and_then(JsonValue::as_str)
                .and_then(parse_rfc3339)
            else {
                continue;
            };
            if max_seen
                .map(|current| last_edited_time > current)
                .unwrap_or(true)
            {
                max_seen = Some(last_edited_time);
            }
            let before = cache.get(&entity_id).map(|entry| entry.snapshot.clone());
            if before.as_ref().is_some_and(|snapshot| snapshot == &page) {
                continue;
            }
            let dedupe_key = notion_dedupe_key(
                Some(entity_id.as_str()),
                Some(last_edited_time),
                None,
                entity_id.as_bytes(),
            );
            if already_recent_bucket(&state, &dedupe_key, last_edited_time) {
                continue;
            }
            let event = polled_event(
                &binding,
                page.clone(),
                before.clone(),
                last_edited_time,
                dedupe_key,
            );
            enqueue_binding_event(ctx.event_log.as_ref(), &binding.binding_id, event)
                .await
                .map_err(|error| ConnectorError::EventLog(error.to_string()))?;
            remember_recent_bucket(
                &state,
                &notion_bucket_for(Some(entity_id.as_str()), Some(last_edited_time), None),
                last_edited_time,
            );
            let entry = PersistedNotionPollCacheEntry {
                binding_id: binding.binding_id.clone(),
                entity_id: entity_id.clone(),
                last_edited_time,
                snapshot: page,
            };
            cache.insert(entity_id.clone(), entry.clone());
            persist_poll_cache_entry(ctx.event_log.as_ref(), &entry).await?;
        }
        if max_seen != high_water {
            if let Some(new_high_water) = max_seen {
                persist_poll_state(
                    ctx.event_log.as_ref(),
                    &PersistedNotionPollState {
                        binding_id: binding.binding_id.clone(),
                        high_water: new_high_water,
                        updated_at: OffsetDateTime::now_utc(),
                    },
                )
                .await?;
                high_water = Some(new_high_water);
            }
        }

        let sleep = tokio::time::sleep(binding.interval);
        tokio::pin!(sleep);
        tokio::select! {
            _ = &mut sleep => {}
            _ = shutdown.cancelled() => return Ok(()),
        }
    }
}

async fn query_pages_since(
    client: &NotionClient,
    config: &ResolvedNotionClientConfig,
    binding: &ActivatedNotionPollBinding,
    high_water: Option<OffsetDateTime>,
) -> Result<Vec<JsonValue>, ConnectorError> {
    let mut start_cursor = None;
    let mut results = Vec::new();
    loop {
        let mut filters = Vec::new();
        if let Some(user_filter) = binding.filter.clone() {
            filters.push(user_filter);
        }
        if let Some(high_water) = high_water {
            filters.push(json!({
                "timestamp": binding.high_water_mark,
                binding.high_water_mark.clone(): {
                    "after": high_water
                        .format(&time::format_description::well_known::Rfc3339)
                        .unwrap_or_default(),
                }
            }));
        }
        let filter = match filters.len() {
            0 => None,
            1 => filters.into_iter().next(),
            _ => Some(JsonValue::Object(
                [("and".to_string(), JsonValue::Array(filters))]
                    .into_iter()
                    .collect(),
            )),
        };
        let sorts = if binding.sorts.is_empty() {
            vec![json!({
                "timestamp": binding.high_water_mark,
                "direction": "ascending",
            })]
        } else {
            binding.sorts.clone()
        };
        let path = match binding.resource {
            NotionPollResource::DataSource => {
                format!("/data_sources/{}/query", binding.resource_id)
            }
            NotionPollResource::Database => format!("/databases/{}/query", binding.resource_id),
        };
        let mut body = JsonMap::new();
        body.insert("page_size".to_string(), JsonValue::from(binding.page_size));
        body.insert("sorts".to_string(), JsonValue::Array(sorts));
        if let Some(filter) = filter {
            body.insert("filter".to_string(), filter);
        }
        if let Some(cursor) = start_cursor.clone() {
            body.insert("start_cursor".to_string(), JsonValue::String(cursor));
        }
        let payload = client
            .request_json(config, Method::POST, &path, Some(JsonValue::Object(body)))
            .await
            .map_err(ConnectorError::from)?;
        let response: NotionListResponse =
            serde_json::from_value(payload).map_err(ConnectorError::from)?;
        results.extend(response.results);
        if !response.has_more {
            break;
        }
        start_cursor = response.next_cursor;
        if start_cursor.is_none() {
            break;
        }
    }
    Ok(results)
}

fn polled_event(
    binding: &ActivatedNotionPollBinding,
    after: JsonValue,
    before: Option<JsonValue>,
    occurred_at: OffsetDateTime,
    dedupe_key: String,
) -> TriggerEvent {
    let entity_id = after
        .get("id")
        .and_then(JsonValue::as_str)
        .map(ToString::to_string);
    let provider_payload = ProviderPayload::Known(KnownProviderPayload::Notion(Box::new(
        crate::triggers::NotionEventPayload {
            event: "page.content_updated".to_string(),
            workspace_id: None,
            request_id: None,
            subscription_id: None,
            integration_id: None,
            attempt_number: None,
            entity_id: entity_id.clone(),
            entity_type: Some("page".to_string()),
            api_version: Some(DEFAULT_NOTION_API_VERSION.to_string()),
            verification_token: None,
            polled: Some(NotionPolledChangeEvent {
                resource: binding.resource.to_string(),
                source_id: binding.resource_id.clone(),
                entity_id: entity_id.unwrap_or_default(),
                high_water_mark: binding.high_water_mark.clone(),
                before,
                after: after.clone(),
            }),
            raw: after,
        },
    )));
    TriggerEvent {
        id: TriggerEventId::new(),
        provider: ProviderId::from(NOTION_PROVIDER_ID),
        kind: "page.content_updated".to_string(),
        received_at: OffsetDateTime::now_utc(),
        occurred_at: Some(occurred_at),
        dedupe_key,
        trace_id: TraceId::new(),
        tenant_id: None,
        headers: BTreeMap::new(),
        batch: None,
        raw_body: None,
        provider_payload,
        signature_status: SignatureStatus::Unsigned,
        dedupe_claimed: false,
    }
}

fn parse_args<T: DeserializeOwned>(args: JsonValue) -> Result<T, ClientError> {
    serde_json::from_value(args).map_err(|error| ClientError::InvalidArgs(error.to_string()))
}

fn parse_poll_resource(raw: Option<&str>) -> Result<NotionPollResource, ConnectorError> {
    match raw.unwrap_or("data_source") {
        "data_source" => Ok(NotionPollResource::DataSource),
        "database" => Ok(NotionPollResource::Database),
        other => Err(ConnectorError::Activation(format!(
            "unsupported notion poll resource `{other}`; expected data_source or database"
        ))),
    }
}

fn effective_headers(headers: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut effective = headers.clone();
    for (raw, canonical) in [
        ("content-type", "Content-Type"),
        ("x-notion-signature", "X-Notion-Signature"),
        ("request-id", "request-id"),
        ("x-request-id", "x-request-id"),
    ] {
        if let Some(value) = header_value(headers, raw) {
            effective
                .entry(canonical.to_string())
                .or_insert_with(|| value.to_string());
        }
    }
    effective
}

fn header_value<'a>(headers: &'a BTreeMap<String, String>, name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
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

fn load_secret_text_blocking(
    ctx: &ConnectorCtx,
    secret_id: &SecretId,
    label: &str,
) -> Result<String, ConnectorError> {
    let secret = futures::executor::block_on(ctx.secrets.get(secret_id))?;
    secret.with_exposed(|bytes| {
        std::str::from_utf8(bytes)
            .map(|value| value.to_string())
            .map_err(|error| {
                ConnectorError::Secret(format!("{label} `{secret_id}` is not valid UTF-8: {error}"))
            })
    })
}

fn absolute_api_url(base_url: &str, path: &str) -> Result<String, ClientError> {
    let base = Url::parse(base_url).map_err(|error| ClientError::InvalidArgs(error.to_string()))?;
    base.join(path.trim_start_matches('/'))
        .map(|value| value.to_string())
        .map_err(|error| ClientError::InvalidArgs(error.to_string()))
}

fn rate_limit_backoff(response: &Response) -> StdDuration {
    response
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .map(StdDuration::from_secs)
        .unwrap_or_else(|| StdDuration::from_secs(1))
}

fn parse_rfc3339(value: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339).ok()
}

fn fallback_body_digest(body: &[u8]) -> String {
    let digest = Sha256::digest(body);
    format!("sha256:{}", hex::encode(digest))
}

fn notion_bucket_for(
    entity_id: Option<&str>,
    occurred_at: Option<OffsetDateTime>,
    fallback: Option<&str>,
) -> String {
    if let (Some(entity_id), Some(occurred_at)) = (entity_id, occurred_at) {
        let bucket = (occurred_at.unix_timestamp() / 60).to_string();
        return format!("notion:{entity_id}:{bucket}");
    }
    if let Some(fallback) = fallback {
        return format!("notion:{fallback}");
    }
    "notion:unknown".to_string()
}

fn notion_dedupe_key(
    entity_id: Option<&str>,
    occurred_at: Option<OffsetDateTime>,
    fallback: Option<&str>,
    raw_body: &[u8],
) -> String {
    let bucket = notion_bucket_for(entity_id, occurred_at, fallback);
    if bucket != "notion:unknown" {
        return bucket;
    }
    fallback_body_digest(raw_body)
}

fn remember_recent_bucket(state: &Arc<NotionConnectorState>, bucket: &str, at: OffsetDateTime) {
    let mut buckets = state
        .recent_buckets
        .lock()
        .expect("notion dedupe buckets poisoned");
    buckets.retain(|_, seen_at| at - *seen_at <= RECENT_BUCKET_RETENTION);
    buckets.insert(bucket.to_string(), at);
}

fn already_recent_bucket(
    state: &Arc<NotionConnectorState>,
    bucket: &str,
    at: OffsetDateTime,
) -> bool {
    let mut buckets = state
        .recent_buckets
        .lock()
        .expect("notion dedupe buckets poisoned");
    buckets.retain(|_, seen_at| at - *seen_at <= RECENT_BUCKET_RETENTION);
    buckets.contains_key(bucket)
}

fn persist_handshake<L: EventLog + ?Sized>(
    event_log: &L,
    handshake: &PersistedNotionWebhookHandshake,
) -> Result<(), ConnectorError> {
    let topic = Topic::new(NOTION_HANDSHAKE_TOPIC).expect("notion handshake topic is valid");
    let payload = serde_json::to_value(handshake).map_err(ConnectorError::from)?;
    futures::executor::block_on(
        event_log.append(&topic, LogEvent::new("notion.webhook.handshake", payload)),
    )
    .map_err(ConnectorError::from)?;
    Ok(())
}

pub async fn load_pending_webhook_handshakes<L: EventLog + ?Sized>(
    event_log: &L,
) -> Result<BTreeMap<String, PersistedNotionWebhookHandshake>, ConnectorError> {
    let topic = Topic::new(NOTION_HANDSHAKE_TOPIC).expect("notion handshake topic is valid");
    let events = event_log
        .read_range(&topic, None, usize::MAX)
        .await
        .map_err(ConnectorError::from)?;
    let mut handshakes = BTreeMap::new();
    for (_, event) in events {
        if event.kind != "notion.webhook.handshake" {
            continue;
        }
        let handshake: PersistedNotionWebhookHandshake =
            serde_json::from_value(event.payload).map_err(ConnectorError::from)?;
        handshakes.insert(handshake.binding_id.clone(), handshake);
    }
    Ok(handshakes)
}

async fn load_poll_state<L: EventLog + ?Sized>(
    event_log: &L,
    binding_id: &str,
) -> Result<Option<OffsetDateTime>, ConnectorError> {
    let topic = Topic::new(NOTION_POLL_STATE_TOPIC).expect("notion poll state topic is valid");
    let events = event_log
        .read_range(&topic, None, usize::MAX)
        .await
        .map_err(ConnectorError::from)?;
    let mut state = None;
    for (_, event) in events {
        if event.kind != "notion.poll.state" {
            continue;
        }
        let entry: PersistedNotionPollState =
            serde_json::from_value(event.payload).map_err(ConnectorError::from)?;
        if entry.binding_id == binding_id {
            state = Some(entry.high_water);
        }
    }
    Ok(state)
}

async fn persist_poll_state<L: EventLog + ?Sized>(
    event_log: &L,
    state: &PersistedNotionPollState,
) -> Result<(), ConnectorError> {
    let topic = Topic::new(NOTION_POLL_STATE_TOPIC).expect("notion poll state topic is valid");
    let payload = serde_json::to_value(state).map_err(ConnectorError::from)?;
    event_log
        .append(&topic, LogEvent::new("notion.poll.state", payload))
        .await
        .map_err(ConnectorError::from)?;
    Ok(())
}

async fn load_poll_cache<L: EventLog + ?Sized>(
    event_log: &L,
    binding_id: &str,
) -> Result<HashMap<String, PersistedNotionPollCacheEntry>, ConnectorError> {
    let topic = Topic::new(NOTION_POLL_CACHE_TOPIC).expect("notion poll cache topic is valid");
    let events = event_log
        .read_range(&topic, None, usize::MAX)
        .await
        .map_err(ConnectorError::from)?;
    let mut cache = HashMap::new();
    for (_, event) in events {
        if event.kind != "notion.poll.cache" {
            continue;
        }
        let entry: PersistedNotionPollCacheEntry =
            serde_json::from_value(event.payload).map_err(ConnectorError::from)?;
        if entry.binding_id == binding_id {
            cache.insert(entry.entity_id.clone(), entry);
        }
    }
    Ok(cache)
}

async fn persist_poll_cache_entry<L: EventLog + ?Sized>(
    event_log: &L,
    entry: &PersistedNotionPollCacheEntry,
) -> Result<(), ConnectorError> {
    let topic = Topic::new(NOTION_POLL_CACHE_TOPIC).expect("notion poll cache topic is valid");
    let payload = serde_json::to_value(entry).map_err(ConnectorError::from)?;
    event_log
        .append(&topic, LogEvent::new("notion.poll.cache", payload))
        .await
        .map_err(ConnectorError::from)?;
    Ok(())
}

async fn enqueue_binding_event<L: EventLog + ?Sized>(
    event_log: &L,
    binding_id: &str,
    event: TriggerEvent,
) -> Result<u64, DispatchError> {
    let topic = Topic::new(TRIGGER_INBOX_ENVELOPES_TOPIC)
        .expect("trigger inbox envelopes topic must be valid");
    let payload = serde_json::to_value(InboxEnvelope {
        trigger_id: Some(binding_id.to_string()),
        binding_version: None,
        event: event.clone(),
    })
    .map_err(|error| DispatchError::Serde(error.to_string()))?;
    let headers = BTreeMap::from([
        ("event_id".to_string(), event.id.0.clone()),
        ("trace_id".to_string(), event.trace_id.0.clone()),
        ("provider".to_string(), event.provider.as_str().to_string()),
        ("kind".to_string(), event.kind.clone()),
        ("trigger_id".to_string(), binding_id.to_string()),
    ]);
    event_log
        .append(
            &topic,
            LogEvent::new("event_ingested", payload).with_headers(headers),
        )
        .await
        .map_err(DispatchError::from)
}
