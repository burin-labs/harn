//! Bulk MCP OAuth driver (harn#3355) — the keystone of the bulk-login program
//! (harn#3354).
//!
//! Authenticating many OAuth-backed MCP servers today is one-at-a-time by
//! construction. This driver orchestrates **all** pending flows at once on top
//! of the unchanged per-server engine in [`crate::mcp_oauth`], and publishes a
//! **per-server status event stream** so every surface (CLI `mcp login --all`,
//! ACP `mcp/authorize_batch`, the burin "Connect all" GUI) renders incremental
//! progress from one source. No protocol changes — bulk auth is N independent,
//! spec-compliant flows that share one driver and one loopback listener
//! (demuxed by the OAuth `state`).
//!
//! Division of labour: the driver owns flow orchestration + status; the
//! **surface** owns IO (opening browsers, the loopback listener, terminal/GUI
//! rendering). [`McpBulkAuth::prepare`] returns the authorize URLs to open
//! (keyed by `state`); the surface opens them (serialized, to avoid a popup
//! storm) and feeds each captured `{ state, code }` back via
//! [`McpBulkAuth::complete`].
//!
//! The OAuth operations are abstracted behind [`OAuthFlowEngine`] so the driver
//! is unit-testable end-to-end against a mock — no network, no browser. The
//! real engine ([`RealOAuthFlowEngine`]) simply delegates to `mcp_oauth`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::mcp_auth::{canonical_resource_indicator, OAuthClientAuthMode};
use crate::mcp_oauth::{self, BeginAuthorization, PendingAuthorization, StoredMcpToken};

/// Broadcast backlog for the status stream. Comfortably exceeds the number of
/// servers a workspace realistically declares; a slow subscriber that lags
/// past this loses old events (it can re-query `mcp status`), never the driver.
const STATUS_CHANNEL_CAPACITY: usize = 256;

/// Which servers a bulk pass should authenticate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BulkAuthMode {
    /// First-auth: begin a flow only for servers without a currently-valid
    /// bearer; skip already-connected ones. (`mcp login --all`.)
    Missing,
    /// Re-auth: begin a flow only for servers that *have* a stored token which
    /// is no longer valid (expired/revoked); skip valid tokens and servers with
    /// no token at all. (`mcp.reauth_expired()` / harn#3358.)
    Expired,
    /// Force a fresh flow for every server regardless of current state.
    All,
}

/// One server to (maybe) authenticate. Mirrors the per-server inputs the
/// engine's [`BeginAuthorization`] needs, plus a display `name` for status.
#[derive(Clone, Debug, Default)]
pub struct BulkAuthServer {
    pub name: String,
    pub server_url: String,
    pub mode: Option<OAuthClientAuthMode>,
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    pub static_secret_id: Option<String>,
    pub scopes: Option<String>,
}

/// A flow that needs the surface to open a browser. The surface opens
/// `authorize_url`, captures the redirect, and calls [`McpBulkAuth::complete`]
/// with the `state` echoed back.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparedFlow {
    pub name: String,
    pub server_url: String,
    pub authorize_url: String,
    pub state: String,
    pub redirect_uri: String,
}

/// The result of preparing one server.
#[derive(Clone, Debug)]
pub enum PrepareOutcome {
    /// Needs a browser consent; open `authorize_url`.
    Pending(PreparedFlow),
    /// Nothing to do (already connected, or nothing to re-auth).
    Skipped {
        name: String,
        server_url: String,
        reason: String,
    },
    /// Could not begin a flow (discovery/registration/timeout failure).
    Failed {
        name: String,
        server_url: String,
        error: String,
    },
}

/// Phase of one server's bulk-auth lifecycle, streamed to subscribers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpAuthPhase {
    /// Resolving the authorization server + client (discovery/registration).
    Discovering,
    /// Authorize URL minted; waiting for the user to consent in the browser.
    AwaitingConsent,
    /// Exchanging the authorization code for a token.
    Exchanging,
    /// Token stored — the server is connected.
    Connected,
    /// This server failed; `detail` carries a redacted reason.
    Failed,
    /// This server was skipped (e.g. already connected); `detail` says why.
    Skipped,
}

/// One per-server status event on the stream.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct McpAuthStatus {
    /// Display name of the server.
    pub server: String,
    /// Server URL (empty when unknown, e.g. a late callback).
    pub server_url: String,
    /// Current phase.
    pub phase: McpAuthPhase,
    /// Human-readable note or redacted error, when relevant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Driver tunables. **Data, not code:** loaded via [`BulkAuthConfig::load`]
/// from `HARN_MCP_BULK_AUTH_CONFIG` or `~/.config/harn/mcp_bulk_auth.toml`
/// (a `[bulk_auth]` table), so concurrency/timeout change without a recompile.
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(default)]
pub struct BulkAuthConfig {
    /// Max concurrent `begin_authorization` flows during prepare.
    pub concurrency: usize,
    /// Per-server budget for discovery + authorize-URL minting.
    pub prepare_timeout_secs: u64,
}

impl Default for BulkAuthConfig {
    fn default() -> Self {
        Self {
            concurrency: 8,
            prepare_timeout_secs: 30,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct BulkAuthConfigFile {
    #[serde(default)]
    bulk_auth: BulkAuthConfig,
}

impl BulkAuthConfig {
    /// Resolve the effective config: the `HARN_MCP_BULK_AUTH_CONFIG` path wins,
    /// else `~/.config/harn/mcp_bulk_auth.toml`, else defaults. Skipped under
    /// `cfg(test)` so unit tests are deterministic.
    pub fn load() -> Self {
        if let Ok(path) = std::env::var("HARN_MCP_BULK_AUTH_CONFIG") {
            if let Some(config) = Self::read(&path) {
                return config;
            }
        }
        if !cfg!(test) {
            if let Some(home) = crate::user_dirs::home_dir() {
                let path = home.join(".config").join("harn").join("mcp_bulk_auth.toml");
                if let Some(config) = Self::read(&path.to_string_lossy()) {
                    return config;
                }
            }
        }
        Self::default()
    }

    fn read(path: &str) -> Option<Self> {
        let content = std::fs::read_to_string(path).ok()?;
        match toml::from_str::<BulkAuthConfigFile>(&content) {
            Ok(file) => Some(file.bulk_auth),
            Err(error) => {
                eprintln!("[mcp_bulk_auth] TOML parse error in {path}: {error}");
                None
            }
        }
    }
}

/// The OAuth operations the driver needs, abstracted so it can be exercised
/// against a mock. The real impl delegates to [`crate::mcp_oauth`].
#[async_trait]
pub trait OAuthFlowEngine: Send + Sync {
    /// A currently-valid bearer for the server (refreshing if needed), or
    /// `None` when none is stored / the stored one cannot be made valid.
    async fn current_bearer(&self, server_url: &str) -> Result<Option<String>, String>;
    /// Whether *any* token is stored for the server (valid or not).
    async fn has_token(&self, server_url: &str) -> Result<bool, String>;
    /// Begin an authorization, returning the authorize URL + `state`.
    async fn begin(&self, request: BeginAuthorization) -> Result<PendingAuthorization, String>;
    /// Complete a flow by `state`, exchanging the code and persisting the token.
    async fn complete(
        &self,
        state: &str,
        code: &str,
        issuer: Option<&str>,
    ) -> Result<StoredMcpToken, String>;
}

/// Production [`OAuthFlowEngine`] — a thin delegation to `mcp_oauth`.
#[derive(Clone, Copy, Debug, Default)]
pub struct RealOAuthFlowEngine;

#[async_trait]
impl OAuthFlowEngine for RealOAuthFlowEngine {
    async fn current_bearer(&self, server_url: &str) -> Result<Option<String>, String> {
        mcp_oauth::resolve_bearer(server_url).await
    }

    async fn has_token(&self, server_url: &str) -> Result<bool, String> {
        let discovery = mcp_oauth::discover(server_url).await?;
        let resource =
            canonical_resource_indicator(server_url).map_err(|error| error.to_string())?;
        Ok(
            mcp_oauth::load_token(&resource, &discovery.authorization_server_issuer, None)
                .await?
                .is_some(),
        )
    }

    async fn begin(&self, request: BeginAuthorization) -> Result<PendingAuthorization, String> {
        mcp_oauth::begin_authorization(request).await
    }

    async fn complete(
        &self,
        state: &str,
        code: &str,
        issuer: Option<&str>,
    ) -> Result<StoredMcpToken, String> {
        mcp_oauth::complete_authorization(state, code, issuer).await
    }
}

/// Name + URL remembered for a pending `state` so [`McpBulkAuth::complete`] can
/// label its status events without the surface re-supplying them.
#[derive(Clone, Debug)]
struct FlowMeta {
    name: String,
    server_url: String,
}

/// Bulk OAuth orchestrator. Construct once, [`subscribe`](Self::subscribe) for
/// the status stream, [`prepare`](Self::prepare) to begin all pending flows,
/// then [`complete`](Self::complete) each captured callback.
pub struct McpBulkAuth<E: OAuthFlowEngine = RealOAuthFlowEngine> {
    engine: Arc<E>,
    config: BulkAuthConfig,
    status_tx: broadcast::Sender<McpAuthStatus>,
    pending: Arc<Mutex<HashMap<String, FlowMeta>>>,
}

impl McpBulkAuth<RealOAuthFlowEngine> {
    /// Construct the driver backed by the real `mcp_oauth` engine, with config
    /// resolved from the overlay.
    pub fn new() -> Self {
        Self::with_engine(RealOAuthFlowEngine, BulkAuthConfig::load())
    }
}

impl Default for McpBulkAuth<RealOAuthFlowEngine> {
    fn default() -> Self {
        Self::new()
    }
}

impl<E: OAuthFlowEngine> McpBulkAuth<E> {
    /// Construct with an explicit engine + config (the test/injection seam).
    pub fn with_engine(engine: E, config: BulkAuthConfig) -> Self {
        let (status_tx, _rx) = broadcast::channel(STATUS_CHANNEL_CAPACITY);
        Self {
            engine: Arc::new(engine),
            config,
            status_tx,
            pending: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Subscribe to the per-server status stream. Subscribe *before* calling
    /// [`prepare`](Self::prepare) to observe every event.
    pub fn subscribe(&self) -> broadcast::Receiver<McpAuthStatus> {
        self.status_tx.subscribe()
    }

    /// Begin flows for all servers selected by `mode`, concurrently (bounded by
    /// the configured concurrency). All flows share `redirect_uri` — the surface
    /// binds one loopback listener and demuxes callbacks by `state`. Returns one
    /// [`PrepareOutcome`] per input server; emits status events throughout.
    pub async fn prepare(
        &self,
        servers: Vec<BulkAuthServer>,
        mode: BulkAuthMode,
        redirect_uri: &str,
    ) -> Vec<PrepareOutcome> {
        let concurrency = self.config.concurrency.max(1);
        let timeout = Duration::from_secs(self.config.prepare_timeout_secs.max(1));
        futures::stream::iter(servers.into_iter().map(|server| {
            let engine = self.engine.clone();
            let status_tx = self.status_tx.clone();
            let pending = self.pending.clone();
            let redirect_uri = redirect_uri.to_string();
            async move {
                prepare_one(
                    engine,
                    status_tx,
                    pending,
                    server,
                    mode,
                    redirect_uri,
                    timeout,
                )
                .await
            }
        }))
        .buffer_unordered(concurrency)
        .collect::<Vec<_>>()
        .await
    }

    /// Complete a flow whose callback the surface captured. Looks up the
    /// server's display name by `state`, emits `Exchanging` then
    /// `Connected`/`Failed`, and returns the stored token on success.
    pub async fn complete(
        &self,
        state: &str,
        code: &str,
        issuer: Option<&str>,
    ) -> Result<StoredMcpToken, String> {
        let meta = self
            .pending
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .get(state)
            .cloned();
        let (name, server_url) = match meta {
            Some(meta) => (meta.name, meta.server_url),
            None => ("<unknown>".to_string(), String::new()),
        };
        emit(
            &self.status_tx,
            &name,
            &server_url,
            McpAuthPhase::Exchanging,
            None,
        );
        match self.engine.complete(state, code, issuer).await {
            Ok(token) => {
                self.pending
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .remove(state);
                emit(
                    &self.status_tx,
                    &name,
                    &server_url,
                    McpAuthPhase::Connected,
                    None,
                );
                Ok(token)
            }
            Err(error) => {
                emit(
                    &self.status_tx,
                    &name,
                    &server_url,
                    McpAuthPhase::Failed,
                    Some(error.clone()),
                );
                Err(error)
            }
        }
    }

    /// Number of flows still awaiting a callback (begun but not completed).
    pub fn pending_count(&self) -> usize {
        self.pending
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .len()
    }
}

/// Begin (or skip) one server, emitting its phase events. Free function so the
/// per-server future owns only cloned handles (no `&self` borrow across the
/// concurrent stream).
async fn prepare_one<E: OAuthFlowEngine>(
    engine: Arc<E>,
    status_tx: broadcast::Sender<McpAuthStatus>,
    pending: Arc<Mutex<HashMap<String, FlowMeta>>>,
    server: BulkAuthServer,
    mode: BulkAuthMode,
    redirect_uri: String,
    timeout: Duration,
) -> PrepareOutcome {
    emit(
        &status_tx,
        &server.name,
        &server.server_url,
        McpAuthPhase::Discovering,
        None,
    );

    match tokio::time::timeout(timeout, decide(&*engine, &server, mode)).await {
        Ok(AuthDecision::Begin) => {}
        Ok(AuthDecision::Skip(reason)) => {
            emit(
                &status_tx,
                &server.name,
                &server.server_url,
                McpAuthPhase::Skipped,
                Some(reason.to_string()),
            );
            return PrepareOutcome::Skipped {
                name: server.name,
                server_url: server.server_url,
                reason: reason.to_string(),
            };
        }
        Err(_) => {
            return fail(
                &status_tx,
                server,
                "timed out resolving authorization server",
            );
        }
    }

    let request = BeginAuthorization {
        server_url: server.server_url.clone(),
        redirect_uri: redirect_uri.clone(),
        mode: server.mode,
        client_id: server.client_id.clone(),
        client_secret: server.client_secret.clone(),
        static_secret_id: server.static_secret_id.clone(),
        scopes: server.scopes.clone(),
    };
    match tokio::time::timeout(timeout, engine.begin(request)).await {
        Ok(Ok(pending_auth)) => {
            pending
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .insert(
                    pending_auth.state.clone(),
                    FlowMeta {
                        name: server.name.clone(),
                        server_url: server.server_url.clone(),
                    },
                );
            emit(
                &status_tx,
                &server.name,
                &server.server_url,
                McpAuthPhase::AwaitingConsent,
                None,
            );
            PrepareOutcome::Pending(PreparedFlow {
                name: server.name,
                server_url: server.server_url,
                authorize_url: pending_auth.authorize_url,
                state: pending_auth.state,
                redirect_uri,
            })
        }
        Ok(Err(error)) => fail(&status_tx, server, &error),
        Err(_) => fail(&status_tx, server, "timed out minting authorization URL"),
    }
}

/// Outcome of the per-mode "does this server need a flow?" decision.
enum AuthDecision {
    Begin,
    Skip(&'static str),
}

async fn decide<E: OAuthFlowEngine>(
    engine: &E,
    server: &BulkAuthServer,
    mode: BulkAuthMode,
) -> AuthDecision {
    match mode {
        BulkAuthMode::All => AuthDecision::Begin,
        BulkAuthMode::Missing => match engine.current_bearer(&server.server_url).await {
            Ok(Some(_)) => AuthDecision::Skip("already connected"),
            // None or error → not currently usable; begin (begin surfaces any
            // real discovery error as a Failed outcome).
            _ => AuthDecision::Begin,
        },
        BulkAuthMode::Expired => {
            // Only re-auth servers that have a token which is no longer valid.
            match engine.has_token(&server.server_url).await {
                Ok(false) => return AuthDecision::Skip("no stored token"),
                Ok(true) => {}
                Err(_) => return AuthDecision::Skip("no stored token"),
            }
            match engine.current_bearer(&server.server_url).await {
                Ok(Some(_)) => AuthDecision::Skip("token still valid"),
                _ => AuthDecision::Begin,
            }
        }
    }
}

fn fail(
    status_tx: &broadcast::Sender<McpAuthStatus>,
    server: BulkAuthServer,
    error: &str,
) -> PrepareOutcome {
    emit(
        status_tx,
        &server.name,
        &server.server_url,
        McpAuthPhase::Failed,
        Some(error.to_string()),
    );
    PrepareOutcome::Failed {
        name: server.name,
        server_url: server.server_url,
        error: error.to_string(),
    }
}

fn emit(
    status_tx: &broadcast::Sender<McpAuthStatus>,
    server: &str,
    server_url: &str,
    phase: McpAuthPhase,
    detail: Option<String>,
) {
    // A send with no live receivers is fine — status is best-effort telemetry.
    let _ = status_tx.send(McpAuthStatus {
        server: server.to_string(),
        server_url: server_url.to_string(),
        phase,
        detail,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Scripted mock engine: per-URL valid/stored-token state + a begin/complete
    /// counter, so tests assert phase sequences and mode filtering with no
    /// network or browser.
    #[derive(Default)]
    struct MockEngine {
        /// URLs that currently have a valid bearer.
        valid: Vec<String>,
        /// URLs that have a stored token (valid or not).
        stored: Vec<String>,
        /// URLs whose `begin` should fail.
        begin_fails: Vec<String>,
        begin_calls: AtomicUsize,
        state_counter: AtomicUsize,
    }

    #[async_trait]
    impl OAuthFlowEngine for MockEngine {
        async fn current_bearer(&self, server_url: &str) -> Result<Option<String>, String> {
            Ok(self
                .valid
                .iter()
                .any(|u| u == server_url)
                .then(|| "bearer".to_string()))
        }
        async fn has_token(&self, server_url: &str) -> Result<bool, String> {
            Ok(self.stored.iter().any(|u| u == server_url))
        }
        async fn begin(&self, request: BeginAuthorization) -> Result<PendingAuthorization, String> {
            self.begin_calls.fetch_add(1, Ordering::SeqCst);
            if self.begin_fails.contains(&request.server_url) {
                return Err("discovery exploded".to_string());
            }
            let n = self.state_counter.fetch_add(1, Ordering::SeqCst);
            let state = format!("state-{n}");
            Ok(PendingAuthorization {
                authorize_url: format!("https://auth.example/authorize?state={state}"),
                state,
                redirect_uri: request.redirect_uri,
                resource: request.server_url,
                issuer: "https://auth.example".to_string(),
            })
        }
        async fn complete(
            &self,
            state: &str,
            _code: &str,
            _issuer: Option<&str>,
        ) -> Result<StoredMcpToken, String> {
            if state == "bad-state" {
                return Err("token exchange failed".to_string());
            }
            Ok(StoredMcpToken {
                access_token: "access".to_string(),
                refresh_token: None,
                expires_at_unix: None,
                token_endpoint: "https://auth.example/token".to_string(),
                client_id: "client".to_string(),
                client_secret: None,
                token_endpoint_auth_method: "none".to_string(),
                issuer: "https://auth.example".to_string(),
                resource: "https://mcp.example/mcp".to_string(),
                scopes: None,
                token_response_extra: None,
            })
        }
    }

    fn server(name: &str, url: &str) -> BulkAuthServer {
        BulkAuthServer {
            name: name.to_string(),
            server_url: url.to_string(),
            ..Default::default()
        }
    }

    fn driver(engine: MockEngine) -> McpBulkAuth<MockEngine> {
        // concurrency 1 keeps the mock's `state-N` assignment deterministic.
        McpBulkAuth::with_engine(
            engine,
            BulkAuthConfig {
                concurrency: 1,
                prepare_timeout_secs: 5,
            },
        )
    }

    async fn drain(rx: &mut broadcast::Receiver<McpAuthStatus>) -> Vec<McpAuthStatus> {
        let mut out = Vec::new();
        while let Ok(status) = rx.try_recv() {
            out.push(status);
        }
        out
    }

    fn phases(events: &[McpAuthStatus], server: &str) -> Vec<McpAuthPhase> {
        events
            .iter()
            .filter(|e| e.server == server)
            .map(|e| e.phase)
            .collect()
    }

    #[tokio::test]
    async fn prepares_all_servers_and_emits_phase_sequence() {
        let driver = driver(MockEngine::default());
        let mut rx = driver.subscribe();
        let outcomes = driver
            .prepare(
                vec![
                    server("a", "https://a.example/mcp"),
                    server("b", "https://b.example/mcp"),
                    server("c", "https://c.example/mcp"),
                ],
                BulkAuthMode::All,
                "http://127.0.0.1:9783/callback",
            )
            .await;

        assert_eq!(outcomes.len(), 3);
        assert!(outcomes
            .iter()
            .all(|o| matches!(o, PrepareOutcome::Pending(_))));
        assert_eq!(driver.pending_count(), 3);

        let events = drain(&mut rx).await;
        for name in ["a", "b", "c"] {
            assert_eq!(
                phases(&events, name),
                vec![McpAuthPhase::Discovering, McpAuthPhase::AwaitingConsent],
                "server {name}"
            );
        }
    }

    #[tokio::test]
    async fn missing_mode_skips_connected_servers() {
        let engine = MockEngine {
            valid: vec!["https://b.example/mcp".to_string()],
            ..Default::default()
        };
        let driver = driver(engine);
        let outcomes = driver
            .prepare(
                vec![
                    server("a", "https://a.example/mcp"),
                    server("b", "https://b.example/mcp"),
                ],
                BulkAuthMode::Missing,
                "http://127.0.0.1:9783/callback",
            )
            .await;

        let a = outcomes.iter().find(|o| outcome_name(o) == "a").unwrap();
        let b = outcomes.iter().find(|o| outcome_name(o) == "b").unwrap();
        assert!(matches!(a, PrepareOutcome::Pending(_)));
        assert!(
            matches!(b, PrepareOutcome::Skipped { reason, .. } if reason == "already connected")
        );
    }

    #[tokio::test]
    async fn expired_mode_only_reauths_stale_stored_tokens() {
        let engine = MockEngine {
            // "stale" has a stored-but-invalid token, "fresh" is valid, "none"
            // has nothing stored.
            valid: vec!["https://fresh.example/mcp".to_string()],
            stored: vec![
                "https://stale.example/mcp".to_string(),
                "https://fresh.example/mcp".to_string(),
            ],
            ..Default::default()
        };
        let driver = driver(engine);
        let outcomes = driver
            .prepare(
                vec![
                    server("stale", "https://stale.example/mcp"),
                    server("fresh", "https://fresh.example/mcp"),
                    server("none", "https://none.example/mcp"),
                ],
                BulkAuthMode::Expired,
                "http://127.0.0.1:9783/callback",
            )
            .await;

        let stale = outcomes
            .iter()
            .find(|o| outcome_name(o) == "stale")
            .unwrap();
        let fresh = outcomes
            .iter()
            .find(|o| outcome_name(o) == "fresh")
            .unwrap();
        let none = outcomes.iter().find(|o| outcome_name(o) == "none").unwrap();
        assert!(
            matches!(stale, PrepareOutcome::Pending(_)),
            "stale → re-auth"
        );
        assert!(
            matches!(fresh, PrepareOutcome::Skipped { reason, .. } if reason == "token still valid")
        );
        assert!(
            matches!(none, PrepareOutcome::Skipped { reason, .. } if reason == "no stored token")
        );
    }

    #[tokio::test]
    async fn one_servers_failure_is_isolated() {
        let engine = MockEngine {
            begin_fails: vec!["https://b.example/mcp".to_string()],
            ..Default::default()
        };
        let driver = driver(engine);
        let mut rx = driver.subscribe();
        let outcomes = driver
            .prepare(
                vec![
                    server("a", "https://a.example/mcp"),
                    server("b", "https://b.example/mcp"),
                    server("c", "https://c.example/mcp"),
                ],
                BulkAuthMode::All,
                "http://127.0.0.1:9783/callback",
            )
            .await;

        let b = outcomes.iter().find(|o| outcome_name(o) == "b").unwrap();
        assert!(matches!(b, PrepareOutcome::Failed { error, .. } if error.contains("discovery")));
        // a and c still succeeded.
        assert_eq!(
            outcomes
                .iter()
                .filter(|o| matches!(o, PrepareOutcome::Pending(_)))
                .count(),
            2
        );
        let events = drain(&mut rx).await;
        assert_eq!(
            phases(&events, "b"),
            vec![McpAuthPhase::Discovering, McpAuthPhase::Failed]
        );
    }

    #[tokio::test]
    async fn complete_routes_by_state_and_streams_terminal_phase() {
        let driver = driver(MockEngine::default());
        let mut rx = driver.subscribe();
        let outcomes = driver
            .prepare(
                vec![server("a", "https://a.example/mcp")],
                BulkAuthMode::All,
                "http://127.0.0.1:9783/callback",
            )
            .await;
        let state = match &outcomes[0] {
            PrepareOutcome::Pending(flow) => flow.state.clone(),
            other => panic!("expected pending, got {other:?}"),
        };
        let _ = drain(&mut rx).await;

        let token = driver.complete(&state, "auth-code", None).await.unwrap();
        assert_eq!(token.access_token, "access");
        assert_eq!(driver.pending_count(), 0, "completed flow is cleared");

        let events = drain(&mut rx).await;
        assert_eq!(
            phases(&events, "a"),
            vec![McpAuthPhase::Exchanging, McpAuthPhase::Connected]
        );
    }

    #[tokio::test]
    async fn complete_failure_emits_failed_and_keeps_pending() {
        let driver = driver(MockEngine::default());
        // Seed a pending flow whose state the mock will reject.
        driver.pending.lock().unwrap().insert(
            "bad-state".to_string(),
            FlowMeta {
                name: "a".to_string(),
                server_url: "https://a.example/mcp".to_string(),
            },
        );
        let mut rx = driver.subscribe();
        let error = driver
            .complete("bad-state", "code", None)
            .await
            .unwrap_err();
        assert!(error.contains("token exchange failed"));
        let events = drain(&mut rx).await;
        assert_eq!(
            phases(&events, "a"),
            vec![McpAuthPhase::Exchanging, McpAuthPhase::Failed]
        );
    }

    #[test]
    fn status_serializes_snake_case() {
        let json = serde_json::to_value(McpAuthStatus {
            server: "Notion".to_string(),
            server_url: "https://mcp.notion.com/mcp".to_string(),
            phase: McpAuthPhase::AwaitingConsent,
            detail: None,
        })
        .unwrap();
        assert_eq!(json["server"], serde_json::json!("Notion"));
        assert_eq!(json["phase"], serde_json::json!("awaiting_consent"));
        assert!(json.get("detail").is_none(), "None detail is omitted");
    }

    #[test]
    fn config_defaults_when_no_overlay() {
        let config = BulkAuthConfig::load();
        assert_eq!(config.concurrency, 8);
        assert_eq!(config.prepare_timeout_secs, 30);
    }

    fn outcome_name(outcome: &PrepareOutcome) -> &str {
        match outcome {
            PrepareOutcome::Pending(flow) => &flow.name,
            PrepareOutcome::Skipped { name, .. } => name,
            PrepareOutcome::Failed { name, .. } => name,
        }
    }
}
