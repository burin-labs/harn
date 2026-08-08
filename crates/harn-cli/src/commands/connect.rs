use std::time::Duration;

use clap::Parser as ClapParser;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::cli::{ConnectArgs, ConnectCommand, ConnectGenericArgs, ConnectOAuthArgs};
use crate::package::{self, ConnectorRecoveryCopy};

mod callback;
mod github;
mod linear;
mod oauth;
mod status;
pub(crate) mod store;
mod workspace;

use self::github::run_connect_github;
use self::linear::run_connect_linear;
use self::oauth::{
    run_connect_generic, run_connect_linear_oauth, run_connect_named_oauth, run_connect_refresh,
    run_connect_registered_provider,
};
use self::status::{run_connect_setup_plan, run_connect_status};
use self::store::{run_connect_api_key, run_connect_list, run_connect_revoke};

#[cfg(test)]
use self::{callback::*, github::*, linear::*, oauth::*, status::*};

const DEFAULT_LINEAR_API_BASE_URL: &str = "https://api.linear.app/graphql";
const OAUTH_CALLBACK_TIMEOUT: Duration = Duration::from_mins(5);
const CONNECT_INDEX_NAMESPACE: &str = "connect";
const CONNECT_INDEX_NAME: &str = "index";

#[derive(Clone, Debug)]
struct OAuthProviderDefaults {
    authorization_endpoint: &'static str,
    token_endpoint: &'static str,
    token_auth_method: &'static str,
    default_resource: &'static str,
    default_scope: Option<&'static str>,
}

#[derive(Clone, Debug)]
struct OAuthConnectRequest {
    provider: String,
    resource: String,
    authorization_endpoint: Option<String>,
    token_endpoint: Option<String>,
    registration_endpoint: Option<String>,
    client_id: Option<String>,
    client_secret: Option<String>,
    scopes: Option<String>,
    redirect_uri: String,
    token_auth_method: Option<String>,
    no_open: bool,
    json: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredConnectorToken {
    provider: String,
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_at_unix: Option<i64>,
    token_endpoint: String,
    client_id: String,
    #[serde(default)]
    client_secret: Option<String>,
    token_endpoint_auth_method: String,
    #[serde(default)]
    issuer: Option<String>,
    resource: String,
    #[serde(default)]
    scopes: Option<String>,
    connected_at_unix: i64,
    #[serde(default)]
    last_used_at_unix: Option<i64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct ConnectIndex {
    #[serde(default)]
    providers: Vec<ConnectIndexEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ConnectIndexEntry {
    provider: String,
    kind: String,
    secret_id: String,
    #[serde(default)]
    secret_ids: Vec<String>,
    #[serde(default)]
    expires_at_unix: Option<i64>,
    #[serde(default)]
    scopes: Option<String>,
    connected_at_unix: i64,
    #[serde(default)]
    last_used_at_unix: Option<i64>,
}

type OAuthServerMetadata = harn_vm::mcp_auth::OAuthAuthorizationServerMetadata;
type DynamicClientRegistrationResponse = harn_vm::mcp_auth::OAuthDynamicClientRegistrationResponse;

#[derive(Clone, Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    ok: Option<bool>,
    #[serde(default)]
    error: Option<String>,
    #[serde(flatten)]
    _extra: serde_json::Map<String, JsonValue>,
}

#[derive(Clone, Debug, Serialize)]
struct ConnectStatusReport {
    schema_version: u32,
    manifest: Option<String>,
    connectors: Vec<ConnectorStatus>,
}

#[derive(Clone, Debug, Serialize)]
struct ConnectorStatus {
    id: String,
    installed: bool,
    usable: bool,
    status: String,
    reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    auth_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    flow: Option<String>,
    required_scopes: Vec<String>,
    missing_scopes: Vec<String>,
    required_secrets: Vec<String>,
    missing_secrets: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    secret_id: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    secret_ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    expires_at_unix: Option<i64>,
    health_checks: Vec<ConnectorHealthStatus>,
    recovery: ConnectorRecoveryCopy,
}

#[derive(Clone, Debug, Serialize)]
struct ConnectorHealthStatus {
    id: String,
    kind: String,
    status: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    detail: String,
}

#[derive(Clone, Debug, Serialize)]
struct ConnectSetupPlan {
    schema_version: u32,
    connector: String,
    installed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    manifest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    auth_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    flow: Option<String>,
    required_scopes: Vec<String>,
    required_secrets: Vec<String>,
    setup_command: Vec<String>,
    validation_command: Vec<String>,
    health_checks: Vec<package::ConnectorHealthCheckManifest>,
    recovery: ConnectorRecoveryCopy,
    steps: Vec<ConnectSetupStep>,
}

#[derive(Clone, Debug, Serialize)]
struct ConnectSetupStep {
    id: String,
    title: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    command: Vec<String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    body: String,
}

pub(crate) async fn run_connect(args: ConnectArgs) {
    if let Err(error) = run_connect_inner(args).await {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

async fn run_connect_inner(args: ConnectArgs) -> Result<(), String> {
    let actions = args.list as u8
        + args.revoke.is_some() as u8
        + args.refresh.is_some() as u8
        + (!args.generic.is_empty()) as u8
        + args.command.is_some() as u8;
    if actions != 1 {
        return Err(
            "choose exactly one connect action: status, setup-plan, api-key, a provider subcommand, --generic, --list, --revoke, or --refresh"
                .to_string(),
        );
    }

    if args.list {
        return run_connect_list(args.json).await;
    }
    if let Some(provider) = args.revoke {
        return run_connect_revoke(&provider, args.json).await;
    }
    if let Some(provider) = args.refresh {
        return run_connect_refresh(&provider, args.json).await;
    }
    if !args.generic.is_empty() {
        if args.generic.len() != 2 {
            return Err("--generic expects <provider> <url>".to_string());
        }
        return run_connect_generic(&ConnectGenericArgs {
            provider: args.generic[0].clone(),
            url: args.generic[1].clone(),
            oauth: ConnectOAuthArgs {
                client_id: None,
                client_secret: None,
                scope: None,
                resource: None,
                auth_url: None,
                token_url: None,
                token_auth_method: None,
                redirect_uri: "http://127.0.0.1:0/oauth/callback".to_string(),
                no_open: false,
                json: args.json,
            },
        })
        .await;
    }

    let json_output = args.json;
    match args.command.expect("validated one command action") {
        ConnectCommand::Status(mut status) => {
            status.json |= json_output;
            run_connect_status(&status).await
        }
        ConnectCommand::SetupPlan(mut setup_plan) => {
            setup_plan.json |= json_output;
            run_connect_setup_plan(&setup_plan)
        }
        ConnectCommand::ApiKey(mut api_key) => {
            api_key.json |= json_output;
            run_connect_api_key(&api_key).await
        }
        ConnectCommand::Github(args) => run_connect_github(&args).await,
        ConnectCommand::Linear(args) if args.url.is_some() => run_connect_linear(&args).await,
        ConnectCommand::Linear(args) => run_connect_linear_oauth(&args).await,
        ConnectCommand::Slack(args) => run_connect_named_oauth("slack", &args).await,
        ConnectCommand::Notion(args) => run_connect_named_oauth("notion", &args).await,
        ConnectCommand::Generic(args) => run_connect_generic(&args).await,
        ConnectCommand::Provider(raw) => {
            let parsed = parse_external_provider_connect(raw, json_output)?;
            run_connect_registered_provider(&parsed.provider, &parsed.oauth).await
        }
    }
}

#[derive(Debug, ClapParser)]
#[command(no_binary_name = true, disable_help_flag = true)]
struct ExternalProviderConnectArgs {
    provider: String,
    #[command(flatten)]
    oauth: ConnectOAuthArgs,
}

fn parse_external_provider_connect(
    raw: Vec<String>,
    json_output: bool,
) -> Result<ExternalProviderConnectArgs, String> {
    let mut parsed = ExternalProviderConnectArgs::try_parse_from(raw)
        .map_err(|error| error.render().to_string())?;
    parsed.oauth.json |= json_output;
    Ok(parsed)
}

#[cfg(test)]
mod tests;
