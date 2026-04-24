use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::{Arc, Mutex, RwLock};

use async_trait::async_trait;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use reqwest::{Method, Response, StatusCode, Url};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map as JsonMap, Value as JsonValue};
use sha2::{Digest, Sha256};
use time::{Duration, OffsetDateTime};

use crate::connectors::{
    ActivationHandle, ClientError, Connector, ConnectorClient, ConnectorCtx, ConnectorError,
    HmacSignatureStyle, ProviderPayloadSchema, RawInbound, TriggerBinding, TriggerKind,
};
use crate::event_log::{EventLog, LogEvent, Topic};
use crate::secrets::{SecretBytes, SecretId, SecretVersion};
use crate::triggers::{
    redact_headers, HeaderRedactionPolicy, ProviderId, ProviderPayload, SignatureStatus, TraceId,
    TriggerEvent, TriggerEventId,
};

#[cfg(test)]
mod tests;

pub const GITHUB_PROVIDER_ID: &str = "github";
const GITHUB_RATE_LIMIT_TOPIC: &str = "connectors.github.rate_limit";
const GITHUB_API_VERSION: &str = "2022-11-28";
const DEFAULT_API_BASE_URL: &str = "https://api.github.com";

pub struct GitHubConnector {
    provider_id: ProviderId,
    kinds: Vec<TriggerKind>,
    state: Arc<GitHubConnectorState>,
    client: Arc<GitHubClient>,
}

#[derive(Default)]
struct GitHubConnectorState {
    ctx: RwLock<Option<ConnectorCtx>>,
    bindings: RwLock<HashMap<String, ActivatedGitHubBinding>>,
}

#[derive(Clone, Debug)]
struct ActivatedGitHubBinding {
    binding_id: String,
    path: Option<String>,
    signing_secret: SecretId,
    dedupe_enabled: bool,
    dedupe_ttl: std::time::Duration,
}

struct GitHubClient {
    provider_id: ProviderId,
    state: Arc<GitHubConnectorState>,
    http: reqwest::Client,
    tokens: GitHubInstallationTokenStore,
}

struct GitHubInstallationTokenStore {
    capacity: usize,
    state: Mutex<TokenCacheState>,
}

#[derive(Default)]
struct TokenCacheState {
    entries: HashMap<u64, InstallationTokenEntry>,
    order: VecDeque<u64>,
}

struct InstallationTokenEntry {
    token: SecretBytes,
    refresh_at: OffsetDateTime,
}

#[derive(Clone, Debug)]
struct ResolvedGitHubClientConfig {
    app_id: u64,
    installation_id: u64,
    api_base_url: String,
    private_key: PrivateKeySource,
}

#[derive(Clone, Debug)]
enum PrivateKeySource {
    Inline(String),
    Secret(SecretId),
}

#[allow(dead_code)]
#[derive(Debug)]
enum ParsedGitHubEvent {
    Issues(IssuesEvent),
    PullRequest(PullRequestEvent),
    IssueComment(IssueCommentEvent),
    PullRequestReview(PullRequestReviewEvent),
    Push(PushEvent),
    WorkflowRun(WorkflowRunEvent),
    DeploymentStatus(DeploymentStatusEvent),
    CheckRun(CheckRunEvent),
    Other { kind: String, raw: JsonValue },
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct IssuesEvent {
    action: Option<String>,
    issue: JsonValue,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct PullRequestEvent {
    action: Option<String>,
    pull_request: JsonValue,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct IssueCommentEvent {
    action: Option<String>,
    comment: JsonValue,
    issue: JsonValue,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct PullRequestReviewEvent {
    action: Option<String>,
    review: JsonValue,
    pull_request: JsonValue,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct PushEvent {
    #[serde(default)]
    commits: Vec<JsonValue>,
    #[serde(default)]
    distinct_size: Option<u64>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct WorkflowRunEvent {
    action: Option<String>,
    workflow_run: JsonValue,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct DeploymentStatusEvent {
    action: Option<String>,
    deployment_status: JsonValue,
    deployment: JsonValue,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct CheckRunEvent {
    action: Option<String>,
    check_run: JsonValue,
}

#[derive(Debug, Default, Deserialize)]
struct GitHubBindingConfig {
    #[serde(default, rename = "match")]
    match_config: GitHubMatchConfig,
    #[serde(default)]
    secrets: GitHubSecretsConfig,
}

#[derive(Debug, Default, Deserialize)]
struct GitHubMatchConfig {
    path: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct GitHubSecretsConfig {
    signing_secret: Option<String>,
    private_key: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct GitHubClientConfigArgs {
    app_id: Option<u64>,
    installation_id: Option<u64>,
    api_base_url: Option<String>,
    private_key_pem: Option<String>,
    private_key_secret: Option<String>,
    #[serde(default)]
    secrets: GitHubSecretsConfig,
}

#[derive(Debug, Deserialize)]
struct CommentArgs {
    #[serde(flatten)]
    config: GitHubClientConfigArgs,
    issue_url: String,
    body: String,
}

#[derive(Debug, Deserialize)]
struct AddLabelsArgs {
    #[serde(flatten)]
    config: GitHubClientConfigArgs,
    issue_url: String,
    labels: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct RequestReviewArgs {
    #[serde(flatten)]
    config: GitHubClientConfigArgs,
    pr_url: String,
    reviewers: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct MergePrArgs {
    #[serde(flatten)]
    config: GitHubClientConfigArgs,
    pr_url: String,
    #[serde(default)]
    commit_title: Option<String>,
    #[serde(default)]
    commit_message: Option<String>,
    #[serde(default)]
    merge_method: Option<String>,
    #[serde(default)]
    sha: Option<String>,
    #[serde(default)]
    admin_override: bool,
}

#[derive(Debug, Deserialize)]
struct ListStalePrsArgs {
    #[serde(flatten)]
    config: GitHubClientConfigArgs,
    repo: String,
    days: i64,
}

#[derive(Debug, Deserialize)]
struct GetPrDiffArgs {
    #[serde(flatten)]
    config: GitHubClientConfigArgs,
    pr_url: String,
}

#[derive(Debug, Deserialize)]
struct CreateIssueArgs {
    #[serde(flatten)]
    config: GitHubClientConfigArgs,
    repo: String,
    title: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    labels: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct ApiCallArgs {
    #[serde(flatten)]
    config: GitHubClientConfigArgs,
    path: String,
    method: String,
    #[serde(default)]
    body: Option<JsonValue>,
    #[serde(default)]
    accept: Option<String>,
}

#[derive(Debug, Deserialize)]
struct InstallationTokenResponse {
    token: String,
    #[serde(default)]
    expires_at: Option<String>,
}

#[derive(Debug, Serialize)]
struct GitHubJwtClaims {
    iat: i64,
    exp: i64,
    iss: String,
}

#[derive(Clone, Debug)]
struct RepoRef {
    owner: String,
    repo: String,
}

#[derive(Clone, Debug)]
struct IssueRef {
    repo: RepoRef,
    number: u64,
}

impl GitHubConnector {
    pub fn new() -> Self {
        let state = Arc::new(GitHubConnectorState::default());
        let client = Arc::new(GitHubClient {
            provider_id: ProviderId::from(GITHUB_PROVIDER_ID),
            state: state.clone(),
            http: crate::connectors::outbound_http_client("harn-github-connector"),
            tokens: GitHubInstallationTokenStore::new(32),
        });
        Self {
            provider_id: ProviderId::from(GITHUB_PROVIDER_ID),
            kinds: vec![TriggerKind::from("webhook")],
            state,
            client,
        }
    }

    fn binding_for_raw(&self, raw: &RawInbound) -> Result<ActivatedGitHubBinding, ConnectorError> {
        let bindings = self
            .state
            .bindings
            .read()
            .expect("github connector bindings poisoned");
        if let Some(binding_id) = raw.metadata.get("binding_id").and_then(JsonValue::as_str) {
            return bindings.get(binding_id).cloned().ok_or_else(|| {
                ConnectorError::Unsupported(format!(
                    "github connector has no active binding `{binding_id}`"
                ))
            });
        }
        if bindings.len() == 1 {
            return bindings
                .values()
                .next()
                .cloned()
                .ok_or_else(|| ConnectorError::Activation("github bindings missing".to_string()));
        }
        Err(ConnectorError::Unsupported(
            "github connector requires raw.metadata.binding_id when multiple bindings are active"
                .to_string(),
        ))
    }

    fn ctx(&self) -> Result<ConnectorCtx, ConnectorError> {
        self.state
            .ctx
            .read()
            .expect("github connector ctx poisoned")
            .clone()
            .ok_or_else(|| {
                ConnectorError::Activation(
                    "github connector must be initialized before use".to_string(),
                )
            })
    }
}

impl Default for GitHubConnector {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Connector for GitHubConnector {
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
            .expect("github connector ctx poisoned") = Some(ctx);
        Ok(())
    }

    async fn activate(
        &self,
        bindings: &[TriggerBinding],
    ) -> Result<ActivationHandle, ConnectorError> {
        let mut configured = HashMap::new();
        let mut paths = BTreeSet::new();
        for binding in bindings {
            let activated = ActivatedGitHubBinding::from_binding(binding)?;
            if let Some(path) = &activated.path {
                if !paths.insert(path.clone()) {
                    return Err(ConnectorError::Activation(format!(
                        "github connector path `{path}` is configured by multiple bindings"
                    )));
                }
            }
            configured.insert(binding.binding_id.clone(), activated);
        }
        *self
            .state
            .bindings
            .write()
            .expect("github connector bindings poisoned") = configured;
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
            HmacSignatureStyle::github(),
            &raw.body,
            &headers,
            secret.as_str(),
            None,
            received_at,
        ))?;

        let payload = raw.json_body()?;
        let event_kind = header_value(&headers, "x-github-event")
            .map(ToString::to_string)
            .or_else(|| {
                if raw.kind.trim().is_empty() {
                    None
                } else {
                    Some(raw.kind.clone())
                }
            })
            .ok_or_else(|| ConnectorError::MissingHeader("X-GitHub-Event".to_string()))?;
        let _typed = parse_typed_event(&event_kind, &payload)?;
        let dedupe_key = header_value(&headers, "x-github-delivery")
            .map(ToString::to_string)
            .unwrap_or_else(|| fallback_body_digest(&raw.body));
        if binding.dedupe_enabled {
            let inserted = ctx
                .inbox
                .insert_if_new(&binding.binding_id, &dedupe_key, binding.dedupe_ttl)
                .await?;
            if !inserted {
                return Err(ConnectorError::DuplicateDelivery(format!(
                    "duplicate GitHub delivery `{dedupe_key}` for binding `{}`",
                    binding.binding_id
                )));
            }
        }

        let provider_payload =
            ProviderPayload::normalize(&provider, &event_kind, &headers, payload)
                .map_err(|error| ConnectorError::Unsupported(error.to_string()))?;
        Ok(TriggerEvent {
            id: TriggerEventId::new(),
            provider,
            kind: event_kind,
            received_at,
            occurred_at: raw.occurred_at,
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
        ProviderPayloadSchema::named("GitHubEventPayload")
    }

    fn client(&self) -> Arc<dyn ConnectorClient> {
        self.client.clone()
    }
}

#[async_trait]
impl ConnectorClient for GitHubClient {
    async fn call(&self, method: &str, args: JsonValue) -> Result<JsonValue, ClientError> {
        match method {
            "comment" => {
                let args: CommentArgs = parse_args(args)?;
                let config = self.resolve_client_config(&args.config)?;
                let issue = parse_issue_like_url(&args.issue_url, "issue")?;
                self.request_json(
                    &config,
                    Method::POST,
                    &format!(
                        "/repos/{}/{}/issues/{}/comments",
                        issue.repo.owner, issue.repo.repo, issue.number
                    ),
                    Some(json!({ "body": args.body })),
                    "application/vnd.github+json",
                )
                .await
            }
            "add_labels" => {
                let args: AddLabelsArgs = parse_args(args)?;
                let config = self.resolve_client_config(&args.config)?;
                let issue = parse_issue_like_url(&args.issue_url, "issue")?;
                self.request_json(
                    &config,
                    Method::POST,
                    &format!(
                        "/repos/{}/{}/issues/{}/labels",
                        issue.repo.owner, issue.repo.repo, issue.number
                    ),
                    Some(json!({ "labels": args.labels })),
                    "application/vnd.github+json",
                )
                .await
            }
            "request_review" => {
                let args: RequestReviewArgs = parse_args(args)?;
                let config = self.resolve_client_config(&args.config)?;
                let pr = parse_issue_like_url(&args.pr_url, "pull")?;
                self.request_json(
                    &config,
                    Method::POST,
                    &format!(
                        "/repos/{}/{}/pulls/{}/requested_reviewers",
                        pr.repo.owner, pr.repo.repo, pr.number
                    ),
                    Some(json!({ "reviewers": args.reviewers })),
                    "application/vnd.github+json",
                )
                .await
            }
            "merge_pr" => {
                let args: MergePrArgs = parse_args(args)?;
                let config = self.resolve_client_config(&args.config)?;
                let pr = parse_issue_like_url(&args.pr_url, "pull")?;
                let mut body = JsonMap::new();
                if let Some(value) = args.commit_title {
                    body.insert("commit_title".to_string(), JsonValue::String(value));
                }
                if let Some(value) = args.commit_message {
                    body.insert("commit_message".to_string(), JsonValue::String(value));
                }
                if let Some(value) = args.merge_method {
                    body.insert("merge_method".to_string(), JsonValue::String(value));
                }
                if let Some(value) = args.sha {
                    body.insert("sha".to_string(), JsonValue::String(value));
                }
                let mut response = self
                    .request_json(
                        &config,
                        Method::PUT,
                        &format!(
                            "/repos/{}/{}/pulls/{}/merge",
                            pr.repo.owner, pr.repo.repo, pr.number
                        ),
                        Some(JsonValue::Object(body)),
                        "application/vnd.github+json",
                    )
                    .await?;
                if args.admin_override {
                    if let Some(map) = response.as_object_mut() {
                        map.insert(
                            "admin_override_requested".to_string(),
                            JsonValue::Bool(true),
                        );
                    }
                }
                Ok(response)
            }
            "list_stale_prs" => {
                let args: ListStalePrsArgs = parse_args(args)?;
                let config = self.resolve_client_config(&args.config)?;
                let repo = parse_repo_ref(&args.repo)?;
                let stale_before = (OffsetDateTime::now_utc() - Duration::days(args.days))
                    .date()
                    .to_string();
                let query = format!(
                    "repo:{}/{} is:pr is:open updated:<{}",
                    repo.owner, repo.repo, stale_before
                );
                let url = Url::parse_with_params(
                    &absolute_api_url(&config.api_base_url, "/search/issues")?,
                    &[("q", query)],
                )
                .map_err(|error| ClientError::InvalidArgs(error.to_string()))?;
                let response = self
                    .request_response(
                        &config,
                        Method::GET,
                        url.to_string(),
                        None,
                        "application/vnd.github+json",
                    )
                    .await?;
                response
                    .json::<JsonValue>()
                    .await
                    .map_err(|error| ClientError::Transport(error.to_string()))
            }
            "get_pr_diff" => {
                let args: GetPrDiffArgs = parse_args(args)?;
                let config = self.resolve_client_config(&args.config)?;
                let pr = parse_issue_like_url(&args.pr_url, "pull")?;
                let text = self
                    .request_text(
                        &config,
                        Method::GET,
                        &format!(
                            "/repos/{}/{}/pulls/{}",
                            pr.repo.owner, pr.repo.repo, pr.number
                        ),
                        "application/vnd.github.diff",
                    )
                    .await?;
                Ok(JsonValue::String(text))
            }
            "create_issue" => {
                let args: CreateIssueArgs = parse_args(args)?;
                let config = self.resolve_client_config(&args.config)?;
                let repo = parse_repo_ref(&args.repo)?;
                let mut body = JsonMap::new();
                body.insert("title".to_string(), JsonValue::String(args.title));
                if let Some(value) = args.body {
                    body.insert("body".to_string(), JsonValue::String(value));
                }
                if let Some(labels) = args.labels {
                    body.insert("labels".to_string(), json!(labels));
                }
                self.request_json(
                    &config,
                    Method::POST,
                    &format!("/repos/{}/{}/issues", repo.owner, repo.repo),
                    Some(JsonValue::Object(body)),
                    "application/vnd.github+json",
                )
                .await
            }
            "api_call" => {
                let args: ApiCallArgs = parse_args(args)?;
                let config = self.resolve_client_config(&args.config)?;
                let method = Method::from_bytes(args.method.as_bytes())
                    .map_err(|error| ClientError::InvalidArgs(error.to_string()))?;
                self.request_json(
                    &config,
                    method,
                    &args.path,
                    args.body,
                    args.accept
                        .as_deref()
                        .unwrap_or("application/vnd.github+json"),
                )
                .await
            }
            other => Err(ClientError::MethodNotFound(format!(
                "github connector does not implement outbound method `{other}`"
            ))),
        }
    }
}

impl GitHubClient {
    fn resolve_client_config(
        &self,
        args: &GitHubClientConfigArgs,
    ) -> Result<ResolvedGitHubClientConfig, ClientError> {
        let app_id = args.app_id.ok_or_else(|| {
            ClientError::InvalidArgs("github connector requires app_id".to_string())
        })?;
        let installation_id = args.installation_id.ok_or_else(|| {
            ClientError::InvalidArgs("github connector requires installation_id".to_string())
        })?;
        let private_key = if let Some(secret_id) = args
            .private_key_secret
            .as_deref()
            .or(args.secrets.private_key.as_deref())
            .and_then(|value| parse_secret_id(Some(value)))
        {
            PrivateKeySource::Secret(secret_id)
        } else if let Some(pem) = args.private_key_pem.clone() {
            PrivateKeySource::Inline(pem)
        } else {
            return Err(ClientError::InvalidArgs(
                "github connector requires private_key_secret or private_key_pem".to_string(),
            ));
        };
        Ok(ResolvedGitHubClientConfig {
            app_id,
            installation_id,
            api_base_url: args
                .api_base_url
                .clone()
                .unwrap_or_else(|| DEFAULT_API_BASE_URL.to_string()),
            private_key,
        })
    }

    fn ctx(&self) -> Result<ConnectorCtx, ClientError> {
        self.state
            .ctx
            .read()
            .expect("github connector ctx poisoned")
            .clone()
            .ok_or_else(|| ClientError::Other("github connector must be initialized".to_string()))
    }

    async fn request_json(
        &self,
        config: &ResolvedGitHubClientConfig,
        method: Method,
        path: &str,
        body: Option<JsonValue>,
        accept: &str,
    ) -> Result<JsonValue, ClientError> {
        let url = absolute_api_url(&config.api_base_url, path)?;
        let response = self
            .request_response(config, method, url, body, accept)
            .await?;
        response
            .json::<JsonValue>()
            .await
            .map_err(|error| ClientError::Transport(error.to_string()))
    }

    async fn request_text(
        &self,
        config: &ResolvedGitHubClientConfig,
        method: Method,
        path: &str,
        accept: &str,
    ) -> Result<String, ClientError> {
        let url = absolute_api_url(&config.api_base_url, path)?;
        let response = self
            .request_response(config, method, url, None, accept)
            .await?;
        response
            .text()
            .await
            .map_err(|error| ClientError::Transport(error.to_string()))
    }

    async fn request_response(
        &self,
        config: &ResolvedGitHubClientConfig,
        method: Method,
        url: String,
        body: Option<JsonValue>,
        accept: &str,
    ) -> Result<Response, ClientError> {
        let mut retried_401 = false;
        let mut retried_rate_limit = false;
        loop {
            let token = self.installation_token(config).await?;
            let token_text = token.with_exposed(|bytes| String::from_utf8_lossy(bytes).to_string());
            let ctx = self.ctx()?;
            ctx.rate_limiter
                .scoped(
                    &self.provider_id,
                    format!("installation:{}", config.installation_id),
                )
                .acquire()
                .await;

            let mut request = self
                .http
                .request(method.clone(), &url)
                .header("Accept", accept)
                .header("X-GitHub-Api-Version", GITHUB_API_VERSION)
                .header("Authorization", format!("Bearer {token_text}"));
            if let Some(payload) = body.clone() {
                request = request.json(&payload);
            }
            let response = request
                .send()
                .await
                .map_err(|error| ClientError::Transport(error.to_string()))?;
            self.record_rate_limit_observation(&response).await;

            if response.status() == StatusCode::UNAUTHORIZED && !retried_401 {
                self.tokens.invalidate(config.installation_id);
                retried_401 = true;
                continue;
            }
            if is_rate_limited(&response) && !retried_rate_limit {
                tokio::time::sleep(rate_limit_backoff(&response)).await;
                retried_rate_limit = true;
                continue;
            }
            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                let message = if body.trim().is_empty() {
                    format!("github API request failed with status {status}")
                } else {
                    format!("github API request failed with status {status}: {body}")
                };
                return Err(if is_rate_limited_status(status) {
                    ClientError::RateLimited(message)
                } else {
                    ClientError::Transport(message)
                });
            }
            return Ok(response);
        }
    }

    async fn installation_token(
        &self,
        config: &ResolvedGitHubClientConfig,
    ) -> Result<SecretBytes, ClientError> {
        let now = OffsetDateTime::now_utc();
        if let Some(token) = self.tokens.get(config.installation_id, now) {
            return Ok(token);
        }
        let jwt = self.mint_app_jwt(config).await?;
        let url = absolute_api_url(
            &config.api_base_url,
            &format!(
                "/app/installations/{}/access_tokens",
                config.installation_id
            ),
        )?;
        let ctx = self.ctx()?;
        ctx.rate_limiter
            .scoped(
                &self.provider_id,
                format!("installation:{}", config.installation_id),
            )
            .acquire()
            .await;
        let response = self
            .http
            .post(url)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", GITHUB_API_VERSION)
            .header("Authorization", format!("Bearer {jwt}"))
            .send()
            .await
            .map_err(|error| ClientError::Transport(error.to_string()))?;
        self.record_rate_limit_observation(&response).await;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(ClientError::Transport(format!(
                "failed to create GitHub installation token ({status}): {body}"
            )));
        }
        let payload: InstallationTokenResponse = response
            .json()
            .await
            .map_err(|error| ClientError::Transport(error.to_string()))?;
        let refresh_at = installation_token_refresh_at(payload.expires_at.as_deref(), now);
        let token = SecretBytes::from(payload.token);
        let result = token.reborrow();
        self.tokens.store(config.installation_id, token, refresh_at);
        Ok(result)
    }

    async fn mint_app_jwt(
        &self,
        config: &ResolvedGitHubClientConfig,
    ) -> Result<String, ClientError> {
        let pem = self.private_key_pem(config).await?;
        let now = OffsetDateTime::now_utc();
        let claims = GitHubJwtClaims {
            iat: (now - Duration::seconds(60)).unix_timestamp(),
            exp: (now + Duration::minutes(9)).unix_timestamp(),
            iss: config.app_id.to_string(),
        };
        let header = Header::new(Algorithm::RS256);
        let key = EncodingKey::from_rsa_pem(pem.as_bytes())
            .map_err(|error| ClientError::Other(error.to_string()))?;
        jsonwebtoken::encode(&header, &claims, &key)
            .map_err(|error| ClientError::Other(error.to_string()))
    }

    async fn private_key_pem(
        &self,
        config: &ResolvedGitHubClientConfig,
    ) -> Result<String, ClientError> {
        match &config.private_key {
            PrivateKeySource::Inline(pem) => Ok(pem.clone()),
            PrivateKeySource::Secret(id) => {
                let ctx = self.ctx()?;
                let secret = ctx
                    .secrets
                    .get(id)
                    .await
                    .map_err(|error| ClientError::Other(error.to_string()))?;
                secret.with_exposed(|bytes| {
                    std::str::from_utf8(bytes)
                        .map(|value| value.to_string())
                        .map_err(|error| ClientError::Other(error.to_string()))
                })
            }
        }
    }

    async fn record_rate_limit_observation(&self, response: &Response) {
        let remaining = response
            .headers()
            .get("x-ratelimit-remaining")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok());
        let reset = response
            .headers()
            .get("x-ratelimit-reset")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<i64>().ok());
        let Some(remaining) = remaining else {
            return;
        };
        if remaining > 10 {
            return;
        }
        let Ok(ctx) = self.ctx() else {
            return;
        };
        let Ok(topic) = Topic::new(GITHUB_RATE_LIMIT_TOPIC) else {
            return;
        };
        let _ = ctx
            .event_log
            .append(
                &topic,
                LogEvent::new(
                    "github.rate_limit",
                    json!({
                        "remaining": remaining,
                        "reset": reset,
                        "status": response.status().as_u16(),
                    }),
                ),
            )
            .await;
    }
}

impl ActivatedGitHubBinding {
    fn from_binding(binding: &TriggerBinding) -> Result<Self, ConnectorError> {
        let config: GitHubBindingConfig =
            serde_json::from_value(binding.config.clone()).map_err(|error| {
                ConnectorError::Activation(format!(
                    "github binding `{}` has invalid config: {error}",
                    binding.binding_id
                ))
            })?;
        let signing_secret =
            parse_secret_id(config.secrets.signing_secret.as_deref()).ok_or_else(|| {
                ConnectorError::Activation(format!(
                    "github binding `{}` requires secrets.signing_secret",
                    binding.binding_id
                ))
            })?;
        Ok(Self {
            binding_id: binding.binding_id.clone(),
            path: config.match_config.path,
            signing_secret,
            dedupe_enabled: binding.dedupe_key.is_some(),
            dedupe_ttl: std::time::Duration::from_secs(
                u64::from(crate::triggers::DEFAULT_INBOX_RETENTION_DAYS) * 24 * 60 * 60,
            ),
        })
    }
}

impl GitHubInstallationTokenStore {
    fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            state: Mutex::new(TokenCacheState::default()),
        }
    }

    fn get(&self, installation_id: u64, now: OffsetDateTime) -> Option<SecretBytes> {
        let mut state = self.state.lock().expect("github token cache poisoned");
        let refresh_at = state
            .entries
            .get(&installation_id)
            .map(|entry| entry.refresh_at)?;
        if refresh_at <= now {
            state.entries.remove(&installation_id);
            state.order.retain(|id| *id != installation_id);
            return None;
        }
        touch_lru(&mut state.order, installation_id);
        state
            .entries
            .get(&installation_id)
            .map(|entry| entry.token.reborrow())
    }

    fn store(&self, installation_id: u64, token: SecretBytes, refresh_at: OffsetDateTime) {
        let mut state = self.state.lock().expect("github token cache poisoned");
        state.entries.insert(
            installation_id,
            InstallationTokenEntry { token, refresh_at },
        );
        touch_lru(&mut state.order, installation_id);
        while state.entries.len() > self.capacity {
            if let Some(evicted) = state.order.pop_front() {
                state.entries.remove(&evicted);
            }
        }
    }

    fn invalidate(&self, installation_id: u64) {
        let mut state = self.state.lock().expect("github token cache poisoned");
        state.entries.remove(&installation_id);
        state.order.retain(|id| *id != installation_id);
    }
}

fn touch_lru(order: &mut VecDeque<u64>, installation_id: u64) {
    order.retain(|id| *id != installation_id);
    order.push_back(installation_id);
}

fn parse_args<T: DeserializeOwned>(args: JsonValue) -> Result<T, ClientError> {
    serde_json::from_value(args).map_err(|error| ClientError::InvalidArgs(error.to_string()))
}

fn parse_typed_event(kind: &str, payload: &JsonValue) -> Result<ParsedGitHubEvent, ConnectorError> {
    match kind {
        "issues" => serde_json::from_value(payload.clone())
            .map(ParsedGitHubEvent::Issues)
            .map_err(|error| ConnectorError::Json(error.to_string())),
        "pull_request" => serde_json::from_value(payload.clone())
            .map(ParsedGitHubEvent::PullRequest)
            .map_err(|error| ConnectorError::Json(error.to_string())),
        "issue_comment" => serde_json::from_value(payload.clone())
            .map(ParsedGitHubEvent::IssueComment)
            .map_err(|error| ConnectorError::Json(error.to_string())),
        "pull_request_review" => serde_json::from_value(payload.clone())
            .map(ParsedGitHubEvent::PullRequestReview)
            .map_err(|error| ConnectorError::Json(error.to_string())),
        "push" => serde_json::from_value(payload.clone())
            .map(ParsedGitHubEvent::Push)
            .map_err(|error| ConnectorError::Json(error.to_string())),
        "workflow_run" => serde_json::from_value(payload.clone())
            .map(ParsedGitHubEvent::WorkflowRun)
            .map_err(|error| ConnectorError::Json(error.to_string())),
        "deployment_status" => serde_json::from_value(payload.clone())
            .map(ParsedGitHubEvent::DeploymentStatus)
            .map_err(|error| ConnectorError::Json(error.to_string())),
        "check_run" => serde_json::from_value(payload.clone())
            .map(ParsedGitHubEvent::CheckRun)
            .map_err(|error| ConnectorError::Json(error.to_string())),
        other => Ok(ParsedGitHubEvent::Other {
            kind: other.to_string(),
            raw: payload.clone(),
        }),
    }
}

fn effective_headers(headers: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut effective = headers.clone();
    canonicalize_header(headers, &mut effective, "content-type", "Content-Type");
    canonicalize_header(headers, &mut effective, "x-github-event", "X-GitHub-Event");
    canonicalize_header(
        headers,
        &mut effective,
        "x-github-delivery",
        "X-GitHub-Delivery",
    );
    canonicalize_header(
        headers,
        &mut effective,
        "x-github-hook-id",
        "X-GitHub-Hook-Id",
    );
    canonicalize_header(
        headers,
        &mut effective,
        "x-hub-signature-256",
        "X-Hub-Signature-256",
    );
    effective
}

fn canonicalize_header(
    source: &BTreeMap<String, String>,
    target: &mut BTreeMap<String, String>,
    lookup_name: &str,
    canonical_name: &str,
) {
    if let Some(value) = header_value(source, lookup_name) {
        target
            .entry(canonical_name.to_string())
            .or_insert_with(|| value.to_string());
    }
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
) -> Result<String, ConnectorError> {
    let secret = futures::executor::block_on(ctx.secrets.get(secret_id))?;
    secret.with_exposed(|bytes| {
        std::str::from_utf8(bytes)
            .map(|value| value.to_string())
            .map_err(|error| {
                ConnectorError::Secret(format!(
                    "github secret `{secret_id}` is not valid UTF-8: {error}"
                ))
            })
    })
}

fn fallback_body_digest(body: &[u8]) -> String {
    let digest = Sha256::digest(body);
    format!("sha256:{}", hex::encode(digest))
}

fn parse_repo_ref(input: &str) -> Result<RepoRef, ClientError> {
    let trimmed = input.trim().trim_matches('/');
    if let Some(rest) = trimmed.strip_prefix("https://github.com/") {
        let parts: Vec<&str> = rest.trim_matches('/').split('/').collect();
        if parts.len() >= 2 {
            return Ok(RepoRef {
                owner: parts[0].to_string(),
                repo: parts[1].to_string(),
            });
        }
    }
    if let Some(rest) = trimmed.strip_prefix("https://api.github.com/repos/") {
        let parts: Vec<&str> = rest.trim_matches('/').split('/').collect();
        if parts.len() >= 2 {
            return Ok(RepoRef {
                owner: parts[0].to_string(),
                repo: parts[1].to_string(),
            });
        }
    }
    if let Some((owner, repo)) = trimmed.split_once('/') {
        if !owner.is_empty() && !repo.is_empty() {
            return Ok(RepoRef {
                owner: owner.to_string(),
                repo: repo.to_string(),
            });
        }
    }
    Err(ClientError::InvalidArgs(format!(
        "invalid GitHub repository reference `{input}`"
    )))
}

fn parse_issue_like_url(input: &str, expected_kind: &str) -> Result<IssueRef, ClientError> {
    let trimmed = input.trim().trim_matches('/');
    if let Some(rest) = trimmed.strip_prefix("https://github.com/") {
        let parts: Vec<&str> = rest.trim_matches('/').split('/').collect();
        if parts.len() >= 4 {
            let kind = parts[2];
            if kind == expected_kind || (expected_kind == "issue" && kind == "issues") {
                return Ok(IssueRef {
                    repo: RepoRef {
                        owner: parts[0].to_string(),
                        repo: parts[1].to_string(),
                    },
                    number: parts[3].parse().map_err(|error| {
                        ClientError::InvalidArgs(format!("invalid GitHub URL `{input}`: {error}"))
                    })?,
                });
            }
        }
    }
    if let Some(rest) = trimmed.strip_prefix("https://api.github.com/repos/") {
        let parts: Vec<&str> = rest.trim_matches('/').split('/').collect();
        if parts.len() >= 4 {
            let kind = parts[2];
            if kind == expected_kind
                || (expected_kind == "issue" && kind == "issues")
                || (expected_kind == "pull" && kind == "pulls")
            {
                return Ok(IssueRef {
                    repo: RepoRef {
                        owner: parts[0].to_string(),
                        repo: parts[1].to_string(),
                    },
                    number: parts[3].parse().map_err(|error| {
                        ClientError::InvalidArgs(format!("invalid GitHub URL `{input}`: {error}"))
                    })?,
                });
            }
        }
    }
    Err(ClientError::InvalidArgs(format!(
        "invalid GitHub {expected_kind} URL `{input}`"
    )))
}

fn absolute_api_url(base_url: &str, path: &str) -> Result<String, ClientError> {
    let base = Url::parse(base_url).map_err(|error| ClientError::InvalidArgs(error.to_string()))?;
    base.join(path.trim_start_matches('/'))
        .map(|value| value.to_string())
        .map_err(|error| ClientError::InvalidArgs(error.to_string()))
}

fn installation_token_refresh_at(expires_at: Option<&str>, now: OffsetDateTime) -> OffsetDateTime {
    let eager_refresh = now + Duration::minutes(55);
    let Some(expires_at) = expires_at else {
        return eager_refresh;
    };
    let parsed =
        OffsetDateTime::parse(expires_at, &time::format_description::well_known::Rfc3339).ok();
    parsed
        .map(|value| value - Duration::minutes(5))
        .map(|value| {
            if value < eager_refresh {
                value
            } else {
                eager_refresh
            }
        })
        .unwrap_or(eager_refresh)
}

fn is_rate_limited(response: &Response) -> bool {
    if is_rate_limited_status(response.status()) {
        return true;
    }
    response
        .headers()
        .get("x-ratelimit-remaining")
        .and_then(|value| value.to_str().ok())
        .map(|value| value == "0")
        .unwrap_or(false)
}

fn is_rate_limited_status(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS || status == StatusCode::FORBIDDEN
}

fn rate_limit_backoff(response: &Response) -> std::time::Duration {
    if let Some(delay) = response
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
    {
        return std::time::Duration::from_secs(delay);
    }
    if let Some(reset_at) = response
        .headers()
        .get("x-ratelimit-reset")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<i64>().ok())
        .and_then(|value| OffsetDateTime::from_unix_timestamp(value).ok())
    {
        let now = OffsetDateTime::now_utc();
        if reset_at > now {
            let delta = reset_at - now;
            return std::time::Duration::from_secs(delta.whole_seconds().max(0) as u64);
        }
    }
    std::time::Duration::from_millis(100)
}
