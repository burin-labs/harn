use std::collections::BTreeSet;
use std::path::Path;

use serde_json::{json, Value as JsonValue};

use crate::cli::ConnectLinearArgs;
use crate::package;
use harn_vm::secrets::SecretProvider;

use super::store::parse_secret_id;
use super::workspace::{resolve_manifest_path, secret_namespace_for};
use super::DEFAULT_LINEAR_API_BASE_URL;

pub(super) async fn run_connect_linear(args: &ConnectLinearArgs) -> Result<(), String> {
    let url = args.url.as_deref().ok_or_else(|| {
        "`harn connect linear` webhook registration requires --url; omit --url for OAuth setup"
            .to_string()
    })?;
    if !args.all_public_teams && args.team_id.is_none() {
        return Err(
            "`harn connect linear` requires either --team-id or --all-public-teams".to_string(),
        );
    }

    let (manifest_path, manifest_dir) = resolve_manifest_path(args.config.as_deref())?;
    let extensions = package::load_runtime_extensions(&manifest_dir);
    let triggers: Vec<_> = extensions
        .triggers
        .into_iter()
        .filter(|trigger| trigger.provider.as_str() == "linear")
        .collect();
    if triggers.is_empty() {
        return Err(format!(
            "no Linear triggers found in {}",
            manifest_path.display()
        ));
    }

    let resource_types = derive_linear_resource_types(&triggers)?;
    let token = resolve_linear_auth(args, &manifest_dir).await?;
    let label = args.label.clone().unwrap_or_else(|| {
        let package = extensions
            .root_manifest
            .as_ref()
            .and_then(|manifest| manifest.package.as_ref())
            .and_then(|package| package.name.clone())
            .unwrap_or_else(|| {
                manifest_dir
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("workspace")
                    .to_string()
            });
        format!("Harn ({package})")
    });

    let input = if let Some(team_id) = args.team_id.as_ref() {
        json!({
            "url": url,
            "teamId": team_id,
            "label": label,
            "resourceTypes": resource_types,
        })
    } else {
        json!({
            "url": url,
            "allPublicTeams": true,
            "label": label,
            "resourceTypes": resource_types,
        })
    };

    let response = reqwest::Client::new()
        .post(
            args.api_base_url
                .clone()
                .unwrap_or_else(|| DEFAULT_LINEAR_API_BASE_URL.to_string()),
        )
        .header("Content-Type", "application/json")
        .header("Authorization", token)
        .json(&json!({
            "query": "mutation RegisterWebhook($input: WebhookCreateInput!) { webhookCreate(input: $input) { success webhook { id enabled url } } }",
            "variables": { "input": input },
            "operationName": "RegisterWebhook",
        }))
        .send()
        .await
        .map_err(|error| format!("failed to call Linear GraphQL API: {error}"))?;

    let status = response.status();
    let payload = response
        .json::<JsonValue>()
        .await
        .map_err(|error| format!("failed to decode Linear GraphQL response: {error}"))?;
    if !status.is_success() {
        return Err(format_linear_graphql_error(status.as_u16(), &payload));
    }
    if payload.get("errors").is_some() {
        return Err(format_linear_graphql_error(status.as_u16(), &payload));
    }
    let result = payload
        .get("data")
        .and_then(|value| value.get("webhookCreate"))
        .ok_or_else(|| "Linear GraphQL response missing data.webhookCreate".to_string())?;
    if !result
        .get("success")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false)
    {
        return Err("Linear webhookCreate returned success = false".to_string());
    }

    let rendered = json!({
        "manifest": manifest_path.display().to_string(),
        "url": url,
        "resource_types": resource_types,
        "webhook": result.get("webhook").cloned().unwrap_or(JsonValue::Null),
    });
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&rendered)
                .map_err(|error| format!("failed to encode JSON output: {error}"))?
        );
    } else {
        let webhook = rendered.get("webhook").unwrap_or(&JsonValue::Null);
        println!(
            "Registered Linear webhook {} for {}",
            webhook
                .get("id")
                .and_then(JsonValue::as_str)
                .unwrap_or("<unknown>"),
            url
        );
        println!(
            "Enabled: {}",
            webhook
                .get("enabled")
                .and_then(JsonValue::as_bool)
                .unwrap_or(false)
        );
        println!(
            "Resource types: {}",
            rendered["resource_types"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(JsonValue::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        );
        println!("Manifest: {}", manifest_path.display());
    }

    Ok(())
}

pub(super) fn derive_linear_resource_types(
    triggers: &[package::ResolvedTriggerConfig],
) -> Result<Vec<String>, String> {
    let mut resource_types = BTreeSet::new();
    for trigger in triggers {
        for event in &trigger.match_.events {
            let resource = linear_resource_type_for_event(event).ok_or_else(|| {
                format!(
                    "trigger '{}' uses unsupported Linear event '{}'",
                    trigger.id, event
                )
            })?;
            resource_types.insert(resource.to_string());
        }
    }
    if resource_types.is_empty() {
        return Err(
            "no Linear resource types could be derived from trigger match.events".to_string(),
        );
    }
    Ok(resource_types.into_iter().collect())
}

pub(super) fn linear_resource_type_for_event(event: &str) -> Option<&'static str> {
    let prefix = event
        .split('.')
        .next()
        .unwrap_or(event)
        .trim()
        .to_ascii_lowercase();
    match prefix.as_str() {
        "issue" => Some("Issue"),
        "comment" | "issue_comment" | "issuecomment" => Some("Comment"),
        "issue_label" | "issuelabel" => Some("IssueLabel"),
        "project" => Some("Project"),
        "cycle" => Some("Cycle"),
        "customer" => Some("Customer"),
        "customer_request" | "customerrequest" => Some("CustomerRequest"),
        _ => None,
    }
}

pub(super) async fn resolve_linear_auth(
    args: &ConnectLinearArgs,
    manifest_dir: &Path,
) -> Result<String, String> {
    if let Some(token) = args.access_token.as_ref() {
        return Ok(format!("Bearer {token}"));
    }
    if let Some(api_key) = args.api_key.as_ref() {
        return Ok(api_key.clone());
    }

    let secret_id = args
        .access_token_secret
        .as_deref()
        .or(args.api_key_secret.as_deref())
        .ok_or_else(|| {
            "provide --access-token, --access-token-secret, --api-key, or --api-key-secret"
                .to_string()
        })
        .and_then(|raw| parse_secret_id(raw).ok_or_else(|| format!("invalid secret id `{raw}`")))?;
    let provider = harn_vm::secrets::configured_default_chain(secret_namespace_for(manifest_dir))
        .map_err(|error| format!("failed to configure secret providers: {error}"))?;
    let secret = provider
        .get(&secret_id)
        .await
        .map_err(|error| format!("failed to load secret `{secret_id}`: {error}"))?;
    let value = secret.with_exposed(|bytes| String::from_utf8_lossy(bytes).to_string());
    if args.access_token_secret.is_some() {
        Ok(format!("Bearer {value}"))
    } else {
        Ok(value)
    }
}

pub(super) fn format_linear_graphql_error(status: u16, payload: &JsonValue) -> String {
    let messages = payload
        .get("errors")
        .and_then(JsonValue::as_array)
        .map(|errors| {
            errors
                .iter()
                .filter_map(|error| error.get("message").and_then(JsonValue::as_str))
                .collect::<Vec<_>>()
                .join("; ")
        })
        .unwrap_or_default();
    if messages.is_empty() {
        format!("Linear GraphQL request failed with status {status}")
    } else {
        format!("Linear GraphQL request failed with status {status}: {messages}")
    }
}
