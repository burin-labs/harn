use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use std::{env, fs, process};

use harn_vm::mcp_auth::OAuthClientAuthMode;
use harn_vm::mcp_bulk_auth::{
    BulkAuthConfig, BulkAuthMode, BulkAuthServer, McpAuthPhase, McpAuthStatus, McpBulkAuth,
    PrepareOutcome, RealOAuthFlowEngine,
};
use harn_vm::mcp_oauth::{self, BeginAuthorization};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::cli::{McpCommand, McpDiscoverArgs, McpLoginArgs, McpServerRefArgs};
use crate::json_envelope::{self, JsonEnvelope};
use crate::package::{self, McpAuthConfig, McpServerConfig};

mod call;
mod mock;
mod oauth_resource;
pub(crate) mod presets;
pub(crate) mod serve;
mod stdio_client;

const DEFAULT_REDIRECT_URI: &str = "http://127.0.0.1:9783/oauth/callback";
use harn_vm::mcp_protocol::PROTOCOL_VERSION as MCP_PROTOCOL_VERSION;

#[derive(Clone)]
pub(crate) struct ResolvedMcpServer {
    pub name: String,
    pub url: String,
    pub auth: Option<McpAuthConfig>,
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    pub scopes: Option<String>,
}

pub(crate) enum AuthResolution {
    None,
    StaticBearer(String),
    OAuthStore,
}

pub(crate) async fn handle_mcp_command(command: &McpCommand) {
    match command {
        McpCommand::Serve(args) => {
            if let Err(error) = serve::run(args).await {
                eprintln!("error: {error}");
                process::exit(1);
            }
        }
        McpCommand::Call(args) => match call::run(args).await {
            Ok(0) => {}
            Ok(code) => process::exit(code),
            Err(error) => {
                eprintln!("error: {error}");
                process::exit(1);
            }
        },
        McpCommand::Mock(args) => match mock::run(&args.command).await {
            Ok(0) => {}
            Ok(code) => process::exit(code),
            Err(error) => {
                eprintln!("error: {error}");
                process::exit(1);
            }
        },
        McpCommand::Login(options) => {
            if let Err(error) = login(options).await {
                eprintln!("error: {error}");
                process::exit(1);
            }
        }
        McpCommand::Logout(server_ref) => {
            let server = resolve_server_reference(server_ref).unwrap_or_else(|error| {
                eprintln!("error: {error}");
                process::exit(1);
            });
            let discovery = mcp_oauth::discover(&server.url)
                .await
                .unwrap_or_else(|error| {
                    eprintln!("error: {error}");
                    process::exit(1);
                });
            let resource = canonical_server_resource(&server.url).unwrap_or_else(|error| {
                eprintln!("error: {error}");
                process::exit(1);
            });
            mcp_oauth::delete_token(
                &resource,
                &discovery.authorization_server_issuer,
                server.client_id.as_deref(),
            )
            .await
            .unwrap_or_else(|error| {
                eprintln!("error: {error}");
                process::exit(1);
            });
            println!(
                "Removed stored OAuth token for {} ({})",
                server.name, server.url
            );
        }
        McpCommand::Status(server_ref) => {
            // With no target/url, report every configured MCP server.
            // With a target, keep the focused per-server OAuth detail.
            if server_ref.target.is_none() && server_ref.url.is_none() {
                if let Err(error) = run_mcp_status_report(server_ref.json).await {
                    eprintln!("error: {error}");
                    process::exit(1);
                }
                return;
            }
            let server = resolve_server_reference(server_ref).unwrap_or_else(|error| {
                eprintln!("error: {error}");
                process::exit(1);
            });
            let discovery = mcp_oauth::discover(&server.url)
                .await
                .unwrap_or_else(|error| {
                    eprintln!("error: {error}");
                    process::exit(1);
                });
            let resource = canonical_server_resource(&server.url).unwrap_or_else(|error| {
                eprintln!("error: {error}");
                process::exit(1);
            });
            match mcp_oauth::load_token(
                &resource,
                &discovery.authorization_server_issuer,
                server.client_id.as_deref(),
            )
            .await
            {
                Ok(Some(token)) => {
                    if server_ref.json {
                        print_focused_status_report(&server, "connected", Some(&token), None)
                            .unwrap_or_else(|error| {
                                eprintln!("error: {error}");
                                process::exit(1);
                            });
                        return;
                    }
                    println!("Server: {}", server.name);
                    println!("URL: {}", server.url);
                    println!("Connected: yes");
                    println!("Protocol: {MCP_PROTOCOL_VERSION}");
                    println!(
                        "Expires: {}",
                        token
                            .expires_at_unix
                            .map(format_expiry)
                            .unwrap_or_else(|| "unknown".to_string())
                    );
                    println!("Client ID: {}", token.client_id);
                    println!("Token auth method: {}", token.token_endpoint_auth_method);
                    println!("Issuer: {}", token.issuer);
                    if let Some(identity) =
                        harn_vm::mcp_identity::display_identity(&server.url, &token)
                    {
                        println!("Identity: {identity}");
                    }
                }
                Ok(None) => {
                    if server_ref.json {
                        print_focused_status_report(&server, "auth_required", None, None)
                            .unwrap_or_else(|error| {
                                eprintln!("error: {error}");
                                process::exit(1);
                            });
                        return;
                    }
                    println!("Server: {}", server.name);
                    println!("URL: {}", server.url);
                    println!("Connected: no");
                }
                Err(error) => {
                    if server_ref.json {
                        print_focused_status_report(&server, "error", None, Some(error))
                            .unwrap_or_else(|encode_error| {
                                eprintln!("error: {encode_error}");
                                process::exit(1);
                            });
                        return;
                    }
                    eprintln!("error: {error}");
                    process::exit(1);
                }
            }
        }
        McpCommand::Discover(args) => {
            if let Err(error) = discover_mcp_json(args).await {
                eprintln!("error: {error}");
                process::exit(1);
            }
        }
        McpCommand::RedirectUri => {
            println!("{DEFAULT_REDIRECT_URI}");
        }
        McpCommand::Presets(args) => {
            presets::run(args);
        }
    }
}

async fn discover_mcp_json(args: &McpDiscoverArgs) -> Result<(), String> {
    let discovery = harn_vm::mcp_json_discovery::discover_mcp_json(&args.url).await?;
    let report = harn_vm::mcp_json_discovery::discovery_report(&args.url, discovery)?;
    if args.json {
        println!(
            "{}",
            json_envelope::to_string_pretty(&JsonEnvelope::ok(
                MCP_DISCOVERY_SCHEMA_VERSION,
                report
            ))
        );
        return Ok(());
    }

    match report.descriptor {
        Some(descriptor) => {
            println!("MCP discovery descriptor found");
            println!("Source: {}", report.source);
            println!("Name: {}", descriptor.name);
            println!("Endpoint: {}", descriptor.endpoint);
            println!("Description: {}", descriptor.description);
            if let Some(icon) = descriptor.icon {
                println!("Icon: {icon}");
            }
        }
        None => {
            println!("No MCP discovery descriptor found at {}", report.source);
        }
    }
    Ok(())
}

pub(crate) const MCP_DISCOVERY_SCHEMA_VERSION: u32 = 1;

/// Schema version for `harn mcp status --json`. Bump when the
/// `McpStatusReport` shape changes in a way agents must detect. Bumped to 3 in
/// harn#3350 with the `display_identity` field.
pub(crate) const MCP_STATUS_SCHEMA_VERSION: u32 = 3;

/// Aggregate readiness report for every MCP server declared in the
/// nearest `harn.toml`. Mirrors the `connect status` report shape: a
/// versioned envelope with the resolving manifest path plus one entry
/// per server, sorted by name for stable diffs.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct McpStatusReport {
    pub schema_version: u32,
    /// Absolute path of the `harn.toml` that declared these servers,
    /// or `None` when no manifest was found in the cwd ancestry.
    pub manifest: Option<String>,
    pub servers: Vec<McpServerStatus>,
}

/// One MCP server's configured shape and derived connection state.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct McpServerStatus {
    pub name: String,
    /// `"stdio"` or `"http"` — the configured transport.
    pub transport: String,
    /// Derived connection state: `connected`, `disconnected`,
    /// `auth_required`, or `error`.
    pub state: String,
    /// Remote URL for HTTP transports; empty for stdio.
    pub url: String,
    /// `true` when the server boots lazily (on first use) rather than
    /// eagerly at session start.
    pub lazy: bool,
    /// Count of tools advertised by the server, when a live handle is
    /// available in the process registry; `None` otherwise (the
    /// standalone CLI does not boot servers to probe them).
    pub tools: Option<usize>,
    /// Resource count when known; see `tools`.
    pub resources: Option<usize>,
    /// Prompt count when known; see `tools`.
    pub prompts: Option<usize>,
    /// Human-readable diagnostic when `state` is `error`, else `None`.
    pub last_error: Option<String>,
    /// Stored OAuth token expiry as Unix seconds when known.
    pub token_expires_at_unix: Option<i64>,
    /// OAuth client id bound to the stored token when known.
    pub token_client_id: Option<String>,
    /// OAuth issuer bound to the stored token when known.
    pub token_issuer: Option<String>,
    /// Human-readable "logged in as …" identity for the authorized account,
    /// when the server has a vetted identity recipe and a captured token
    /// payload (harn#3350). `None` otherwise.
    pub display_identity: Option<String>,
}

/// Build and print the all-servers MCP status report.
async fn run_mcp_status_report(json: bool) -> Result<(), String> {
    let report = mcp_status_report().await;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report)
                .map_err(|error| format!("failed to encode JSON output: {error}"))?
        );
    } else if report.servers.is_empty() {
        println!("No [[mcp]] servers declared in the nearest harn.toml.");
    } else {
        for server in &report.servers {
            let counts = match (server.tools, server.resources, server.prompts) {
                (Some(t), Some(r), Some(p)) => format!("tools={t} resources={r} prompts={p}"),
                _ => "tools=? resources=? prompts=?".to_string(),
            };
            let mut line = format!(
                "{}\t{}\t{}\t{}",
                server.name, server.transport, server.state, counts
            );
            if let Some(identity) = &server.display_identity {
                line.push_str(&format!("\tas={identity}"));
            }
            if let Some(error) = &server.last_error {
                line.push_str(&format!("\terror={error}"));
            }
            println!("{line}");
        }
    }
    Ok(())
}

/// Assemble the report by reading the nearest manifest's `[[mcp]]`
/// entries and deriving each server's state. HTTP servers that rely on
/// OAuth probe local stored-token state (no network beyond discovery,
/// which the OAuth helpers already perform); stdio servers report as
/// `connected` since they have no auth gate. Live tool/resource/prompt
/// counts are filled from the in-process registry when a handle exists
/// (e.g. when invoked inside a running session); otherwise `None`.
pub(crate) async fn mcp_status_report() -> McpStatusReport {
    let (manifest_path, servers) = match find_manifest() {
        Ok((path, manifest)) => (Some(path.display().to_string()), manifest.mcp),
        Err(_) => (None, Vec::new()),
    };

    let mut entries = Vec::with_capacity(servers.len());
    for server in servers {
        entries.push(derive_server_status(&server).await);
    }
    entries.sort_by(|left, right| left.name.cmp(&right.name));

    McpStatusReport {
        schema_version: MCP_STATUS_SCHEMA_VERSION,
        manifest: manifest_path,
        servers: entries,
    }
}

async fn derive_server_status(server: &McpServerConfig) -> McpServerStatus {
    let transport = server
        .transport
        .clone()
        .unwrap_or_else(|| "stdio".to_string());
    let registry = harn_vm::mcp_snapshot_status()
        .into_iter()
        .find(|entry| entry.name == server.name);
    let active = registry.as_ref().map(|entry| entry.active).unwrap_or(false);

    let mut last_error = None;
    let state = if transport == "http" && !server.url.is_empty() {
        // A configured static bearer token is treated as connected; an
        // OAuth server is `auth_required` until a token is stored.
        if server.auth_token.as_deref().is_some_and(|t| !t.is_empty()) {
            "connected".to_string()
        } else {
            match resolve_auth_for_server(server).await {
                Ok(AuthResolution::StaticBearer(_) | AuthResolution::OAuthStore) => {
                    "connected".to_string()
                }
                Ok(AuthResolution::None) => "auth_required".to_string(),
                Err(error) => {
                    last_error = Some(error);
                    "error".to_string()
                }
            }
        }
    } else if active {
        // Stdio (or already-booted) server with a live handle.
        "connected".to_string()
    } else {
        // Declared stdio server with no live handle in this process.
        "disconnected".to_string()
    };

    // Resolve a "logged in as …" string for connected servers that have a
    // vetted identity recipe (harn#3350). Cheap-guarded so only descriptor-
    // bearing servers (e.g. Notion) pay for a token load.
    let display_identity = if state == "connected" {
        server_display_identity(server).await
    } else {
        None
    };

    McpServerStatus {
        name: server.name.clone(),
        transport,
        state,
        url: server.url.clone(),
        lazy: server.lazy,
        // The standalone CLI does not boot servers, so counts are only
        // known when a live handle is already registered in-process.
        tools: None,
        resources: None,
        prompts: None,
        last_error,
        token_expires_at_unix: None,
        token_client_id: None,
        token_issuer: None,
        display_identity,
    }
}

/// Resolve the authenticated-identity display string for a server, if it has a
/// catalog identity recipe and a stored OAuth token carrying the captured
/// token-response payload. Returns `None` (never errors) so status reporting
/// degrades gracefully.
async fn server_display_identity(server: &McpServerConfig) -> Option<String> {
    harn_vm::mcp_identity::display_identity_from_store(&server.url, None).await
}

fn print_focused_status_report(
    server: &ResolvedMcpServer,
    state: &str,
    token: Option<&mcp_oauth::StoredMcpToken>,
    last_error: Option<String>,
) -> Result<(), String> {
    let report = McpStatusReport {
        schema_version: MCP_STATUS_SCHEMA_VERSION,
        manifest: None,
        servers: vec![McpServerStatus {
            name: server.name.clone(),
            transport: "http".to_string(),
            state: state.to_string(),
            url: server.url.clone(),
            lazy: false,
            tools: None,
            resources: None,
            prompts: None,
            last_error,
            token_expires_at_unix: token.and_then(|token| token.expires_at_unix),
            token_client_id: token.map(|token| token.client_id.clone()),
            token_issuer: token.map(|token| token.issuer.clone()),
            display_identity: token
                .and_then(|token| harn_vm::mcp_identity::display_identity(&server.url, token)),
        }],
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&report)
            .map_err(|error| format!("failed to encode JSON output: {error}"))?
    );
    Ok(())
}

pub(crate) async fn resolve_auth_for_server(
    server: &McpServerConfig,
) -> Result<AuthResolution, String> {
    // Static `[mcp.auth]`: the bearer lives in the connect secret store, not
    // an OAuth flow. Resolve it up front so it wins over discovery/refresh.
    if let Some(auth) = server.auth.as_ref() {
        if auth.mode == Some(OAuthClientAuthMode::Static) || auth.secret_id.is_some() {
            let secret_id = auth.secret_id.as_deref().ok_or_else(|| {
                format!(
                    "MCP server '{}' uses static auth but does not set auth.secret_id",
                    server.name
                )
            })?;
            let token =
                crate::commands::connect::store::load_connect_secret_text(secret_id).await?;
            return Ok(AuthResolution::StaticBearer(token));
        }
    }

    if let Some(token) = &server.auth_token {
        if !token.is_empty() {
            return Ok(AuthResolution::StaticBearer(token.clone()));
        }
    }

    let transport = server.transport.as_deref().unwrap_or("stdio");
    if transport != "http" || server.url.is_empty() {
        return Ok(AuthResolution::None);
    }

    match mcp_oauth::resolve_bearer(&server.url).await? {
        Some(_) => Ok(AuthResolution::OAuthStore),
        None => Ok(AuthResolution::None),
    }
}

/// Run the interactive `harn mcp login` flow: harn mints the authorization URL
/// (discovery + client resolution + PKCE), opens the browser, captures the
/// redirect on a local loopback listener, then exchanges and stores the token.
/// All OAuth state lives in [`harn_vm::mcp_oauth`]; this command only owns the
/// browser + loopback IO.
async fn login(options: &McpLoginArgs) -> Result<(), String> {
    if options.all || options.reauth {
        return login_bulk(options).await;
    }

    let server = resolve_server_reference(&McpServerRefArgs {
        target: options.target.clone(),
        url: options.url.clone(),
        json: false,
    })?;

    // Resolve an optional BYO client secret stored in the connect secret store
    // (`auth.client_secret_id`) before handing the flow to the engine.
    let client_secret = if let Some(secret) = options.client_secret.clone() {
        Some(secret)
    } else if let Some(secret_id) = server
        .auth
        .as_ref()
        .and_then(|auth| auth.client_secret_id.as_deref())
    {
        Some(crate::commands::connect::store::load_connect_secret_text(secret_id).await?)
    } else {
        server.client_secret.clone()
    };

    let callback_listener = bind_callback_listener(&options.redirect_uri)?;
    let pending = mcp_oauth::begin_authorization(BeginAuthorization {
        server_url: server.url.clone(),
        redirect_uri: options.redirect_uri.clone(),
        mode: server.auth.as_ref().and_then(|auth| auth.mode),
        client_id: options
            .client_id
            .clone()
            .or_else(|| server.auth.as_ref().and_then(|auth| auth.client_id.clone()))
            .or(server.client_id.clone()),
        client_secret,
        static_secret_id: server.auth.as_ref().and_then(|auth| auth.secret_id.clone()),
        scopes: options
            .scope
            .clone()
            .or_else(|| server.auth.as_ref().and_then(|auth| auth.scopes.clone()))
            .or(server.scopes.clone()),
    })
    .await?;

    println!("Server: {} ({})", server.name, server.url);
    println!("Redirect URI: {}", options.redirect_uri);
    println!("Protocol Version: {MCP_PROTOCOL_VERSION}");
    println!("Opening browser for OAuth authorization...");

    if webbrowser::open(&pending.authorize_url).is_err() {
        println!("Open this URL manually:\n{}", pending.authorize_url);
    }

    let callback =
        wait_for_oauth_response(callback_listener, &options.redirect_uri, &pending.state)?;
    mcp_oauth::complete_authorization(&pending.state, &callback.code, callback.issuer.as_deref())
        .await?;
    println!("OAuth token stored for {}.", server.name);
    Ok(())
}

/// Stagger between opening consecutive browser consents so a bulk login does
/// not trigger a popup storm (open the next tab shortly after the previous).
const BROWSER_OPEN_STAGGER_MS: u64 = 400;
/// Overall budget for collecting all callbacks in a bulk login.
const BULK_CALLBACK_TIMEOUT_SECS: u64 = 300;

/// Bulk login: authenticate every (selected) OAuth-backed MCP server in the
/// nearest `harn.toml` at once, driven by [`McpBulkAuth`]. One shared loopback
/// listener captures every redirect (demuxed by the OAuth `state`); browser
/// consents are opened serially to avoid a popup storm; per-server status
/// streams to the terminal as each flow advances.
async fn login_bulk(options: &McpLoginArgs) -> Result<(), String> {
    let mode = match (options.all, options.reauth) {
        (true, true) => BulkAuthMode::All,
        (false, true) => BulkAuthMode::Expired,
        _ => BulkAuthMode::Missing,
    };
    let servers = enumerate_oauth_servers(&options.only).await?;
    if servers.is_empty() {
        if !options.json {
            println!("No OAuth-backed [[mcp]] servers found in the nearest harn.toml.");
        }
        return Ok(());
    }
    if !options.json {
        let verb = if mode == BulkAuthMode::Expired {
            "Re-authenticating"
        } else {
            "Authenticating"
        };
        println!(
            "{verb} {} OAuth-backed MCP server(s) via {}…",
            servers.len(),
            options.redirect_uri
        );
    }

    let listener = bind_callback_listener(&options.redirect_uri)?;
    let expected_path = Url::parse(&options.redirect_uri)
        .map_err(|error| format!("Invalid redirect URI: {error}"))?
        .path()
        .to_string();

    let mut config = BulkAuthConfig::load();
    if let Some(concurrency) = options.concurrency {
        config.concurrency = concurrency.max(1);
    }
    let driver = McpBulkAuth::with_engine(RealOAuthFlowEngine, config);
    let json = options.json;
    let mut rx = driver.subscribe();
    let printer = tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(status) => print_bulk_status(&status, json),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    let outcomes = driver.prepare(servers, mode, &options.redirect_uri).await;
    let mut pending = Vec::new();
    let mut skipped = 0usize;
    let mut failed = 0usize;
    for outcome in outcomes {
        match outcome {
            PrepareOutcome::Pending(flow) => pending.push(flow),
            PrepareOutcome::Skipped { .. } => skipped += 1,
            PrepareOutcome::Failed { .. } => failed += 1,
        }
    }

    if pending.is_empty() {
        drop(driver);
        let _ = printer.await;
        if !json {
            println!(
                "\nNothing to authorize: {skipped} already satisfied, {failed} failed to prepare."
            );
        }
        return Ok(());
    }

    // Open each consent serially (stagger to avoid a popup storm). All flows
    // share the one listener; callbacks are demuxed by `state`.
    for flow in &pending {
        if webbrowser::open(&flow.authorize_url).is_err() && !json {
            println!(
                "Open this URL to authorize {}:\n  {}",
                flow.name, flow.authorize_url
            );
        }
        tokio::time::sleep(Duration::from_millis(BROWSER_OPEN_STAGGER_MS)).await;
    }

    let deadline = Instant::now() + Duration::from_secs(BULK_CALLBACK_TIMEOUT_SECS);
    let mut remaining = pending.len();
    let mut connected = 0usize;
    // `failed` already counts prepare-time failures; callback failures add on.
    while remaining > 0 {
        match accept_bulk_callback(&listener, &expected_path, deadline).await? {
            None => break,
            Some(BulkCallback::WrongPath) => continue,
            Some(BulkCallback::Denied { .. }) => {
                failed += 1;
                remaining -= 1;
            }
            Some(BulkCallback::Code {
                state,
                code,
                issuer,
            }) => {
                match driver.complete(&state, &code, issuer.as_deref()).await {
                    Ok(_) => connected += 1,
                    // The driver already streamed the Failed status with detail.
                    Err(_) => failed += 1,
                }
                remaining -= 1;
            }
        }
    }

    drop(driver);
    let _ = printer.await;
    if !json {
        println!(
            "\nBulk login complete: {connected} connected, {skipped} skipped, {failed} failed, {remaining} pending."
        );
        if remaining > 0 {
            println!(
                "Timed out after {BULK_CALLBACK_TIMEOUT_SECS}s waiting for {remaining} consent(s). Re-run with --reauth to retry."
            );
        }
    }
    Ok(())
}

/// Collect the OAuth-backed MCP servers from the nearest manifest, optionally
/// filtered to `only` names, resolving each server's BYO client secret (when it
/// declares one) the same way the single-server login does.
async fn enumerate_oauth_servers(only: &[String]) -> Result<Vec<BulkAuthServer>, String> {
    // No manifest is "no servers", not a hard error — bulk login over an empty
    // set just reports there is nothing to do (mirrors `mcp status`).
    let manifest = match find_manifest() {
        Ok((_, manifest)) => manifest,
        Err(_) => return Ok(Vec::new()),
    };
    let mut out = Vec::new();
    for server in manifest.mcp {
        if !is_oauth_server(&server) {
            continue;
        }
        if !only.is_empty() && !only.iter().any(|name| name == &server.name) {
            continue;
        }
        let client_secret = match server
            .auth
            .as_ref()
            .and_then(|auth| auth.client_secret_id.as_deref())
        {
            Some(secret_id) => {
                Some(crate::commands::connect::store::load_connect_secret_text(secret_id).await?)
            }
            None => server.client_secret.clone(),
        };
        out.push(BulkAuthServer {
            name: server.name.clone(),
            server_url: server.url.clone(),
            mode: server.auth.as_ref().and_then(|auth| auth.mode),
            client_id: server
                .auth
                .as_ref()
                .and_then(|auth| auth.client_id.clone())
                .or(server.client_id.clone()),
            client_secret,
            static_secret_id: server.auth.as_ref().and_then(|auth| auth.secret_id.clone()),
            scopes: server
                .auth
                .as_ref()
                .and_then(|auth| auth.scopes.clone())
                .or(server.scopes.clone()),
        });
    }
    Ok(out)
}

/// Whether a manifest server is authenticated by an interactive OAuth flow (so
/// bulk login should drive it): an HTTP server with a URL and no static bearer.
fn is_oauth_server(server: &McpServerConfig) -> bool {
    let transport = server.transport.as_deref().unwrap_or("stdio");
    if transport != "http" || server.url.is_empty() {
        return false;
    }
    if server.auth_token.as_deref().is_some_and(|t| !t.is_empty()) {
        return false;
    }
    if let Some(auth) = &server.auth {
        if auth.mode == Some(OAuthClientAuthMode::Static) || auth.secret_id.is_some() {
            return false;
        }
    }
    true
}

/// One parsed bulk-login redirect.
enum BulkCallback {
    Code {
        state: String,
        code: String,
        issuer: Option<String>,
    },
    Denied {
        #[allow(dead_code)]
        error: String,
    },
    WrongPath,
}

/// Accept one callback on the shared listener without blocking the runtime,
/// returning `None` once `deadline` passes. Unlike the single-login path this
/// does not validate `state` — the driver demuxes by it.
async fn accept_bulk_callback(
    listener: &TcpListener,
    expected_path: &str,
    deadline: Instant,
) -> Result<Option<BulkCallback>, String> {
    listener
        .set_nonblocking(true)
        .map_err(|error| format!("Failed to configure redirect listener: {error}"))?;
    loop {
        match listener.accept() {
            Ok((mut stream, _)) => {
                stream.set_nonblocking(false).ok();
                return Ok(Some(read_bulk_callback(&mut stream, expected_path)?));
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Ok(None);
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            Err(error) => return Err(format!("Failed to accept OAuth callback: {error}")),
        }
    }
}

fn read_bulk_callback(
    stream: &mut std::net::TcpStream,
    expected_path: &str,
) -> Result<BulkCallback, String> {
    let mut buffer = [0u8; 8192];
    let bytes_read = stream
        .read(&mut buffer)
        .map_err(|error| format!("Failed to read OAuth callback: {error}"))?;
    let request = String::from_utf8_lossy(&buffer[..bytes_read]);
    let request_line = request
        .lines()
        .next()
        .ok_or_else(|| "OAuth callback request was empty".to_string())?;
    let path_and_query = request_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| "OAuth callback request line was invalid".to_string())?;
    let callback_url = Url::parse(&format!("http://127.0.0.1{path_and_query}"))
        .map_err(|error| format!("OAuth callback URL was invalid: {error}"))?;

    if callback_url.path() != expected_path {
        let _ = stream.write_all(html_response(404, "Invalid callback path").as_bytes());
        return Ok(BulkCallback::WrongPath);
    }
    let query = parse_callback_query(&callback_url);
    if let Some(error) = query_get(&query, "error") {
        let _ = stream
            .write_all(html_response(400, &format!("Authorization failed: {error}")).as_bytes());
        return Ok(BulkCallback::Denied { error });
    }
    let Some(code) = query_get(&query, "code") else {
        let _ = stream.write_all(html_response(400, "Missing authorization code").as_bytes());
        return Ok(BulkCallback::Denied {
            error: "missing authorization code".to_string(),
        });
    };
    let Some(state) = query_get(&query, "state") else {
        let _ = stream.write_all(html_response(400, "Missing state").as_bytes());
        return Ok(BulkCallback::Denied {
            error: "missing state".to_string(),
        });
    };
    let _ = stream.write_all(
        html_response(200, "Authorization complete. You can close this window.").as_bytes(),
    );
    Ok(BulkCallback::Code {
        state,
        code,
        issuer: query_get(&query, "iss"),
    })
}

fn parse_callback_query(url: &Url) -> Vec<(String, String)> {
    url.query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect()
}

fn query_get(query: &[(String, String)], key: &str) -> Option<String> {
    query
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.clone())
}

fn print_bulk_status(status: &McpAuthStatus, json: bool) {
    if json {
        if let Ok(line) = serde_json::to_string(status) {
            println!("{line}");
        }
        return;
    }
    let (symbol, label) = match status.phase {
        McpAuthPhase::Discovering => ("→", "discovering"),
        McpAuthPhase::AwaitingConsent => ("→", "awaiting consent"),
        McpAuthPhase::Exchanging => ("→", "exchanging"),
        McpAuthPhase::Connected => ("✓", "connected"),
        McpAuthPhase::Failed => ("✗", "failed"),
        McpAuthPhase::Skipped => ("·", "skipped"),
    };
    let detail = status
        .detail
        .as_deref()
        .map(|detail| format!(" ({detail})"))
        .unwrap_or_default();
    println!("  {symbol} {}: {label}{detail}", status.server);
}

fn resolve_server_reference(server_ref: &McpServerRefArgs) -> Result<ResolvedMcpServer, String> {
    if let Some(url) = &server_ref.url {
        return Ok(ResolvedMcpServer {
            name: server_ref
                .target
                .clone()
                .unwrap_or_else(|| infer_name_from_url(url)),
            url: url.clone(),
            auth: None,
            client_id: None,
            client_secret: None,
            scopes: None,
        });
    }

    let target = server_ref
        .target
        .as_ref()
        .ok_or_else(|| "Missing MCP server name or URL".to_string())?;
    if target.starts_with("http://") || target.starts_with("https://") {
        return Ok(ResolvedMcpServer {
            name: infer_name_from_url(target),
            url: target.clone(),
            auth: None,
            client_id: None,
            client_secret: None,
            scopes: None,
        });
    }

    let (_, manifest) = find_manifest()?;
    let server = manifest
        .mcp
        .into_iter()
        .find(|entry| entry.name == *target)
        .ok_or_else(|| format!("No [[mcp]] entry named '{target}' in the nearest harn.toml"))?;
    if server.url.is_empty() {
        return Err(format!(
            "MCP server '{target}' does not define a remote URL. Use --url for ad hoc login or add url = ... to harn.toml."
        ));
    }

    Ok(ResolvedMcpServer {
        name: server.name,
        url: server.url,
        auth: server.auth,
        client_id: server.client_id,
        client_secret: server.client_secret,
        scopes: server.scopes,
    })
}

fn find_manifest() -> Result<(PathBuf, package::Manifest), String> {
    let cwd =
        env::current_dir().map_err(|error| format!("Failed to read current directory: {error}"))?;
    let Some(found) = harn_modules::manifest_walk::find_nearest_manifest(&cwd) else {
        return Err("No harn.toml found in the current directory or its parents".to_string());
    };
    let content = fs::read_to_string(&found.path)
        .map_err(|error| format!("Failed to read {}: {error}", found.path.display()))?;
    let manifest = toml::from_str::<package::Manifest>(&content)
        .map_err(|error| format!("Failed to parse {}: {error}", found.path.display()))?;
    Ok((found.path, manifest))
}

fn canonical_server_resource(server_url: &str) -> Result<String, String> {
    harn_vm::mcp_auth::canonical_resource_indicator(server_url).map_err(|error| error.to_string())
}

fn bind_callback_listener(redirect_uri: &str) -> Result<TcpListener, String> {
    let parsed =
        Url::parse(redirect_uri).map_err(|error| format!("Invalid redirect URI: {error}"))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| "Redirect URI must include a host".to_string())?;
    let port = parsed
        .port_or_known_default()
        .ok_or_else(|| "Redirect URI must include a port".to_string())?;
    let listener = TcpListener::bind((host, port))
        .map_err(|error| format!("Failed to bind redirect URI {redirect_uri}: {error}"))?;
    listener
        .set_nonblocking(false)
        .map_err(|error| format!("Failed to configure redirect listener: {error}"))?;
    Ok(listener)
}

struct OAuthCallbackResponse {
    code: String,
    issuer: Option<String>,
}

fn wait_for_oauth_response(
    listener: TcpListener,
    redirect_uri: &str,
    expected_state: &str,
) -> Result<OAuthCallbackResponse, String> {
    let expected_path = Url::parse(redirect_uri)
        .map_err(|error| format!("Invalid redirect URI: {error}"))?
        .path()
        .to_string();

    let (mut stream, _) = listener
        .accept()
        .map_err(|error| format!("Failed to accept OAuth callback: {error}"))?;
    let mut buffer = [0u8; 8192];
    let bytes_read = stream
        .read(&mut buffer)
        .map_err(|error| format!("Failed to read OAuth callback: {error}"))?;
    let request = String::from_utf8_lossy(&buffer[..bytes_read]);
    let request_line = request
        .lines()
        .next()
        .ok_or_else(|| "OAuth callback request was empty".to_string())?;
    let path_and_query = request_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| "OAuth callback request line was invalid".to_string())?;
    let callback_url = Url::parse(&format!("http://127.0.0.1{path_and_query}"))
        .map_err(|error| format!("OAuth callback URL was invalid: {error}"))?;

    let response = if callback_url.path() != expected_path {
        html_response(404, "Invalid callback path")
    } else if callback_url
        .query_pairs()
        .find(|(key, _)| key == "state")
        .map(|(_, value)| value.into_owned())
        .as_deref()
        != Some(expected_state)
    {
        html_response(400, "State mismatch")
    } else if let Some(error) = callback_url
        .query_pairs()
        .find(|(key, _)| key == "error")
        .map(|(_, value)| value.into_owned())
    {
        let _ = stream
            .write_all(html_response(400, &format!("Authorization failed: {error}")).as_bytes());
        return Err(format!("Authorization failed: {error}"));
    } else {
        html_response(200, "Authorization complete. You can close this window.")
    };
    let _ = stream.write_all(response.as_bytes());

    let code = callback_url
        .query_pairs()
        .find(|(key, _)| key == "code")
        .map(|(_, value)| value.into_owned())
        .ok_or_else(|| "OAuth callback did not include an authorization code".to_string())?;
    let issuer = callback_url
        .query_pairs()
        .find(|(key, _)| key == "iss")
        .map(|(_, value)| value.into_owned());
    Ok(OAuthCallbackResponse { code, issuer })
}

fn format_expiry(unix: i64) -> String {
    unix.to_string()
}

fn infer_name_from_url(url: &str) -> String {
    Url::parse(url)
        .ok()
        .and_then(|parsed| parsed.host_str().map(|host| host.to_string()))
        .unwrap_or_else(|| "remote".to_string())
}

fn html_response(status: u16, message: &str) -> String {
    let status_line = match status {
        200 => "HTTP/1.1 200 OK",
        400 => "HTTP/1.1 400 Bad Request",
        _ => "HTTP/1.1 404 Not Found",
    };
    let (title, accent, badge) = match status {
        200 => ("Authorization Complete", "#159f6b", "Connected"),
        400 => ("Authorization Failed", "#c76b19", "Retry Needed"),
        _ => ("Callback Error", "#b42318", "Invalid Request"),
    };
    let message = crate::format::escape_html(message);
    format!(
        r#"{status_line}
Content-Type: text/html; charset=utf-8
Connection: close

<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
<style>
:root {{ color-scheme: light dark; }}
body {{ margin: 0; font-family: ui-sans-serif, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; background: radial-gradient(circle at top, rgba(21,159,107,.12), transparent 35%), #0f1115; color: #f5f7fa; min-height: 100vh; display: grid; place-items: center; }}
.card {{ width: min(560px, calc(100vw - 32px)); background: rgba(17, 24, 39, 0.88); border: 1px solid rgba(255,255,255,0.08); border-radius: 20px; padding: 28px; box-shadow: 0 24px 80px rgba(0,0,0,0.35); }}
.badge {{ display: inline-block; padding: 6px 10px; border-radius: 999px; background: {accent}; color: white; font-size: 12px; font-weight: 700; letter-spacing: .04em; text-transform: uppercase; }}
h1 {{ margin: 16px 0 10px; font-size: 28px; line-height: 1.1; }}
p {{ margin: 0; color: #c6cfdb; font-size: 15px; line-height: 1.55; }}
.hint {{ margin-top: 18px; color: #98a4b3; font-size: 13px; }}
.dot {{ width: 14px; height: 14px; border-radius: 999px; background: {accent}; box-shadow: 0 0 0 8px rgba(255,255,255,0.06); }}
.row {{ display: flex; align-items: center; gap: 12px; margin-bottom: 10px; }}
</style>
</head>
<body>
<main class="card">
<div class="row"><div class="dot"></div><span class="badge">{badge}</span></div>
<h1>{title}</h1>
<p>{message}</p>
<p class="hint">You can close this tab and return to Harn.</p>
</main>
</body>
</html>"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use harn_vm::mcp_auth::{
        authorization_server_metadata_candidates, build_oauth_authorization_url,
        canonical_resource_indicator, protected_resource_metadata_candidates,
        OAuthAuthorizationUrlOptions,
    };

    #[test]
    fn protected_resource_candidate_prefers_path_specific_url() {
        let url = Url::parse("https://example.com/mcp/notion").unwrap();
        let candidates = protected_resource_metadata_candidates(&url);
        assert_eq!(
            candidates[0].as_str(),
            "https://example.com/.well-known/oauth-protected-resource/mcp/notion"
        );
        assert_eq!(
            candidates[1].as_str(),
            "https://example.com/.well-known/oauth-protected-resource"
        );
    }

    #[test]
    fn authorization_server_candidate_prefers_path_specific_metadata() {
        let url = Url::parse("https://auth.example.com/oauth").unwrap();
        let candidates = authorization_server_metadata_candidates(&url);
        assert_eq!(
            candidates[0].url.as_str(),
            "https://auth.example.com/.well-known/oauth-authorization-server/oauth"
        );
        assert_eq!(
            candidates[1].url.as_str(),
            "https://auth.example.com/.well-known/openid-configuration/oauth"
        );
        assert_eq!(
            candidates[2].url.as_str(),
            "https://auth.example.com/oauth/.well-known/openid-configuration"
        );
    }

    #[test]
    fn callback_html_response_escapes_message() {
        let response = html_response(400, "<script>alert('x')</script>&");
        assert!(response.contains("&lt;script&gt;alert(&#39;x&#39;)&lt;/script&gt;&amp;"));
        assert!(!response.contains("<script>"));
    }

    #[test]
    fn authorization_url_includes_canonical_resource_indicator() {
        let resource = canonical_resource_indicator("https://MCP.Example.com:443/mcp/").unwrap();
        let url = build_oauth_authorization_url(OAuthAuthorizationUrlOptions {
            authorization_endpoint: "https://auth.example.com/authorize",
            client_id: "client-123",
            redirect_uri: "http://127.0.0.1:9783/oauth/callback",
            state: "state-abc",
            code_challenge: "challenge-xyz",
            resource: &resource,
            scopes: Some("mcp.read"),
        })
        .unwrap();
        let resource_param = url
            .query_pairs()
            .find(|(key, _)| key == "resource")
            .map(|(_, value)| value.into_owned());
        assert_eq!(
            resource_param.as_deref(),
            Some("https://mcp.example.com/mcp")
        );
    }

    fn parse_server(toml_table: &str) -> McpServerConfig {
        toml::from_str::<McpServerConfig>(toml_table).expect("mcp server config")
    }

    #[test]
    fn mcp_server_config_parses_token_exchange_row() {
        let server = parse_server(
            r#"
name = "api"
transport = "http"
url = "https://mcp.example/mcp"

[token_exchange]
token_url = "https://auth.example/token"
actor_token = "agent.jwt"
actor_token_type = "jwt"
subject_token_type = "access_token"
client_id = "agent-client"
client_secret = "agent-secret"
token_endpoint_auth_method = "client_secret_basic"
scope = "repo"

[token_exchange.extra_params]
deployment = "enterprise"
"#,
        );
        let exchange = server
            .token_exchange
            .as_ref()
            .expect("token exchange row parsed");
        assert_eq!(
            exchange.token_url.as_deref(),
            Some("https://auth.example/token")
        );
        assert_eq!(exchange.actor_token.as_deref(), Some("agent.jwt"));
        assert_eq!(
            exchange.token_endpoint_auth_method.as_deref(),
            Some("client_secret_basic")
        );
        assert_eq!(
            exchange.extra_params.get("deployment"),
            Some(&serde_json::json!("enterprise"))
        );
    }

    #[test]
    fn oauth_server_classification() {
        let oauth = parse_server(
            "name = \"notion\"\ntransport = \"http\"\nurl = \"https://mcp.notion.com/mcp\"\n",
        );
        assert!(is_oauth_server(&oauth), "http server with a url is OAuth");

        let stdio = parse_server("name = \"fs\"\ntransport = \"stdio\"\ncommand = \"npx\"\n");
        assert!(!is_oauth_server(&stdio), "stdio is not OAuth");

        let static_bearer = parse_server(
            "name = \"api\"\ntransport = \"http\"\nurl = \"https://mcp.example/mcp\"\nauth_token = \"static\"\n",
        );
        assert!(
            !is_oauth_server(&static_bearer),
            "a static bearer token is not interactive OAuth"
        );

        let static_secret = parse_server(
            "name = \"api\"\ntransport = \"http\"\nurl = \"https://mcp.example/mcp\"\n[auth]\nsecret_id = \"my-secret\"\n",
        );
        assert!(
            !is_oauth_server(&static_secret),
            "an [auth].secret_id static token is not interactive OAuth"
        );

        let no_url = parse_server("name = \"x\"\ntransport = \"http\"\n");
        assert!(!is_oauth_server(&no_url), "http without a url is not OAuth");
    }

    #[test]
    fn callback_query_parsing_extracts_code_state_issuer() {
        let url = Url::parse(
            "http://127.0.0.1:9783/oauth/callback?code=abc&state=xyz&iss=https%3A%2F%2Fauth.example",
        )
        .unwrap();
        let query = parse_callback_query(&url);
        assert_eq!(query_get(&query, "code").as_deref(), Some("abc"));
        assert_eq!(query_get(&query, "state").as_deref(), Some("xyz"));
        assert_eq!(
            query_get(&query, "iss").as_deref(),
            Some("https://auth.example")
        );
        assert!(query_get(&query, "error").is_none());
    }

    #[test]
    fn bulk_status_json_is_one_line_per_event() {
        let status = McpAuthStatus {
            server: "Notion".to_string(),
            server_url: "https://mcp.notion.com/mcp".to_string(),
            phase: McpAuthPhase::Connected,
            detail: None,
        };
        let line = serde_json::to_string(&status).expect("serialize status");
        assert!(!line.contains('\n'), "one status per line");
        let value: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["phase"], serde_json::json!("connected"));
        assert_eq!(value["server"], serde_json::json!("Notion"));
    }

    #[tokio::test]
    async fn stdio_server_without_live_handle_is_disconnected() {
        let server =
            parse_server("name = \"fs\"\ntransport = \"stdio\"\ncommand = \"/bin/true\"\n");
        let status = derive_server_status(&server).await;
        assert_eq!(status.name, "fs");
        assert_eq!(status.transport, "stdio");
        assert_eq!(status.state, "disconnected");
        assert_eq!(status.url, "");
        assert!(status.tools.is_none());
        assert!(status.last_error.is_none());
    }

    #[tokio::test]
    async fn http_server_with_static_token_is_connected() {
        let server = parse_server(
            "name = \"api\"\ntransport = \"http\"\nurl = \"https://mcp.example.com/mcp\"\nauth_token = \"static-bearer\"\n",
        );
        let status = derive_server_status(&server).await;
        assert_eq!(status.transport, "http");
        assert_eq!(status.state, "connected");
        assert_eq!(status.url, "https://mcp.example.com/mcp");
    }

    #[test]
    fn status_report_serializes_with_stable_keys() {
        let report = McpStatusReport {
            schema_version: MCP_STATUS_SCHEMA_VERSION,
            manifest: Some("/repo/harn.toml".to_string()),
            servers: vec![McpServerStatus {
                name: "fs".to_string(),
                transport: "stdio".to_string(),
                state: "disconnected".to_string(),
                url: String::new(),
                lazy: true,
                tools: None,
                resources: None,
                prompts: None,
                last_error: None,
                token_expires_at_unix: None,
                token_client_id: None,
                token_issuer: None,
                display_identity: None,
            }],
        };
        let value: serde_json::Value = serde_json::to_value(&report).expect("serialize");
        assert_eq!(value["schema_version"], 3);
        assert_eq!(value["manifest"], "/repo/harn.toml");
        assert_eq!(value["servers"][0]["name"], "fs");
        assert_eq!(value["servers"][0]["transport"], "stdio");
        assert_eq!(value["servers"][0]["state"], "disconnected");
        assert_eq!(value["servers"][0]["lazy"], true);
        assert!(value["servers"][0]["tools"].is_null());
        assert!(value["servers"][0]["last_error"].is_null());
        assert!(value["servers"][0]["token_expires_at_unix"].is_null());
        assert!(value["servers"][0]["display_identity"].is_null());
    }
}
