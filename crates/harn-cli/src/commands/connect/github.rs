use serde_json::json;
use url::Url;

use crate::cli::ConnectGithubArgs;
use harn_vm::secrets::{SecretBytes, SecretId, SecretProvider};

use super::callback::{bind_loopback_listener, wait_for_github_installation};
use super::oauth::random_hex;
use super::store::{connect_secret_provider, current_unix_timestamp, upsert_index_entry};
use super::ConnectIndexEntry;

pub(super) async fn run_connect_github(args: &ConnectGithubArgs) -> Result<(), String> {
    let provider = connect_secret_provider()?;
    let state = random_hex(16);
    let installation_id = match args.installation_id.clone() {
        Some(id) => id,
        None => {
            let install_url = github_install_url(args, &state)?;
            let (listener, redirect_uri) = bind_loopback_listener(&args.redirect_uri)?;
            println!("Opening browser for GitHub App installation...");
            println!("Callback listener: {redirect_uri}");
            if args.no_open || webbrowser::open(install_url.as_str()).is_err() {
                println!("Open this URL manually:\n{install_url}");
            }
            wait_for_github_installation(listener, &redirect_uri, Some(&state))?
        }
    };

    let mut stored = Vec::new();
    let metadata = json!({
        "provider": "github",
        "app_slug": args.app_slug,
        "app_id": args.app_id,
        "installation_id": installation_id,
        "connected_at_unix": current_unix_timestamp(),
    });
    let metadata_id = SecretId::new("github", format!("installation-{installation_id}"));
    provider
        .put(
            &metadata_id,
            SecretBytes::from(
                serde_json::to_vec(&metadata)
                    .map_err(|error| format!("failed to encode GitHub metadata: {error}"))?,
            ),
        )
        .await
        .map_err(|error| format!("failed to store {metadata_id}: {error}"))?;
    stored.push(metadata_id.to_string());

    if let Some(private_key_file) = args.private_key_file.as_ref() {
        let app_id = args
            .app_id
            .as_ref()
            .ok_or_else(|| "--app-id is required with --private-key-file".to_string())?;
        let private_key = std::fs::read(private_key_file).map_err(|error| {
            format!(
                "failed to read private key file {}: {error}",
                private_key_file.display()
            )
        })?;
        let key_id = SecretId::new("github", format!("app-{app_id}/private-key"));
        provider
            .put(&key_id, SecretBytes::from(private_key))
            .await
            .map_err(|error| format!("failed to store {key_id}: {error}"))?;
        stored.push(key_id.to_string());
    }

    if args.webhook_secret.is_some() || args.webhook_secret_file.is_some() {
        let secret = match (
            args.webhook_secret.as_ref(),
            args.webhook_secret_file.as_ref(),
        ) {
            (Some(value), None) => value.as_bytes().to_vec(),
            (None, Some(path)) => std::fs::read(path).map_err(|error| {
                format!(
                    "failed to read webhook secret file {}: {error}",
                    path.display()
                )
            })?,
            _ => unreachable!("clap enforces webhook secret conflicts"),
        };
        let secret_id = SecretId::new("github", "webhook-secret");
        provider
            .put(&secret_id, SecretBytes::from(secret))
            .await
            .map_err(|error| format!("failed to store {secret_id}: {error}"))?;
        stored.push(secret_id.to_string());
    }

    upsert_index_entry(
        &provider,
        ConnectIndexEntry {
            provider: "github".to_string(),
            kind: "github-app".to_string(),
            secret_id: format!("github/installation-{installation_id}"),
            secret_ids: vec![format!("github/installation-{installation_id}")],
            expires_at_unix: None,
            scopes: None,
            connected_at_unix: current_unix_timestamp(),
            last_used_at_unix: None,
        },
    )
    .await?;

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "provider": "github",
                "installation_id": installation_id,
                "stored": stored,
            }))
            .map_err(|error| format!("failed to encode JSON output: {error}"))?
        );
    } else {
        println!("Connected GitHub App installation {installation_id}.");
        println!("Stored: {}", stored.join(", "));
    }

    Ok(())
}

pub(super) fn github_install_url(args: &ConnectGithubArgs, state: &str) -> Result<Url, String> {
    let raw = if let Some(url) = args.install_url.as_ref() {
        url.clone()
    } else {
        let slug = args
            .app_slug
            .as_ref()
            .ok_or_else(|| "provide --app-slug, --install-url, or --installation-id".to_string())?;
        format!("https://github.com/apps/{slug}/installations/new")
    };
    let mut url =
        Url::parse(&raw).map_err(|error| format!("invalid GitHub install URL: {error}"))?;
    url.query_pairs_mut().append_pair("state", state);
    Ok(url)
}
