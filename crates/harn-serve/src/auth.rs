use std::collections::{BTreeMap, BTreeSet};

use harn_vm::connectors::ConnectorError;
use harn_vm::event_log::MemoryEventLog;
use harn_vm::{ProviderId, TenantId};
use subtle::ConstantTimeEq;
use time::{Duration, OffsetDateTime};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthenticatedPrincipal {
    pub subject: String,
    pub scheme: String,
    /// Scopes the credential carries. Compared against the per-route
    /// `required_scopes` passed to `AuthPolicy::authorize_with_scopes`.
    pub granted_scopes: BTreeSet<String>,
    /// Tenant the credential is bound to. Populated by API-key /
    /// HMAC / OAuth verification when the configured method ties the
    /// credential to a specific `TenantId`; `None` means the
    /// credential is tenant-agnostic (e.g. an open-permissions key, or
    /// an admin/system bearer). Threaded through `DispatchCtx` into
    /// the `.harn` callee as `harness.tenant.id()`.
    pub tenant_id: Option<TenantId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OAuthClaims {
    pub subject: String,
    pub issuer: String,
    pub audience: Option<String>,
    pub scopes: BTreeSet<String>,
    /// Tenant claim the transport extracted from the verified JWT
    /// (typically `tenant_id`, `tid`, or a custom claim configured per
    /// deployment). When the auth policy's `OAuth21AuthConfig` does not
    /// pick a specific claim name, this is the trusted fallback the
    /// transport itself believes refers to the tenant.
    pub tenant_id: Option<TenantId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct AuthRequest {
    pub method: String,
    pub path: String,
    pub body: Vec<u8>,
    pub headers: BTreeMap<String, String>,
    pub validated_oauth: Option<OAuthClaims>,
    /// Tenant the transport resolved ahead of authentication (e.g. a
    /// cloud gateway that looks up the API key in its own store and
    /// hands `harn-serve` the result). When set, it wins over whatever
    /// the configured auth method would resolve, so the same
    /// `AuthPolicy` works for both first-party API key configs and
    /// transports that own tenant resolution. `None` means defer to
    /// the auth method.
    pub tenant_id: Option<TenantId>,
}

impl AuthRequest {
    pub fn bearer_token(&self) -> Option<&str> {
        header_value(&self.headers, "authorization")
            .and_then(|value| value.split_once(' '))
            .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
            .map(|(_, token)| token.trim())
            .filter(|value| !value.is_empty())
    }

    pub fn api_key(&self) -> Option<&str> {
        header_value(&self.headers, "x-api-key")
            .filter(|value| !value.is_empty())
            .or_else(|| self.bearer_token())
    }
}

fn header_value<'a>(headers: &'a BTreeMap<String, String>, name: &str) -> Option<&'a str> {
    headers
        .get(name)
        .map(String::as_str)
        .or_else(|| {
            headers
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
                .map(|(_, value)| value.as_str())
        })
        .map(str::trim)
}

/// A single API key paired with the scopes the key grants. Two keys
/// pointing at the same secret with different scope sets are treated as
/// separate entries; the first match wins.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiKeyEntry {
    pub key: String,
    pub scopes: BTreeSet<String>,
    /// Tenant the key is bound to. When set, every authenticated
    /// dispatch under this key carries the tenant into `DispatchCtx`
    /// and the `harness.tenant` ambient. `None` means a tenant-agnostic
    /// key — useful for single-tenant deployments and integration tests.
    pub tenant_id: Option<TenantId>,
}

impl ApiKeyEntry {
    pub fn new(key: impl Into<String>, scopes: impl IntoIterator<Item = String>) -> Self {
        Self {
            key: key.into(),
            scopes: scopes.into_iter().collect(),
            tenant_id: None,
        }
    }

    /// Bind this key to a tenant. Chainable so callers can construct
    /// the entry in one expression: `ApiKeyEntry::new("k", scopes).with_tenant("acme")`.
    pub fn with_tenant(mut self, tenant: impl Into<String>) -> Self {
        self.tenant_id = Some(TenantId::new(tenant));
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ApiKeyAuthConfig {
    pub keys: Vec<ApiKeyEntry>,
}

impl ApiKeyAuthConfig {
    /// Build a single-key config with no scopes (open-permissions).
    /// Convenience for tests and configurations that don't care about
    /// scope checks yet.
    pub fn single(key: impl Into<String>) -> Self {
        Self {
            keys: vec![ApiKeyEntry::new(key.into(), [])],
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HmacAuthConfig {
    pub shared_secret: String,
    pub provider: String,
    pub timestamp_window: Duration,
    /// Scopes any caller authenticated via this shared secret carries.
    pub granted_scopes: BTreeSet<String>,
    /// Tenant any caller authenticated via this shared secret is bound
    /// to. `None` leaves the dispatch tenant-agnostic — appropriate for
    /// single-tenant deployments or callers that resolve tenancy out of
    /// band (e.g. from the canonical request body).
    pub tenant_id: Option<TenantId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OAuth21AuthConfig {
    pub issuer: String,
    pub audience: Option<String>,
    /// Scopes the OAuth method itself requires beyond any per-route check.
    /// Independent from per-route `required_scopes` so an OAuth-only deployment
    /// can pin a baseline (e.g. `harn:invoke`) without repeating it on every
    /// route mount.
    pub required_scopes: BTreeSet<String>,
    /// Tenant fallback when the verified JWT did not carry a
    /// `tenant_id` claim (or the transport did not extract one).
    /// Useful for single-tenant OAuth deployments where every token is
    /// implicitly bound to the same tenant. The transport-supplied
    /// `OAuthClaims.tenant_id` still wins when present.
    pub tenant_id: Option<TenantId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthMethodConfig {
    ApiKey(ApiKeyAuthConfig),
    Hmac(HmacAuthConfig),
    OAuth21(OAuth21AuthConfig),
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct AuthPolicy {
    pub methods: Vec<AuthMethodConfig>,
    /// Per-tenant MCP allowlist (#2504, A.7). `None` means allow-all —
    /// every hosted MCP server and every tool on it is callable. When
    /// `Some`, only the listed (server, tool) pairs are reachable.
    pub mcp_allowlist: Option<McpAllowlist>,
}

/// Tenant-scoped allowlist for the `harness.mcp.*` host primitive.
/// Each entry maps an MCP server name to the tools that tenant is
/// allowed to invoke on it. An entry whose tool set is `All` allows
/// every tool the server advertises; an explicit `Only(...)` set
/// restricts callers to the named tools (case-sensitive match against
/// the server-advertised tool name).
///
/// The list is checked at *dispatch* time — before any network traffic
/// — and surfaces a typed [`AuthorizationDecision::McpNotAllowlisted`]
/// variant so callers can render an actionable error body without
/// guessing which check fired.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct McpAllowlist {
    pub servers: BTreeMap<String, McpAllowlistTools>,
}

/// Tool-level allowlist filter for a single MCP server. `All` allows
/// every tool the server reports; `Only` restricts to the named set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McpAllowlistTools {
    All,
    Only(BTreeSet<String>),
}

impl McpAllowlist {
    /// Empty allowlist — denies every server. Useful as a tenant-level
    /// default-deny that operators then layer permits on top of.
    pub fn deny_all() -> Self {
        Self::default()
    }

    /// Convenience: allow every tool on every named server. Equivalent
    /// to inserting `(name, McpAllowlistTools::All)` for each name.
    pub fn allow_servers(names: impl IntoIterator<Item = String>) -> Self {
        let servers = names
            .into_iter()
            .map(|name| (name, McpAllowlistTools::All))
            .collect();
        Self { servers }
    }

    /// Insert / replace the entry for `server`. `tools` is the explicit
    /// allowlist for that server; pass [`McpAllowlistTools::All`] to
    /// allow every tool. Returns `&mut self` to support builder-style
    /// composition.
    pub fn allow(&mut self, server: impl Into<String>, tools: McpAllowlistTools) -> &mut Self {
        self.servers.insert(server.into(), tools);
        self
    }

    /// Resolve a (server, tool) pair against the allowlist.
    ///
    /// - `server` not present → denied (server not allowlisted)
    /// - server present with `All` tools → allowed regardless of `tool`
    /// - server present with `Only(set)`:
    ///   - `tool == Some(name)` and `name ∈ set` → allowed
    ///   - `tool == Some(name)` and `name ∉ set` → denied (tool not allowlisted)
    ///   - `tool == None` (spawn check, no specific tool yet) → allowed
    pub fn check(&self, server: &str, tool: Option<&str>) -> AllowlistOutcome {
        let Some(entry) = self.servers.get(server) else {
            return AllowlistOutcome::ServerDenied;
        };
        match (entry, tool) {
            (McpAllowlistTools::All, _) => AllowlistOutcome::Allow,
            (McpAllowlistTools::Only(_), None) => AllowlistOutcome::Allow,
            (McpAllowlistTools::Only(set), Some(name)) if set.contains(name) => {
                AllowlistOutcome::Allow
            }
            (McpAllowlistTools::Only(_), Some(_)) => AllowlistOutcome::ToolDenied,
        }
    }
}

/// Result of evaluating an [`McpAllowlist`] against a (server, tool)
/// pair. Mirrors [`AuthorizationDecision::McpNotAllowlisted`] without
/// the principal/scope context.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AllowlistOutcome {
    Allow,
    ServerDenied,
    ToolDenied,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthorizationDecision {
    Authorized(AuthenticatedPrincipal),
    /// Authentication itself failed — no method accepted the credentials.
    Rejected(String),
    /// Authentication succeeded but the principal lacks one or more scopes
    /// required by the route. `required` is the full route requirement;
    /// `granted` is what the credential actually carries. Both sets are
    /// returned so the caller can render an actionable error body.
    MissingScope {
        required: BTreeSet<String>,
        granted: BTreeSet<String>,
    },
    /// The credential authenticated and met any scope requirements, but
    /// the per-tenant MCP allowlist (#2504) refused the (server, tool)
    /// pair. `reason` is a short, user-facing explanation; `server` and
    /// `tool` echo what was attempted so the caller can render a
    /// targeted error.
    McpNotAllowlisted {
        server: String,
        tool: Option<String>,
        reason: String,
    },
}

impl AuthPolicy {
    pub fn allow_all() -> Self {
        Self::default()
    }

    pub fn method_by_acp_id(&self, method_id: &str) -> Option<&AuthMethodConfig> {
        let mut counts = BTreeMap::new();
        self.methods.iter().find(|method| {
            let id = next_acp_auth_method_id(method, &mut counts);
            id == method_id
        })
    }

    pub fn acp_auth_methods(&self) -> Vec<serde_json::Value> {
        let mut counts = BTreeMap::new();
        self.methods
            .iter()
            .map(|method| {
                let id = next_acp_auth_method_id(method, &mut counts);
                acp_auth_method(method, id)
            })
            .collect()
    }

    /// Authenticate without checking any per-route scopes. Equivalent to
    /// `authorize_with_scopes(request, &BTreeSet::new())`. Retained as the
    /// short-form for callers that don't have route metadata (e.g. the
    /// REST API adapter's static handlers).
    pub async fn authorize(&self, request: &AuthRequest) -> AuthorizationDecision {
        self.authorize_with_scopes(request, &BTreeSet::new()).await
    }

    /// Authenticate the request and verify the resulting principal's
    /// `granted_scopes` ⊇ `required`. When the policy has no configured
    /// methods, the request is accepted with no scopes granted; the scope
    /// check then succeeds iff `required` is empty.
    pub async fn authorize_with_scopes(
        &self,
        request: &AuthRequest,
        required: &BTreeSet<String>,
    ) -> AuthorizationDecision {
        let mut principal = if self.methods.is_empty() {
            AuthenticatedPrincipal {
                subject: "anonymous".to_string(),
                scheme: "none".to_string(),
                granted_scopes: BTreeSet::new(),
                tenant_id: None,
            }
        } else {
            let mut failures = Vec::new();
            let mut chosen: Option<AuthenticatedPrincipal> = None;
            for method in &self.methods {
                match authorize_method(method, request).await {
                    Ok(principal) => {
                        chosen = Some(principal);
                        break;
                    }
                    Err(message) => failures.push(message),
                }
            }
            match chosen {
                Some(principal) => principal,
                None => return AuthorizationDecision::Rejected(failures.join("; ")),
            }
        };

        // Transports that resolve tenancy themselves (e.g. an upstream
        // cloud gateway that owns the API-key → tenant lookup) hand the
        // result in via `AuthRequest.tenant_id`. That override wins so
        // operators can swap auth methods without rewiring how the
        // tenant is sourced.
        if let Some(override_tenant) = request.tenant_id.clone() {
            principal.tenant_id = Some(override_tenant);
        }

        if !required.is_subset(&principal.granted_scopes) {
            return AuthorizationDecision::MissingScope {
                required: required.clone(),
                granted: principal.granted_scopes,
            };
        }
        AuthorizationDecision::Authorized(principal)
    }

    /// Evaluate the per-tenant MCP allowlist for a (`server`, `tool`)
    /// pair. Returns [`AuthorizationDecision::Authorized`] with an
    /// anonymous principal when no allowlist is configured (allow-all),
    /// and otherwise dispatches against [`McpAllowlist::check`].
    ///
    /// `tool == None` is the spawn-time check — the caller has not yet
    /// chosen a specific tool, only a server. For that path we accept
    /// any server present in the allowlist regardless of its tool
    /// filter; the per-tool check still fires on each subsequent
    /// `call`.
    pub fn authorize_mcp(&self, server: &str, tool: Option<&str>) -> AuthorizationDecision {
        let Some(allowlist) = &self.mcp_allowlist else {
            return AuthorizationDecision::Authorized(AuthenticatedPrincipal {
                subject: "anonymous".to_string(),
                scheme: "none".to_string(),
                granted_scopes: BTreeSet::new(),
            });
        };
        match allowlist.check(server, tool) {
            AllowlistOutcome::Allow => AuthorizationDecision::Authorized(AuthenticatedPrincipal {
                subject: "anonymous".to_string(),
                scheme: "none".to_string(),
                granted_scopes: BTreeSet::new(),
            }),
            AllowlistOutcome::ServerDenied => AuthorizationDecision::McpNotAllowlisted {
                server: server.to_string(),
                tool: tool.map(str::to_string),
                reason: format!("MCP server '{server}' is not on the tenant allowlist"),
            },
            AllowlistOutcome::ToolDenied => AuthorizationDecision::McpNotAllowlisted {
                server: server.to_string(),
                tool: tool.map(str::to_string),
                reason: format!(
                    "MCP tool '{}' on server '{server}' is not on the tenant allowlist",
                    tool.unwrap_or("<unknown>")
                ),
            },
        }
    }
}

fn acp_auth_method_kind(method: &AuthMethodConfig) -> &'static str {
    match method {
        AuthMethodConfig::ApiKey(_) => "apiKey",
        AuthMethodConfig::Hmac(_) => "hmac",
        AuthMethodConfig::OAuth21(_) => "oauth2",
    }
}

fn next_acp_auth_method_id(
    method: &AuthMethodConfig,
    counts: &mut BTreeMap<&'static str, usize>,
) -> String {
    let base = acp_auth_method_kind(method);
    let count = counts.entry(base).or_insert(0);
    let id = if *count == 0 {
        base.to_string()
    } else {
        format!("{base}-{}", *count + 1)
    };
    *count += 1;
    id
}

fn acp_auth_method(method: &AuthMethodConfig, id: String) -> serde_json::Value {
    match method {
        AuthMethodConfig::ApiKey(_) => serde_json::json!({
            "id": id,
            "name": "Harn API key",
            "description": "Authenticate with an API key supplied as `Authorization: Bearer <key>` or `X-API-Key`.",
            "_meta": {
                "harn": {
                    "scheme": "api_key",
                    "challenge": {
                        "type": "api_key",
                        "headers": ["Authorization", "X-API-Key"],
                        "authorizationScheme": "Bearer"
                    }
                }
            }
        }),
        AuthMethodConfig::Hmac(config) => serde_json::json!({
            "id": id,
            "name": "Harn HMAC signature",
            "description": "Authenticate with an HMAC-SHA256 canonical request signature.",
            "_meta": {
                "harn": {
                    "scheme": "hmac",
                    "challenge": {
                        "type": "hmac",
                        "algorithm": "HMAC-SHA256",
                        "provider": config.provider,
                        "headers": ["Authorization"],
                        "canonicalRequest": {
                            "method": "ACP",
                            "path": "authenticate"
                        }
                    }
                }
            }
        }),
        AuthMethodConfig::OAuth21(config) => serde_json::json!({
            "id": id,
            "name": "OAuth 2.1 bearer token",
            "description": "Authenticate with a bearer token validated by the transport.",
            "_meta": {
                "harn": {
                    "scheme": "oauth2",
                    "challenge": {
                        "type": "oauth2",
                        "issuer": config.issuer,
                        "audience": config.audience,
                        "scopes": config.required_scopes.iter().cloned().collect::<Vec<_>>(),
                        "headers": ["Authorization"],
                        "authorizationScheme": "Bearer"
                    }
                }
            }
        }),
    }
}

async fn authorize_method(
    method: &AuthMethodConfig,
    request: &AuthRequest,
) -> Result<AuthenticatedPrincipal, String> {
    match method {
        AuthMethodConfig::ApiKey(config) => {
            let Some(api_key) = request.api_key() else {
                return Err("missing API key".to_string());
            };
            let entry = match_api_key(&config.keys, api_key);
            match entry {
                Some(entry) => Ok(AuthenticatedPrincipal {
                    subject: "api-key".to_string(),
                    scheme: "api_key".to_string(),
                    granted_scopes: entry.scopes.clone(),
                    tenant_id: entry.tenant_id.clone(),
                }),
                None => Err("invalid API key".to_string()),
            }
        }
        AuthMethodConfig::Hmac(config) => {
            let log = MemoryEventLog::new(8);
            harn_vm::connectors::hmac::verify_hmac_authorization(
                &log,
                &ProviderId::new(config.provider.clone()),
                &request.method,
                &request.path,
                &request.body,
                &request.headers,
                &config.shared_secret,
                config.timestamp_window,
                OffsetDateTime::now_utc(),
            )
            .await
            .map_err(connector_error_message)?;
            Ok(AuthenticatedPrincipal {
                subject: "hmac".to_string(),
                scheme: "hmac".to_string(),
                granted_scopes: config.granted_scopes.clone(),
                tenant_id: config.tenant_id.clone(),
            })
        }
        AuthMethodConfig::OAuth21(config) => {
            let Some(claims) = &request.validated_oauth else {
                return Err("oauth token was not validated by the transport".to_string());
            };
            if claims.issuer != config.issuer {
                return Err(format!(
                    "oauth issuer mismatch: expected '{}', got '{}'",
                    config.issuer, claims.issuer
                ));
            }
            if let Some(expected) = config.audience.as_ref() {
                match claims.audience.as_ref() {
                    Some(actual) if actual == expected => {}
                    _ => return Err("oauth audience mismatch".to_string()),
                }
            }
            if !config.required_scopes.is_subset(&claims.scopes) {
                return Err("oauth scope requirement not satisfied".to_string());
            }
            Ok(AuthenticatedPrincipal {
                subject: claims.subject.clone(),
                scheme: "oauth21".to_string(),
                granted_scopes: claims.scopes.clone(),
                // Token-borne claim wins, with the policy-level
                // fallback (`OAuth21AuthConfig.tenant_id`) covering
                // single-tenant deployments where every token implicitly
                // belongs to the same tenant.
                tenant_id: claims
                    .tenant_id
                    .clone()
                    .or_else(|| config.tenant_id.clone()),
            })
        }
    }
}

fn match_api_key<'a>(entries: &'a [ApiKeyEntry], candidate: &str) -> Option<&'a ApiKeyEntry> {
    entries
        .iter()
        .find(|entry| entry.key.as_bytes().ct_eq(candidate.as_bytes()).into())
}

fn connector_error_message(error: ConnectorError) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    use base64::Engine;
    use hmac::{Hmac, KeyInit, Mac};
    use sha2::{Digest, Sha256};

    fn scopes(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[tokio::test]
    async fn api_key_policy_accepts_matching_bearer_token() {
        let policy = AuthPolicy {
            methods: vec![AuthMethodConfig::ApiKey(ApiKeyAuthConfig::single("secret"))],
            mcp_allowlist: None,
        };
        let request = AuthRequest {
            headers: BTreeMap::from([("authorization".to_string(), "Bearer secret".to_string())]),
            ..AuthRequest::default()
        };
        let decision = policy.authorize(&request).await;
        assert!(matches!(decision, AuthorizationDecision::Authorized(_)));
    }

    #[tokio::test]
    async fn api_key_policy_accepts_case_insensitive_header_names() {
        let policy = AuthPolicy {
            methods: vec![AuthMethodConfig::ApiKey(ApiKeyAuthConfig::single("secret"))],
            mcp_allowlist: None,
        };
        let request = AuthRequest {
            headers: BTreeMap::from([("X-API-Key".to_string(), " secret ".to_string())]),
            ..AuthRequest::default()
        };
        let decision = policy.authorize(&request).await;
        assert!(matches!(decision, AuthorizationDecision::Authorized(_)));
    }

    #[test]
    fn acp_auth_methods_use_stable_kind_ids() {
        let policy = AuthPolicy {
            methods: vec![
                AuthMethodConfig::ApiKey(ApiKeyAuthConfig::single("secret")),
                AuthMethodConfig::Hmac(HmacAuthConfig {
                    shared_secret: "shared-secret".to_string(),
                    provider: "harn-serve".to_string(),
                    timestamp_window: Duration::seconds(60),
                    granted_scopes: BTreeSet::new(),
                    tenant_id: None,
                }),
                AuthMethodConfig::ApiKey(ApiKeyAuthConfig::single("second")),
            ],
            mcp_allowlist: None,
        };

        let methods = policy.acp_auth_methods();
        assert_eq!(methods[0]["id"], "apiKey");
        assert_eq!(methods[1]["id"], "hmac");
        assert_eq!(methods[2]["id"], "apiKey-2");
        assert!(matches!(
            policy.method_by_acp_id("hmac"),
            Some(AuthMethodConfig::Hmac(_))
        ));
    }

    #[tokio::test]
    async fn hmac_policy_accepts_valid_canonical_request_signature() {
        let timestamp = OffsetDateTime::now_utc().unix_timestamp().to_string();
        let body = br#"{"ok":true}"#;
        let hash = Sha256::digest(body);
        let body_hash = hex::encode(hash);
        let signed = format!("POST\n/mcp\n{timestamp}\n{body_hash}");
        let mut mac = Hmac::<Sha256>::new_from_slice(b"shared-secret").expect("mac key");
        mac.update(signed.as_bytes());
        let signature =
            base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());
        let authorization = format!("HMAC-SHA256 timestamp={timestamp},signature={signature}");

        let policy = AuthPolicy {
            methods: vec![AuthMethodConfig::Hmac(HmacAuthConfig {
                shared_secret: "shared-secret".to_string(),
                provider: "harn-serve".to_string(),
                timestamp_window: Duration::seconds(60),
                granted_scopes: BTreeSet::new(),
                tenant_id: None,
            })],
            mcp_allowlist: None,
        };
        let request = AuthRequest {
            method: "POST".to_string(),
            path: "/mcp".to_string(),
            body: body.to_vec(),
            headers: BTreeMap::from([("authorization".to_string(), authorization)]),
            validated_oauth: None,
            tenant_id: None,
        };

        let decision = policy.authorize(&request).await;
        assert!(matches!(decision, AuthorizationDecision::Authorized(_)));
    }

    #[tokio::test]
    async fn oauth_policy_requires_transport_validated_claims() {
        let policy = AuthPolicy {
            methods: vec![AuthMethodConfig::OAuth21(OAuth21AuthConfig {
                issuer: "https://issuer.example".to_string(),
                audience: Some("harn-serve".to_string()),
                required_scopes: scopes(&["invoke"]),
                tenant_id: None,
            })],
            mcp_allowlist: None,
        };
        let request = AuthRequest {
            validated_oauth: Some(OAuthClaims {
                subject: "alice".to_string(),
                issuer: "https://issuer.example".to_string(),
                audience: Some("harn-serve".to_string()),
                scopes: scopes(&["invoke", "read"]),
                tenant_id: None,
            }),
            ..AuthRequest::default()
        };

        let decision = policy.authorize(&request).await;
        assert_eq!(
            decision,
            AuthorizationDecision::Authorized(AuthenticatedPrincipal {
                subject: "alice".to_string(),
                scheme: "oauth21".to_string(),
                granted_scopes: scopes(&["invoke", "read"]),
                tenant_id: None,
            })
        );
    }

    #[tokio::test]
    async fn oauth_policy_rejects_missing_required_audience() {
        let policy = AuthPolicy {
            methods: vec![AuthMethodConfig::OAuth21(OAuth21AuthConfig {
                issuer: "https://issuer.example".to_string(),
                audience: Some("harn-serve".to_string()),
                required_scopes: BTreeSet::new(),
                tenant_id: None,
            })],
            mcp_allowlist: None,
        };
        let request = AuthRequest {
            validated_oauth: Some(OAuthClaims {
                subject: "alice".to_string(),
                issuer: "https://issuer.example".to_string(),
                audience: None,
                scopes: BTreeSet::new(),
                tenant_id: None,
            }),
            ..AuthRequest::default()
        };

        let decision = policy.authorize(&request).await;
        assert_eq!(
            decision,
            AuthorizationDecision::Rejected("oauth audience mismatch".to_string())
        );
    }

    #[tokio::test]
    async fn api_key_policy_carries_per_key_scopes_into_principal() {
        let policy = AuthPolicy {
            methods: vec![AuthMethodConfig::ApiKey(ApiKeyAuthConfig {
                keys: vec![
                    ApiKeyEntry::new("alice-key", ["personas:read".to_string()]),
                    ApiKeyEntry::new(
                        "bob-key",
                        ["sessions:write".to_string(), "personas:read".to_string()],
                    ),
                ],
            })],
            mcp_allowlist: None,
        };
        let request = AuthRequest {
            headers: BTreeMap::from([(
                "authorization".to_string(),
                "Bearer alice-key".to_string(),
            )]),
            ..AuthRequest::default()
        };

        let AuthorizationDecision::Authorized(principal) = policy.authorize(&request).await else {
            panic!("expected Authorized");
        };
        assert_eq!(principal.granted_scopes, scopes(&["personas:read"]));
    }

    #[tokio::test]
    async fn scope_check_rejects_principal_missing_required_scope() {
        let policy = AuthPolicy {
            methods: vec![AuthMethodConfig::ApiKey(ApiKeyAuthConfig {
                keys: vec![ApiKeyEntry::new(
                    "limited-key",
                    ["sessions:read".to_string()],
                )],
            })],
            mcp_allowlist: None,
        };
        let request = AuthRequest {
            headers: BTreeMap::from([(
                "authorization".to_string(),
                "Bearer limited-key".to_string(),
            )]),
            ..AuthRequest::default()
        };

        let decision = policy
            .authorize_with_scopes(&request, &scopes(&["personas:read"]))
            .await;
        assert_eq!(
            decision,
            AuthorizationDecision::MissingScope {
                required: scopes(&["personas:read"]),
                granted: scopes(&["sessions:read"]),
            }
        );
    }

    #[tokio::test]
    async fn scope_check_accepts_principal_with_superset_of_required_scopes() {
        let policy = AuthPolicy {
            methods: vec![AuthMethodConfig::ApiKey(ApiKeyAuthConfig {
                keys: vec![ApiKeyEntry::new(
                    "admin-key",
                    [
                        "personas:read".to_string(),
                        "sessions:read".to_string(),
                        "sessions:write".to_string(),
                    ],
                )],
            })],
            mcp_allowlist: None,
        };
        let request = AuthRequest {
            headers: BTreeMap::from([(
                "authorization".to_string(),
                "Bearer admin-key".to_string(),
            )]),
            ..AuthRequest::default()
        };

        let decision = policy
            .authorize_with_scopes(&request, &scopes(&["personas:read", "sessions:read"]))
            .await;
        assert!(matches!(decision, AuthorizationDecision::Authorized(_)));
    }

    #[tokio::test]
    async fn scope_check_short_circuits_to_rejected_when_authentication_fails() {
        let policy = AuthPolicy {
            methods: vec![AuthMethodConfig::ApiKey(ApiKeyAuthConfig::single("secret"))],
            mcp_allowlist: None,
        };
        let request = AuthRequest::default();
        let decision = policy
            .authorize_with_scopes(&request, &scopes(&["personas:read"]))
            .await;
        assert!(matches!(decision, AuthorizationDecision::Rejected(_)));
    }

    #[tokio::test]
    async fn hmac_principal_carries_configured_granted_scopes() {
        let timestamp = OffsetDateTime::now_utc().unix_timestamp().to_string();
        let body = b"{}";
        let hash = Sha256::digest(body);
        let body_hash = hex::encode(hash);
        let signed = format!("POST\n/mcp\n{timestamp}\n{body_hash}");
        let mut mac = Hmac::<Sha256>::new_from_slice(b"shared-secret").expect("mac key");
        mac.update(signed.as_bytes());
        let signature =
            base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());
        let authorization = format!("HMAC-SHA256 timestamp={timestamp},signature={signature}");

        let policy = AuthPolicy {
            methods: vec![AuthMethodConfig::Hmac(HmacAuthConfig {
                shared_secret: "shared-secret".to_string(),
                provider: "harn-serve".to_string(),
                timestamp_window: Duration::seconds(60),
                granted_scopes: scopes(&["personas:read"]),
                tenant_id: None,
            })],
            mcp_allowlist: None,
        };
        let request = AuthRequest {
            method: "POST".to_string(),
            path: "/mcp".to_string(),
            body: body.to_vec(),
            headers: BTreeMap::from([("authorization".to_string(), authorization)]),
            validated_oauth: None,
            tenant_id: None,
        };

        let decision = policy
            .authorize_with_scopes(&request, &scopes(&["personas:read"]))
            .await;
        let AuthorizationDecision::Authorized(principal) = decision else {
            panic!("expected Authorized");
        };
        assert_eq!(principal.granted_scopes, scopes(&["personas:read"]));
    }

    #[tokio::test]
    async fn api_key_with_tenant_carries_tenant_into_principal() {
        let policy = AuthPolicy {
            methods: vec![AuthMethodConfig::ApiKey(ApiKeyAuthConfig {
                keys: vec![ApiKeyEntry::new("tenant-key", []).with_tenant("acme")],
            })],
        };
        let request = AuthRequest {
            headers: BTreeMap::from([(
                "authorization".to_string(),
                "Bearer tenant-key".to_string(),
            )]),
            ..AuthRequest::default()
        };

        let AuthorizationDecision::Authorized(principal) = policy.authorize(&request).await else {
            panic!("expected Authorized");
        };
        assert_eq!(principal.tenant_id, Some(TenantId::new("acme")));
    }

    #[tokio::test]
    async fn request_tenant_override_wins_over_method_tenant() {
        let policy = AuthPolicy {
            methods: vec![AuthMethodConfig::ApiKey(ApiKeyAuthConfig {
                keys: vec![ApiKeyEntry::new("key", []).with_tenant("baseline")],
            })],
        };
        let request = AuthRequest {
            headers: BTreeMap::from([("authorization".to_string(), "Bearer key".to_string())]),
            tenant_id: Some(TenantId::new("override")),
            ..AuthRequest::default()
        };

        let AuthorizationDecision::Authorized(principal) = policy.authorize(&request).await else {
            panic!("expected Authorized");
        };
        assert_eq!(principal.tenant_id, Some(TenantId::new("override")));
    }

    #[tokio::test]
    async fn oauth_claims_tenant_wins_over_policy_fallback() {
        let policy = AuthPolicy {
            methods: vec![AuthMethodConfig::OAuth21(OAuth21AuthConfig {
                issuer: "https://issuer.example".to_string(),
                audience: None,
                required_scopes: BTreeSet::new(),
                tenant_id: Some(TenantId::new("policy-default")),
            })],
        };
        let request = AuthRequest {
            validated_oauth: Some(OAuthClaims {
                subject: "alice".to_string(),
                issuer: "https://issuer.example".to_string(),
                audience: None,
                scopes: BTreeSet::new(),
                tenant_id: Some(TenantId::new("token-bound")),
            }),
            ..AuthRequest::default()
        };

        let AuthorizationDecision::Authorized(principal) = policy.authorize(&request).await else {
            panic!("expected Authorized");
        };
        assert_eq!(principal.tenant_id, Some(TenantId::new("token-bound")));
    }

    #[tokio::test]
    async fn oauth_policy_fallback_tenant_used_when_claims_have_none() {
        let policy = AuthPolicy {
            methods: vec![AuthMethodConfig::OAuth21(OAuth21AuthConfig {
                issuer: "https://issuer.example".to_string(),
                audience: None,
                required_scopes: BTreeSet::new(),
                tenant_id: Some(TenantId::new("policy-default")),
            })],
        };
        let request = AuthRequest {
            validated_oauth: Some(OAuthClaims {
                subject: "alice".to_string(),
                issuer: "https://issuer.example".to_string(),
                audience: None,
                scopes: BTreeSet::new(),
                tenant_id: None,
            }),
            ..AuthRequest::default()
        };

        let AuthorizationDecision::Authorized(principal) = policy.authorize(&request).await else {
            panic!("expected Authorized");
        };
        assert_eq!(principal.tenant_id, Some(TenantId::new("policy-default")));
    }

    #[tokio::test]
    async fn empty_policy_grants_no_scopes_so_required_scopes_reject() {
        let policy = AuthPolicy::allow_all();
        let request = AuthRequest::default();
        let decision = policy
            .authorize_with_scopes(&request, &scopes(&["personas:read"]))
            .await;
        assert_eq!(
            decision,
            AuthorizationDecision::MissingScope {
                required: scopes(&["personas:read"]),
                granted: BTreeSet::new(),
            }
        );
    }
}
