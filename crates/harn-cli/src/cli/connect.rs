use std::path::PathBuf;

use clap::{Args, Subcommand};

#[derive(Debug, Args)]
#[command(
    after_long_help = "Registered provider commands:\n  harn connect <provider> [OPTIONS]\n\nIf <provider> is not one of the built-in subcommands, Harn reads OAuth metadata from the nearest harn.toml [[providers]] entry."
)]
pub(crate) struct ConnectArgs {
    /// Show authenticated connector tokens known to the local keyring.
    #[arg(long)]
    pub list: bool,
    /// Remove locally stored OAuth material for a provider.
    #[arg(long, value_name = "PROVIDER")]
    pub revoke: Option<String>,
    /// Force-refresh locally stored OAuth material for a provider.
    #[arg(long, value_name = "PROVIDER")]
    pub refresh: Option<String>,
    /// Run the generic OAuth 2.1 flow for a provider/resource pair.
    #[arg(long, value_names = ["PROVIDER", "URL"], num_args = 2)]
    pub generic: Vec<String>,
    /// Emit machine-readable JSON for list/connect management operations.
    #[arg(long)]
    pub json: bool,
    #[command(subcommand)]
    pub command: Option<ConnectCommand>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ConnectCommand {
    /// Report whether connector packages are installed and usable.
    Status(ConnectStatusArgs),
    /// Emit the generic host setup plan for a connector.
    SetupPlan(ConnectSetupPlanArgs),
    /// Store an API-key style connector secret.
    ApiKey(ConnectApiKeyArgs),
    /// Capture GitHub App installation metadata and optional app secrets.
    Github(ConnectGithubArgs),
    /// Authorize Linear using OAuth, or register a webhook when --url is supplied.
    Linear(ConnectLinearArgs),
    /// Authorize Slack using OAuth and store connector tokens.
    Slack(ConnectOAuthArgs),
    /// Authorize Notion using OAuth and store connector tokens.
    Notion(ConnectOAuthArgs),
    /// Run the generic OAuth 2.1 flow for any compliant provider.
    Generic(ConnectGenericArgs),
    /// Authorize a provider registered in harn.toml [[providers]] metadata.
    #[command(external_subcommand)]
    Provider(Vec<String>),
}

#[derive(Debug, Args, Clone)]
pub(crate) struct ConnectApiKeyArgs {
    /// Connector id that owns the API key.
    #[arg(long = "connector", value_name = "ID")]
    pub connector: String,
    /// Secret id to store, in namespace/name form.
    #[arg(long = "secret-id", value_name = "ID")]
    pub secret_id: String,
    /// Inline API key value. Prefer --value-file or prompt input in shared shells.
    #[arg(long, conflicts_with = "value_file")]
    pub value: Option<String>,
    /// File containing the API key value.
    #[arg(long = "value-file", conflicts_with = "value")]
    pub value_file: Option<PathBuf>,
    /// Optional scope string associated with this key.
    #[arg(long = "scopes")]
    pub scopes: Option<String>,
    /// Emit machine-readable JSON instead of a human summary.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args, Clone)]
pub(crate) struct ConnectStatusArgs {
    /// Restrict status to one connector id.
    #[arg(long = "connector", value_name = "ID")]
    pub connector: Option<String>,
    /// Execute declared health-check commands in addition to local credential checks.
    #[arg(long = "run-health-checks")]
    pub run_health_checks: bool,
    /// Emit machine-readable JSON instead of a human summary.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args, Clone)]
pub(crate) struct ConnectSetupPlanArgs {
    /// Connector id to plan setup for.
    #[arg(long = "connector", value_name = "ID")]
    pub connector: String,
    /// Emit machine-readable JSON instead of a human summary.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct ConnectGithubArgs {
    /// GitHub App slug used to build the install URL.
    #[arg(long)]
    pub app_slug: Option<String>,
    /// GitHub App id. Required when storing a private key.
    #[arg(long)]
    pub app_id: Option<String>,
    /// Existing installation id. Skips waiting for the browser callback.
    #[arg(long)]
    pub installation_id: Option<String>,
    /// Override the GitHub App installation URL.
    #[arg(long)]
    pub install_url: Option<String>,
    /// Loopback callback URL. Port 0 binds a random localhost port.
    #[arg(long, default_value = "http://127.0.0.1:0/gh-install-callback")]
    pub redirect_uri: String,
    /// PEM private-key file to store as github/app-<app_id>/private-key.
    #[arg(long)]
    pub private_key_file: Option<PathBuf>,
    /// Inline webhook signing secret to store as github/webhook-secret.
    #[arg(long, conflicts_with = "webhook_secret_file")]
    pub webhook_secret: Option<String>,
    /// Webhook signing secret file to store as github/webhook-secret.
    #[arg(long, conflicts_with = "webhook_secret")]
    pub webhook_secret_file: Option<PathBuf>,
    /// Do not open the system browser; print the URL instead.
    #[arg(long)]
    pub no_open: bool,
    /// Emit machine-readable JSON instead of a human summary.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct ConnectLinearArgs {
    /// Public HTTPS URL that Linear should deliver webhook events to.
    #[arg(long)]
    pub url: Option<String>,
    /// Optional path to an explicit `harn.toml`. Defaults to the nearest manifest from cwd.
    #[arg(long)]
    pub config: Option<String>,
    /// Linear team id for a team-scoped webhook.
    #[arg(long, conflicts_with = "all_public_teams")]
    pub team_id: Option<String>,
    /// Register the webhook for all public teams instead of one team.
    #[arg(long, conflicts_with = "team_id")]
    pub all_public_teams: bool,
    /// Optional display label for the Linear webhook.
    #[arg(long)]
    pub label: Option<String>,
    /// Override the Linear GraphQL endpoint, mainly for tests and self-hosted proxies.
    #[arg(long)]
    pub api_base_url: Option<String>,
    /// Inline Linear personal API key.
    #[arg(long, conflicts_with_all = ["api_key_secret", "access_token", "access_token_secret"])]
    pub api_key: Option<String>,
    /// Secret id containing a Linear personal API key (`namespace/name[@version]`).
    #[arg(long, conflicts_with_all = ["api_key", "access_token", "access_token_secret"])]
    pub api_key_secret: Option<String>,
    /// Inline OAuth access token.
    #[arg(long, conflicts_with_all = ["api_key", "api_key_secret", "access_token_secret"])]
    pub access_token: Option<String>,
    /// Secret id containing an OAuth access token (`namespace/name[@version]`).
    #[arg(long, conflicts_with_all = ["api_key", "api_key_secret", "access_token"])]
    pub access_token_secret: Option<String>,
    /// Explicit OAuth client ID for guided Linear authorization.
    #[arg(long = "client-id")]
    pub client_id: Option<String>,
    /// Explicit OAuth client secret for guided Linear authorization.
    #[arg(long = "client-secret")]
    pub client_secret: Option<String>,
    /// Requested OAuth scope string for guided Linear authorization.
    #[arg(long = "scope")]
    pub scope: Option<String>,
    /// Optional OAuth resource indicator.
    #[arg(long = "resource")]
    pub resource: Option<String>,
    /// Override the authorization endpoint.
    #[arg(long = "auth-url")]
    pub auth_url: Option<String>,
    /// Override the token endpoint.
    #[arg(long = "token-url")]
    pub token_url: Option<String>,
    /// Override token endpoint auth method: none, client_secret_post, or client_secret_basic.
    #[arg(long = "token-auth-method")]
    pub token_auth_method: Option<String>,
    /// Loopback callback URL. Port 0 binds a random localhost port.
    #[arg(
        long = "redirect-uri",
        default_value = "http://127.0.0.1:0/oauth/callback"
    )]
    pub redirect_uri: String,
    /// Do not open the system browser; print the URL instead.
    #[arg(long)]
    pub no_open: bool,
    /// Emit machine-readable JSON instead of a human summary.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args, Clone)]
pub(crate) struct ConnectOAuthArgs {
    /// Explicit OAuth client ID.
    #[arg(long = "client-id")]
    pub client_id: Option<String>,
    /// Explicit OAuth client secret.
    #[arg(long = "client-secret")]
    pub client_secret: Option<String>,
    /// Requested OAuth scope string.
    #[arg(long = "scope")]
    pub scope: Option<String>,
    /// Optional OAuth resource indicator.
    #[arg(long = "resource")]
    pub resource: Option<String>,
    /// Override the authorization endpoint.
    #[arg(long = "auth-url")]
    pub auth_url: Option<String>,
    /// Override the token endpoint.
    #[arg(long = "token-url")]
    pub token_url: Option<String>,
    /// Override token endpoint auth method: none, client_secret_post, or client_secret_basic.
    #[arg(long = "token-auth-method")]
    pub token_auth_method: Option<String>,
    /// Loopback callback URL. Port 0 binds a random localhost port.
    #[arg(
        long = "redirect-uri",
        default_value = "http://127.0.0.1:0/oauth/callback"
    )]
    pub redirect_uri: String,
    /// Do not open the system browser; print the URL instead.
    #[arg(long)]
    pub no_open: bool,
    /// Emit machine-readable JSON instead of a human summary.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args, Clone)]
pub(crate) struct ConnectGenericArgs {
    /// Provider name used for local secret ids.
    pub provider: String,
    /// Protected resource URL. Used for OAuth discovery and resource indicators.
    pub url: String,
    #[command(flatten)]
    pub oauth: ConnectOAuthArgs,
}
