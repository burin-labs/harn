use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use reqwest::Method;
use serde::Deserialize;
use serde_json::{json, Map as JsonMap, Value as JsonValue};
use sha2::Digest;
use time::{Duration, OffsetDateTime};

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

pub const SLACK_PROVIDER_ID: &str = "slack";
const DEFAULT_API_BASE_URL: &str = "https://slack.com/api";

pub struct SlackConnector {
    provider_id: ProviderId,
    kinds: Vec<TriggerKind>,
    state: Arc<SlackConnectorState>,
    client: Arc<SlackClient>,
}

#[derive(Default)]
struct SlackConnectorState {
    ctx: RwLock<Option<ConnectorCtx>>,
    bindings: RwLock<HashMap<String, ActivatedSlackBinding>>,
}

#[derive(Clone, Debug)]
struct ActivatedSlackBinding {
    #[allow(dead_code)]
    binding_id: String,
    path: Option<String>,
    signing_secret: SecretId,
}

struct SlackClient {
    provider_id: ProviderId,
    state: Arc<SlackConnectorState>,
    http: reqwest::Client,
}

#[allow(dead_code)]
#[derive(Debug)]
enum ParsedSlackEvent {
    Message(MessageEvent),
    AppMention(AppMentionEvent),
    ReactionAdded(ReactionAddedEvent),
    AppHomeOpened(AppHomeOpenedEvent),
    AssistantThreadStarted(AssistantThreadStartedEvent),
    Other { kind: String, raw: JsonValue },
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct MessageEvent {
    channel: Option<String>,
    user: Option<String>,
    text: Option<String>,
    ts: Option<String>,
    thread_ts: Option<String>,
    subtype: Option<String>,
    channel_type: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct AppMentionEvent {
    channel: Option<String>,
    user: Option<String>,
    text: Option<String>,
    ts: Option<String>,
    thread_ts: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct ReactionAddedEvent {
    reaction: Option<String>,
    item_user: Option<String>,
    item: JsonValue,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct AppHomeOpenedEvent {
    user: Option<String>,
    channel: Option<String>,
    tab: Option<String>,
    view: Option<JsonValue>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct AssistantThreadStartedEvent {
    assistant_thread: JsonValue,
}

#[derive(Debug, Default, Deserialize)]
struct SlackBindingConfig {
    #[serde(default, rename = "match")]
    match_config: SlackMatchConfig,
    #[serde(default)]
    secrets: SlackSecretsConfig,
}

#[derive(Debug, Default, Deserialize)]
struct SlackMatchConfig {
    path: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct SlackSecretsConfig {
    signing_secret: Option<String>,
    bot_token: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct SlackClientConfigArgs {
    api_base_url: Option<String>,
    bot_token: Option<String>,
    bot_token_secret: Option<String>,
    #[serde(default)]
    secrets: SlackSecretsConfig,
}

#[derive(Clone, Debug)]
struct ResolvedSlackClientConfig {
    api_base_url: String,
    bot_token: BotTokenSource,
}

#[derive(Clone, Debug)]
enum BotTokenSource {
    Inline(String),
    Secret(SecretId),
}

#[derive(Debug, Deserialize)]
struct PostMessageArgs {
    #[serde(flatten)]
    config: SlackClientConfigArgs,
    channel: String,
    text: String,
    #[serde(default)]
    blocks: Option<JsonValue>,
    #[serde(default)]
    thread_ts: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UpdateMessageArgs {
    #[serde(flatten)]
    config: SlackClientConfigArgs,
    channel: String,
    ts: String,
    text: String,
    #[serde(default)]
    blocks: Option<JsonValue>,
}

#[derive(Debug, Deserialize)]
struct AddReactionArgs {
    #[serde(flatten)]
    config: SlackClientConfigArgs,
    channel: String,
    ts: String,
    name: String,
}

#[derive(Debug, Deserialize)]
struct OpenViewArgs {
    #[serde(flatten)]
    config: SlackClientConfigArgs,
    trigger_id: String,
    view: JsonValue,
}

#[derive(Debug, Deserialize)]
struct UserInfoArgs {
    #[serde(flatten)]
    config: SlackClientConfigArgs,
    user_id: String,
    #[serde(default)]
    include_locale: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct ApiCallArgs {
    #[serde(flatten)]
    config: SlackClientConfigArgs,
    method: String,
    #[serde(default)]
    args: JsonValue,
}

#[derive(Debug, Deserialize)]
struct UploadFileArgs {
    #[serde(flatten)]
    config: SlackClientConfigArgs,
    filename: String,
    content: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    channel_id: Option<String>,
    #[serde(default)]
    initial_comment: Option<String>,
    #[serde(default)]
    thread_ts: Option<String>,
    #[serde(default)]
    alt_txt: Option<String>,
    #[serde(default)]
    snippet_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SlackApiEnvelope {
    ok: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(flatten)]
    rest: JsonMap<String, JsonValue>,
}

#[derive(Debug, Deserialize)]
struct SlackUploadUrlResponse {
    ok: bool,
    #[serde(default)]
    error: Option<String>,
    upload_url: Option<String>,
    file_id: Option<String>,
    #[allow(dead_code)]
    #[serde(flatten)]
    rest: JsonMap<String, JsonValue>,
}

impl SlackConnector {
    pub fn new() -> Self {
        let state = Arc::new(SlackConnectorState::default());
        let client = Arc::new(SlackClient {
            provider_id: ProviderId::from(SLACK_PROVIDER_ID),
            state: state.clone(),
            http: reqwest::Client::builder()
                .user_agent("harn-slack-connector")
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
        });
        Self {
            provider_id: ProviderId::from(SLACK_PROVIDER_ID),
            kinds: vec![TriggerKind::from("webhook")],
            state,
            client,
        }
    }

    fn binding_for_raw(&self, raw: &RawInbound) -> Result<ActivatedSlackBinding, ConnectorError> {
        let bindings = self
            .state
            .bindings
            .read()
            .expect("slack connector bindings poisoned");
        if let Some(binding_id) = raw.metadata.get("binding_id").and_then(JsonValue::as_str) {
            return bindings.get(binding_id).cloned().ok_or_else(|| {
                ConnectorError::Unsupported(format!(
                    "slack connector has no active binding `{binding_id}`"
                ))
            });
        }
        if bindings.len() == 1 {
            return bindings
                .values()
                .next()
                .cloned()
                .ok_or_else(|| ConnectorError::Activation("slack bindings missing".to_string()));
        }
        Err(ConnectorError::Unsupported(
            "slack connector requires raw.metadata.binding_id when multiple bindings are active"
                .to_string(),
        ))
    }

    fn ctx(&self) -> Result<ConnectorCtx, ConnectorError> {
        self.state
            .ctx
            .read()
            .expect("slack connector ctx poisoned")
            .clone()
            .ok_or_else(|| {
                ConnectorError::Activation(
                    "slack connector must be initialized before use".to_string(),
                )
            })
    }
}

impl Default for SlackConnector {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Connector for SlackConnector {
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
            .expect("slack connector ctx poisoned") = Some(ctx);
        Ok(())
    }

    async fn activate(
        &self,
        bindings: &[TriggerBinding],
    ) -> Result<ActivationHandle, ConnectorError> {
        let mut configured = HashMap::new();
        let mut paths = BTreeSet::new();
        for binding in bindings {
            let activated = ActivatedSlackBinding::from_binding(binding)?;
            if let Some(path) = &activated.path {
                if !paths.insert(path.clone()) {
                    return Err(ConnectorError::Activation(format!(
                        "slack connector path `{path}` is configured by multiple bindings"
                    )));
                }
            }
            configured.insert(binding.binding_id.clone(), activated);
        }
        *self
            .state
            .bindings
            .write()
            .expect("slack connector bindings poisoned") = configured;
        Ok(ActivationHandle::new(
            self.provider_id.clone(),
            bindings.len(),
        ))
    }

    async fn normalize_inbound(&self, raw: RawInbound) -> Result<TriggerEvent, ConnectorError> {
        let ctx = self.ctx()?;
        let binding = self.binding_for_raw(&raw)?;
        let provider = self.provider_id.clone();
        let received_at = raw.received_at;
        let headers = effective_headers(&raw.headers);
        let secret = load_secret_text_blocking(&ctx, &binding.signing_secret)?;
        futures::executor::block_on(crate::connectors::hmac::verify_hmac_signed(
            ctx.event_log.as_ref(),
            &provider,
            HmacSignatureStyle::slack(),
            &raw.body,
            &headers,
            secret.as_str(),
            Some(Duration::minutes(5)),
            received_at,
        ))?;

        let payload = raw.json_body()?;
        let event_kind = slack_event_kind(&payload, &raw)?;
        let _typed = parse_typed_event(&event_kind, &payload)?;
        let dedupe_key = payload
            .get("event_id")
            .and_then(JsonValue::as_str)
            .map(ToString::to_string)
            .unwrap_or_else(|| fallback_body_digest(&raw.body));
        let provider_payload =
            ProviderPayload::normalize(&provider, &event_kind, &headers, payload)
                .map_err(|error| ConnectorError::Unsupported(error.to_string()))?;
        Ok(TriggerEvent {
            id: TriggerEventId::new(),
            provider,
            kind: event_kind,
            received_at,
            occurred_at: raw
                .occurred_at
                .or_else(|| infer_occurred_at(&provider_payload)),
            dedupe_key,
            trace_id: TraceId::new(),
            tenant_id: raw.tenant_id.clone(),
            headers: redact_headers(&headers, &HeaderRedactionPolicy::default()),
            batch: None,
            provider_payload,
            signature_status: SignatureStatus::Verified,
            dedupe_claimed: false,
        })
    }

    fn payload_schema(&self) -> ProviderPayloadSchema {
        ProviderPayloadSchema::named("SlackEventPayload")
    }

    fn client(&self) -> Arc<dyn ConnectorClient> {
        self.client.clone()
    }
}

#[async_trait]
impl ConnectorClient for SlackClient {
    async fn call(&self, method: &str, args: JsonValue) -> Result<JsonValue, ClientError> {
        match method {
            "post_message" => {
                let args: PostMessageArgs = parse_args(args)?;
                let config = self.resolve_client_config(&args.config)?;
                let mut body = JsonMap::new();
                body.insert("channel".to_string(), JsonValue::String(args.channel));
                body.insert("text".to_string(), JsonValue::String(args.text));
                if let Some(blocks) = args.blocks {
                    body.insert("blocks".to_string(), blocks);
                }
                if let Some(thread_ts) = args.thread_ts {
                    body.insert("thread_ts".to_string(), JsonValue::String(thread_ts));
                }
                self.request_json(&config, "chat.postMessage", JsonValue::Object(body))
                    .await
            }
            "update_message" => {
                let args: UpdateMessageArgs = parse_args(args)?;
                let config = self.resolve_client_config(&args.config)?;
                let mut body = JsonMap::new();
                body.insert("channel".to_string(), JsonValue::String(args.channel));
                body.insert("ts".to_string(), JsonValue::String(args.ts));
                body.insert("text".to_string(), JsonValue::String(args.text));
                if let Some(blocks) = args.blocks {
                    body.insert("blocks".to_string(), blocks);
                }
                self.request_json(&config, "chat.update", JsonValue::Object(body))
                    .await
            }
            "add_reaction" => {
                let args: AddReactionArgs = parse_args(args)?;
                let config = self.resolve_client_config(&args.config)?;
                self.request_json(
                    &config,
                    "reactions.add",
                    json!({
                        "channel": args.channel,
                        "timestamp": args.ts,
                        "name": args.name,
                    }),
                )
                .await
            }
            "open_view" => {
                let args: OpenViewArgs = parse_args(args)?;
                let config = self.resolve_client_config(&args.config)?;
                self.request_json(
                    &config,
                    "views.open",
                    json!({
                        "trigger_id": args.trigger_id,
                        "view": args.view,
                    }),
                )
                .await
            }
            "user_info" => {
                let args: UserInfoArgs = parse_args(args)?;
                let config = self.resolve_client_config(&args.config)?;
                let mut query = vec![("user".to_string(), args.user_id)];
                if let Some(include_locale) = args.include_locale {
                    query.push((
                        "include_locale".to_string(),
                        if include_locale { "true" } else { "false" }.to_string(),
                    ));
                }
                self.request_query_json(&config, "users.info", &query).await
            }
            "api_call" => {
                let args: ApiCallArgs = parse_args(args)?;
                let config = self.resolve_client_config(&args.config)?;
                let body = match args.args {
                    JsonValue::Null => JsonValue::Object(JsonMap::new()),
                    JsonValue::Object(_) => args.args,
                    _ => {
                        return Err(ClientError::InvalidArgs(
                            "slack api_call args must be a JSON object".to_string(),
                        ));
                    }
                };
                self.request_json(&config, &args.method, body).await
            }
            "upload_file" => {
                let args: UploadFileArgs = parse_args(args)?;
                let config = self.resolve_client_config(&args.config)?;
                self.upload_file(&config, args).await
            }
            other => Err(ClientError::MethodNotFound(format!(
                "slack connector does not implement outbound method `{other}`"
            ))),
        }
    }
}

impl SlackClient {
    fn resolve_client_config(
        &self,
        args: &SlackClientConfigArgs,
    ) -> Result<ResolvedSlackClientConfig, ClientError> {
        let bot_token = if let Some(secret_id) = args
            .bot_token_secret
            .as_deref()
            .or(args.secrets.bot_token.as_deref())
            .and_then(|value| parse_secret_id(Some(value)))
        {
            BotTokenSource::Secret(secret_id)
        } else if let Some(token) = args.bot_token.clone() {
            BotTokenSource::Inline(token)
        } else {
            return Err(ClientError::InvalidArgs(
                "slack connector requires bot_token or bot_token_secret".to_string(),
            ));
        };
        Ok(ResolvedSlackClientConfig {
            api_base_url: args
                .api_base_url
                .clone()
                .unwrap_or_else(|| DEFAULT_API_BASE_URL.to_string()),
            bot_token,
        })
    }

    fn ctx(&self) -> Result<ConnectorCtx, ClientError> {
        self.state
            .ctx
            .read()
            .expect("slack connector ctx poisoned")
            .clone()
            .ok_or_else(|| ClientError::Other("slack connector must be initialized".to_string()))
    }

    async fn request_json(
        &self,
        config: &ResolvedSlackClientConfig,
        method: &str,
        body: JsonValue,
    ) -> Result<JsonValue, ClientError> {
        let response = self
            .request(Method::POST, method, config, Some(body))
            .await?;
        self.decode_api_response(method, response).await
    }

    async fn request_query_json(
        &self,
        config: &ResolvedSlackClientConfig,
        method: &str,
        query: &[(String, String)],
    ) -> Result<JsonValue, ClientError> {
        let response = self.request_query(method, config, query).await?;
        self.decode_api_response(method, response).await
    }

    async fn decode_api_response(
        &self,
        method: &str,
        response: reqwest::Response,
    ) -> Result<JsonValue, ClientError> {
        let status = response.status();
        let payload = response
            .json::<SlackApiEnvelope>()
            .await
            .map_err(|error| ClientError::Transport(error.to_string()))?;
        if !status.is_success() {
            let message = payload
                .error
                .unwrap_or_else(|| format!("Slack API request failed with status {status}"));
            return Err(ClientError::Transport(message));
        }
        if !payload.ok {
            return Err(ClientError::Transport(
                payload
                    .error
                    .unwrap_or_else(|| format!("Slack API method `{method}` failed")),
            ));
        }
        let mut body = JsonValue::Object(payload.rest);
        if let Some(object) = body.as_object_mut() {
            object.insert("ok".to_string(), JsonValue::Bool(true));
        }
        Ok(body)
    }

    async fn request(
        &self,
        http_method: Method,
        method: &str,
        config: &ResolvedSlackClientConfig,
        body: Option<JsonValue>,
    ) -> Result<reqwest::Response, ClientError> {
        let ctx = self.ctx()?;
        ctx.rate_limiter
            .scoped(&self.provider_id, "bot")
            .acquire()
            .await;
        let token = self.bot_token(config).await?;
        let url = format!(
            "{}/{}",
            config.api_base_url.trim_end_matches('/'),
            method.trim_start_matches('/')
        );
        let mut request = self
            .http
            .request(http_method, url)
            .header("Authorization", format!("Bearer {token}"));
        if let Some(body) = body {
            request = request.json(&body);
        }
        request
            .send()
            .await
            .map_err(|error| ClientError::Transport(error.to_string()))
    }

    async fn request_query(
        &self,
        method: &str,
        config: &ResolvedSlackClientConfig,
        query: &[(String, String)],
    ) -> Result<reqwest::Response, ClientError> {
        let ctx = self.ctx()?;
        ctx.rate_limiter
            .scoped(&self.provider_id, "bot")
            .acquire()
            .await;
        let token = self.bot_token(config).await?;
        let url = format!(
            "{}/{}",
            config.api_base_url.trim_end_matches('/'),
            method.trim_start_matches('/')
        );
        self.http
            .request(Method::GET, url)
            .header("Authorization", format!("Bearer {token}"))
            .query(query)
            .send()
            .await
            .map_err(|error| ClientError::Transport(error.to_string()))
    }

    async fn bot_token(&self, config: &ResolvedSlackClientConfig) -> Result<String, ClientError> {
        match &config.bot_token {
            BotTokenSource::Inline(token) => Ok(token.clone()),
            BotTokenSource::Secret(secret_id) => {
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

    async fn upload_file(
        &self,
        config: &ResolvedSlackClientConfig,
        args: UploadFileArgs,
    ) -> Result<JsonValue, ClientError> {
        let token = self.bot_token(config).await?;
        let ctx = self.ctx()?;
        ctx.rate_limiter
            .scoped(&self.provider_id, "bot")
            .acquire()
            .await;

        let start_url = format!(
            "{}/files.getUploadURLExternal",
            config.api_base_url.trim_end_matches('/')
        );
        let mut form = vec![
            ("filename".to_string(), args.filename.clone()),
            ("length".to_string(), args.content.len().to_string()),
        ];
        if let Some(alt_txt) = args.alt_txt.clone() {
            form.push(("alt_txt".to_string(), alt_txt));
        }
        if let Some(snippet_type) = args.snippet_type.clone() {
            form.push(("snippet_type".to_string(), snippet_type));
        }
        let start = self
            .http
            .request(Method::POST, start_url)
            .header("Authorization", format!("Bearer {token}"))
            .form(&form);
        let start_response = start
            .send()
            .await
            .map_err(|error| ClientError::Transport(error.to_string()))?;
        let start_status = start_response.status();
        let start_payload = start_response
            .json::<SlackUploadUrlResponse>()
            .await
            .map_err(|error| ClientError::Transport(error.to_string()))?;
        if !start_status.is_success() || !start_payload.ok {
            return Err(ClientError::Transport(start_payload.error.unwrap_or_else(
                || "Slack API method `files.getUploadURLExternal` failed".to_string(),
            )));
        }
        let upload_url = start_payload.upload_url.ok_or_else(|| {
            ClientError::Transport(
                "Slack API response missing upload_url for files.getUploadURLExternal".to_string(),
            )
        })?;
        let file_id = start_payload.file_id.ok_or_else(|| {
            ClientError::Transport(
                "Slack API response missing file_id for files.getUploadURLExternal".to_string(),
            )
        })?;
        self.http
            .request(Method::POST, upload_url)
            .header("Content-Type", "application/octet-stream")
            .body(args.content.into_bytes())
            .send()
            .await
            .map_err(|error| ClientError::Transport(error.to_string()))?
            .error_for_status()
            .map_err(|error| ClientError::Transport(error.to_string()))?;

        let mut files = JsonMap::new();
        files.insert("id".to_string(), JsonValue::String(file_id.clone()));
        files.insert(
            "title".to_string(),
            JsonValue::String(args.title.unwrap_or_else(|| args.filename.clone())),
        );
        let mut body = JsonMap::new();
        body.insert(
            "files".to_string(),
            JsonValue::Array(vec![JsonValue::Object(files)]),
        );
        if let Some(channel_id) = args.channel_id {
            body.insert("channel_id".to_string(), JsonValue::String(channel_id));
        }
        if let Some(initial_comment) = args.initial_comment {
            body.insert(
                "initial_comment".to_string(),
                JsonValue::String(initial_comment),
            );
        }
        if let Some(thread_ts) = args.thread_ts {
            body.insert("thread_ts".to_string(), JsonValue::String(thread_ts));
        }
        self.request_json(
            config,
            "files.completeUploadExternal",
            JsonValue::Object(body),
        )
        .await
        .map(|mut value| {
            if let Some(object) = value.as_object_mut() {
                object.insert("file_id".to_string(), JsonValue::String(file_id));
            }
            value
        })
    }
}

impl ActivatedSlackBinding {
    fn from_binding(binding: &TriggerBinding) -> Result<Self, ConnectorError> {
        let config: SlackBindingConfig =
            serde_json::from_value(binding.config.clone()).map_err(|error| {
                ConnectorError::Activation(format!(
                    "slack binding `{}` has invalid config: {error}",
                    binding.binding_id
                ))
            })?;
        let signing_secret =
            parse_secret_id(config.secrets.signing_secret.as_deref()).ok_or_else(|| {
                ConnectorError::Activation(format!(
                    "slack binding `{}` requires secrets.signing_secret",
                    binding.binding_id
                ))
            })?;
        Ok(Self {
            binding_id: binding.binding_id.clone(),
            path: config.match_config.path,
            signing_secret,
        })
    }
}

fn effective_headers(
    headers: &std::collections::BTreeMap<String, String>,
) -> std::collections::BTreeMap<String, String> {
    let mut effective = headers.clone();
    for (raw, canonical) in [
        ("content-type", "Content-Type"),
        ("x-slack-signature", "X-Slack-Signature"),
        ("x-slack-request-timestamp", "X-Slack-Request-Timestamp"),
        ("x-slack-retry-num", "X-Slack-Retry-Num"),
        ("x-slack-retry-reason", "X-Slack-Retry-Reason"),
    ] {
        if let Some(value) = header_value(headers, raw) {
            effective
                .entry(canonical.to_string())
                .or_insert_with(|| value.to_string());
        }
    }
    effective
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
                    "slack signing secret `{secret_id}` is not valid UTF-8: {error}"
                ))
            })
    })
}

fn slack_event_kind(payload: &JsonValue, raw: &RawInbound) -> Result<String, ConnectorError> {
    if !raw.kind.trim().is_empty() {
        return Ok(raw.kind.clone());
    }
    let kind = payload
        .get("type")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| {
            ConnectorError::Unsupported("slack payload missing top-level type".to_string())
        })?;
    if kind == "url_verification" {
        return Ok("url_verification".to_string());
    }
    if kind != "event_callback" {
        return Ok(kind.to_string());
    }
    let event = payload.get("event").ok_or_else(|| {
        ConnectorError::Unsupported("slack event_callback missing event".to_string())
    })?;
    let event_type = event
        .get("type")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| ConnectorError::Unsupported("slack event missing type".to_string()))?;
    if event_type == "message" {
        let channel_type = event
            .get("channel_type")
            .and_then(JsonValue::as_str)
            .unwrap_or_default();
        if !channel_type.is_empty() {
            let kind = match channel_type {
                "channel" => "message.channels",
                "group" => "message.groups",
                "im" => "message.im",
                "mpim" => "message.mpim",
                "app_home" => "message.app_home",
                other => return Ok(format!("message.{other}")),
            };
            return Ok(kind.to_string());
        }
    }
    Ok(event_type.to_string())
}

fn parse_typed_event(kind: &str, payload: &JsonValue) -> Result<ParsedSlackEvent, ConnectorError> {
    if kind == "url_verification" {
        return Ok(ParsedSlackEvent::Other {
            kind: kind.to_string(),
            raw: payload.clone(),
        });
    }
    let event = payload.get("event").cloned().unwrap_or(JsonValue::Null);
    match kind {
        kind if kind == "message" || kind.starts_with("message.") => serde_json::from_value(event)
            .map(ParsedSlackEvent::Message)
            .map_err(|error| ConnectorError::Json(error.to_string())),
        "app_mention" => serde_json::from_value(event)
            .map(ParsedSlackEvent::AppMention)
            .map_err(|error| ConnectorError::Json(error.to_string())),
        "reaction_added" => serde_json::from_value(event)
            .map(ParsedSlackEvent::ReactionAdded)
            .map_err(|error| ConnectorError::Json(error.to_string())),
        "app_home_opened" => serde_json::from_value(event)
            .map(ParsedSlackEvent::AppHomeOpened)
            .map_err(|error| ConnectorError::Json(error.to_string())),
        "assistant_thread_started" => serde_json::from_value(event)
            .map(ParsedSlackEvent::AssistantThreadStarted)
            .map_err(|error| ConnectorError::Json(error.to_string())),
        _ => Ok(ParsedSlackEvent::Other {
            kind: kind.to_string(),
            raw: payload.clone(),
        }),
    }
}

fn infer_occurred_at(provider_payload: &ProviderPayload) -> Option<OffsetDateTime> {
    let ProviderPayload::Known(payload) = provider_payload else {
        return None;
    };
    let raw = match payload {
        crate::triggers::event::KnownProviderPayload::Slack(payload) => slack_raw(payload),
        _ => return None,
    };
    raw.get("event")
        .and_then(|event| event.get("event_ts"))
        .and_then(JsonValue::as_str)
        .and_then(parse_slack_timestamp)
        .or_else(|| {
            raw.get("event_time")
                .and_then(JsonValue::as_i64)
                .and_then(|secs| OffsetDateTime::from_unix_timestamp(secs).ok())
        })
}

fn slack_raw(payload: &crate::triggers::event::SlackEventPayload) -> &JsonValue {
    match payload {
        crate::triggers::event::SlackEventPayload::Message(inner) => &inner.common.raw,
        crate::triggers::event::SlackEventPayload::AppMention(inner) => &inner.common.raw,
        crate::triggers::event::SlackEventPayload::ReactionAdded(inner) => &inner.common.raw,
        crate::triggers::event::SlackEventPayload::AppHomeOpened(inner) => &inner.common.raw,
        crate::triggers::event::SlackEventPayload::AssistantThreadStarted(inner) => {
            &inner.common.raw
        }
        crate::triggers::event::SlackEventPayload::Other(common) => &common.raw,
    }
}

fn parse_slack_timestamp(raw: &str) -> Option<OffsetDateTime> {
    let seconds = raw.split('.').next()?.parse::<i64>().ok()?;
    OffsetDateTime::from_unix_timestamp(seconds).ok()
}

fn header_value<'a>(
    headers: &'a std::collections::BTreeMap<String, String>,
    name: &str,
) -> Option<&'a str> {
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

fn fallback_body_digest(body: &[u8]) -> String {
    let digest = sha2::Sha256::digest(body);
    format!("sha256:{}", hex::encode(digest))
}
