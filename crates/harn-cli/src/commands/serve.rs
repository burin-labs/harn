use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use harn_serve::{
    A2aHttpServeOptions, A2aServer, A2aServerConfig, ApiKeyAuthConfig, AuthMethodConfig,
    AuthPolicy, DispatchCore, DispatchCoreConfig, ExportCatalog, ExportedCallableKind,
    HmacAuthConfig, McpHttpServeOptions, McpServer, McpServerConfig,
};
use time::Duration;

use crate::cli::{A2aServeArgs, McpServeTransport, ServeAcpArgs, ServeMcpArgs};

pub(crate) async fn run_acp_server(args: &ServeAcpArgs) -> Result<(), String> {
    crate::acp::run_acp_server(Some(&args.file)).await;
    Ok(())
}

pub(crate) async fn run_a2a_server(args: &A2aServeArgs) -> Result<(), String> {
    let mut config = DispatchCoreConfig::for_script(&args.file);
    config.auth_policy = build_auth_policy(&args.api_key, args.hmac_secret.as_ref());
    let core = DispatchCore::new(config).map_err(|error| error.to_string())?;
    let mut server_config = A2aServerConfig::new(core);
    server_config.card_signing_secret = args.card_signing_secret.clone();
    let server = Arc::new(A2aServer::new(server_config));
    server
        .run_http(A2aHttpServeOptions {
            bind: SocketAddr::from(([0, 0, 0, 0], args.port)),
            public_url: args.public_url.clone(),
        })
        .await
}

pub(crate) async fn run_mcp_server(args: &ServeMcpArgs) -> Result<(), String> {
    if args.transport == McpServeTransport::Stdio
        && (!args.api_key.is_empty() || args.hmac_secret.is_some())
    {
        return Err("HTTP auth flags require `harn serve mcp --transport http`".to_string());
    }

    // Scripts that author the MCP surface explicitly through
    // `mcp_tools(registry)` / `mcp_resource(...)` / `mcp_prompt(...)`
    // typically don't expose any `pub fn` entrypoints. Dispatch those to
    // the legacy script-driven runner that runs the script once,
    // collects the registered tools/resources/prompts, and serves them
    // over stdio. The DispatchCore-based adapter only knows how to
    // route incoming MCP calls to `pub fn` exports.
    let catalog = ExportCatalog::from_path(Path::new(&args.file))
        .map_err(|error| format!("failed to load script: {error}"))?;
    let has_pub_fn_exports = catalog
        .functions
        .values()
        .any(|function| function.kind == ExportedCallableKind::Function);

    if !has_pub_fn_exports {
        if args.transport != McpServeTransport::Stdio {
            return Err(
                "scripts using `mcp_tools(...)` are only served over stdio; \
                 either expose `pub fn` entrypoints or omit `--transport http`"
                    .to_string(),
            );
        }
        crate::commands::run::run_file_mcp_serve(&args.file, args.card.as_deref()).await;
        return Ok(());
    }

    if args.card.is_some() {
        return Err(
            "`--card` is only honored for legacy `mcp_tools(...)` scripts; \
             attach card metadata directly to your `pub fn` exports instead"
                .to_string(),
        );
    }

    let mut config = DispatchCoreConfig::for_script(&args.file);
    config.auth_policy = build_auth_policy(&args.api_key, args.hmac_secret.as_ref());
    let core = DispatchCore::new(config).map_err(|error| error.to_string())?;
    let server = Arc::new(McpServer::new(McpServerConfig::new(core)));

    match args.transport {
        McpServeTransport::Stdio => server.run_stdio().await,
        McpServeTransport::Http => {
            server
                .run_http(McpHttpServeOptions {
                    bind: args.bind,
                    path: args.path.clone(),
                    sse_path: args.sse_path.clone(),
                    messages_path: args.messages_path.clone(),
                })
                .await
        }
    }
}

fn build_auth_policy(api_keys: &[String], hmac_secret: Option<&String>) -> AuthPolicy {
    let mut methods = Vec::new();
    if !api_keys.is_empty() {
        methods.push(AuthMethodConfig::ApiKey(ApiKeyAuthConfig {
            keys: api_keys.iter().cloned().collect::<BTreeSet<_>>(),
        }));
    }
    if let Some(secret) = hmac_secret {
        methods.push(AuthMethodConfig::Hmac(HmacAuthConfig {
            shared_secret: secret.clone(),
            provider: "harn-serve".to_string(),
            timestamp_window: Duration::seconds(300),
        }));
    }
    AuthPolicy { methods }
}
