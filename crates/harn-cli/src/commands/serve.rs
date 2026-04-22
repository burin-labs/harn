use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::sync::Arc;

use harn_serve::{
    A2aHttpServeOptions, A2aServer, A2aServerConfig, ApiKeyAuthConfig, AuthMethodConfig,
    AuthPolicy, DispatchCore, DispatchCoreConfig, HmacAuthConfig, McpHttpServeOptions, McpServer,
    McpServerConfig,
};
use time::Duration;

use crate::cli::{A2aServeArgs, McpServeTransport, ServeMcpArgs};

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
