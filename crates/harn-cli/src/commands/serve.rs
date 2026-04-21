use std::collections::BTreeSet;
use std::sync::Arc;

use harn_serve::{
    ApiKeyAuthConfig, AuthMethodConfig, AuthPolicy, DispatchCore, DispatchCoreConfig,
    HmacAuthConfig, McpHttpServeOptions, McpServer, McpServerConfig,
};
use time::Duration;

use crate::cli::{McpServeTransport, ServeMcpArgs};

pub(crate) async fn run_mcp_server(args: &ServeMcpArgs) -> Result<(), String> {
    if args.transport == McpServeTransport::Stdio
        && (!args.api_key.is_empty() || args.hmac_secret.is_some())
    {
        return Err("HTTP auth flags require `harn serve mcp --transport http`".to_string());
    }

    let mut config = DispatchCoreConfig::for_script(&args.file);
    config.auth_policy = build_auth_policy(args);
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

fn build_auth_policy(args: &ServeMcpArgs) -> AuthPolicy {
    let mut methods = Vec::new();
    if !args.api_key.is_empty() {
        methods.push(AuthMethodConfig::ApiKey(ApiKeyAuthConfig {
            keys: args.api_key.iter().cloned().collect::<BTreeSet<_>>(),
        }));
    }
    if let Some(secret) = args.hmac_secret.as_ref() {
        methods.push(AuthMethodConfig::Hmac(HmacAuthConfig {
            shared_secret: secret.clone(),
            provider: "harn-serve".to_string(),
            timestamp_window: Duration::seconds(300),
        }));
    }
    AuthPolicy { methods }
}
