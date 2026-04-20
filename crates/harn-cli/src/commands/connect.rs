use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::{json, Value as JsonValue};

use crate::cli::{ConnectCommand, ConnectLinearArgs};
use crate::package;
use harn_vm::secrets::SecretProvider;

const MANIFEST: &str = "harn.toml";
const DEFAULT_LINEAR_API_BASE_URL: &str = "https://api.linear.app/graphql";

pub(crate) async fn run_connect(args: ConnectCommand) {
    match args {
        ConnectCommand::Linear(args) => {
            if let Err(error) = run_connect_linear(&args).await {
                eprintln!("error: {error}");
                std::process::exit(1);
            }
        }
    }
}

async fn run_connect_linear(args: &ConnectLinearArgs) -> Result<(), String> {
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
            "url": args.url,
            "teamId": team_id,
            "label": label,
            "resourceTypes": resource_types,
        })
    } else {
        json!({
            "url": args.url,
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
        "url": args.url,
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
            args.url
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

fn derive_linear_resource_types(
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

fn linear_resource_type_for_event(event: &str) -> Option<&'static str> {
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

async fn resolve_linear_auth(
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

fn resolve_manifest_path(explicit: Option<&str>) -> Result<(PathBuf, PathBuf), String> {
    if let Some(path) = explicit {
        let path = PathBuf::from(path);
        if !path.is_file() {
            return Err(format!("manifest not found: {}", path.display()));
        }
        let dir = path
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| format!("manifest has no parent directory: {}", path.display()))?;
        return Ok((path, dir));
    }

    find_nearest_manifest(&std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
        .ok_or_else(|| {
            "could not find a harn.toml in the current directory or its parents".to_string()
        })
}

fn find_nearest_manifest(start: &Path) -> Option<(PathBuf, PathBuf)> {
    const MAX_PARENT_DIRS: usize = 16;
    let mut cursor = Some(start.to_path_buf());
    let mut steps = 0usize;
    while let Some(dir) = cursor {
        if steps >= MAX_PARENT_DIRS {
            break;
        }
        steps += 1;
        let base = if dir.is_dir() {
            dir
        } else {
            dir.parent()?.to_path_buf()
        };
        let candidate = base.join(MANIFEST);
        if candidate.is_file() {
            return Some((candidate, base));
        }
        if base.join(".git").exists() {
            break;
        }
        cursor = base.parent().map(Path::to_path_buf);
    }
    None
}

fn parse_secret_id(raw: &str) -> Option<harn_vm::secrets::SecretId> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let (base, version) = match trimmed.rsplit_once('@') {
        Some((base, version_text)) => {
            let version = version_text.parse::<u64>().ok()?;
            (base, harn_vm::secrets::SecretVersion::Exact(version))
        }
        None => (trimmed, harn_vm::secrets::SecretVersion::Latest),
    };
    let (namespace, name) = base.split_once('/')?;
    if namespace.is_empty() || name.is_empty() {
        return None;
    }
    Some(harn_vm::secrets::SecretId::new(namespace, name).with_version(version))
}

fn secret_namespace_for(manifest_dir: &Path) -> String {
    match std::env::var("HARN_SECRET_NAMESPACE") {
        Ok(namespace) if !namespace.trim().is_empty() => namespace,
        _ => {
            let leaf = manifest_dir
                .file_name()
                .and_then(|name| name.to_str())
                .filter(|name| !name.is_empty())
                .unwrap_or("workspace");
            format!("harn/{leaf}")
        }
    }
}

fn format_linear_graphql_error(status: u16, payload: &JsonValue) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_linear_resource_types_from_trigger_events() {
        let manifest = package::ResolvedTriggerConfig {
            id: "linear-issues".to_string(),
            kind: package::TriggerKind::Webhook,
            provider: harn_vm::ProviderId::from("linear"),
            autonomy_tier: harn_vm::AutonomyTier::Shadow,
            match_: package::TriggerMatchExpr {
                events: vec!["issue.update".to_string(), "comment.create".to_string()],
                extra: Default::default(),
            },
            when: None,
            when_budget: None,
            handler: "handlers::on_linear".to_string(),
            dedupe_key: None,
            retry: package::TriggerRetrySpec::default(),
            dispatch_priority: package::TriggerDispatchPriority::Normal,
            budget: package::TriggerBudgetSpec::default(),
            concurrency: None,
            throttle: None,
            rate_limit: None,
            debounce: None,
            singleton: None,
            batch: None,
            priority_flow: None,
            secrets: Default::default(),
            filter: None,
            kind_specific: Default::default(),
            manifest_dir: PathBuf::from("/tmp"),
            manifest_path: PathBuf::from("/tmp/harn.toml"),
            package_name: None,
            exports: Default::default(),
            table_index: 0,
        };
        let resource_types = derive_linear_resource_types(&[manifest]).expect("resource types");
        assert_eq!(
            resource_types,
            vec!["Comment".to_string(), "Issue".to_string()]
        );
    }

    #[test]
    fn linear_resource_type_mapping_covers_customer_request() {
        assert_eq!(
            linear_resource_type_for_event("customer_request.update"),
            Some("CustomerRequest")
        );
    }
}
