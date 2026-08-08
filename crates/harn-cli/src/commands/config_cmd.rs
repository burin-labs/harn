use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::{
    cli::{ConfigArgs, ConfigCommand, ConfigInspectArgs, ConfigValidateArgs},
    net,
};

const PROJECT_CONFIG: &str = "harn.config.toml";
const MANIFEST: &str = harn_modules::manifest_walk::MANIFEST_FILENAME;

pub(crate) async fn run(args: ConfigArgs) -> Result<(), String> {
    match args.command {
        ConfigCommand::Inspect(args) => run_inspect(args).await,
        ConfigCommand::Validate(args) => run_validate(args),
        ConfigCommand::Schema(args) => {
            let text = serde_json::to_string_pretty(&harn_vm::config::schema_json())
                .map_err(|error| error.to_string())?;
            if let Some(path) = args.output {
                harn_vm::atomic_io::atomic_write(&path, text.as_bytes())
                    .map_err(|error| format!("failed to write {}: {error}", path.display()))?;
            } else {
                println!("{text}");
            }
            Ok(())
        }
    }
}

async fn run_inspect(args: ConfigInspectArgs) -> Result<(), String> {
    let layers = build_layers(&args).await?;
    let resolved = harn_vm::config::merge_layers(layers).map_err(|error| error.to_string())?;
    let output = if args.explain {
        serde_json::json!({
            "config": resolved.redacted_config,
            "layers": resolved.layers,
            "explain": resolved.explain,
        })
    } else {
        resolved.redacted_config
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&output).map_err(|error| error.to_string())?
    );
    Ok(())
}

fn run_validate(args: ConfigValidateArgs) -> Result<(), String> {
    let files = if args.files.is_empty() {
        discovered_validation_files()
    } else {
        args.files
    };
    if files.is_empty() {
        println!("No Harn config files found.");
        return Ok(());
    }
    for path in files {
        let source = path.display().to_string();
        let content = fs::read_to_string(&path)
            .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
        if path.file_name().is_some_and(|name| name == MANIFEST) {
            if let Some(value) = harn_vm::config::parse_manifest_config_table(&content, &source)
                .map_err(|error| error.to_string())?
            {
                if args.managed {
                    harn_vm::config::validate_policy_paths(&value)
                        .map_err(|error| error.to_string())?;
                }
            }
        } else {
            let value = harn_vm::config::parse_config_toml(&content, &source)
                .map_err(|error| error.to_string())?;
            if args.managed {
                harn_vm::config::validate_policy_paths(&value)
                    .map_err(|error| error.to_string())?;
            }
        }
        let kind = if args.managed {
            "managed policy"
        } else {
            "config"
        };
        println!("ok: {kind} {}", path.display());
    }
    Ok(())
}

async fn build_layers(
    args: &ConfigInspectArgs,
) -> Result<Vec<harn_vm::config::ConfigLayer>, String> {
    let mut layers = vec![harn_vm::config::built_in_defaults_layer()];

    if !args.no_discovery {
        if let Some(layer) = legacy_provider_layer()? {
            layers.push(layer);
        }
        for path in install_config_paths() {
            push_file_layer(
                &mut layers,
                harn_vm::config::ConfigLayerKind::RuntimeInstallDefaults,
                "runtime install defaults",
                &path,
            )?;
        }
        if let Some(url) = remote_defaults_url(args, true) {
            layers.push(fetch_remote_defaults(&url).await?);
        }
        for path in user_config_paths() {
            push_file_layer(
                &mut layers,
                harn_vm::config::ConfigLayerKind::UserConfig,
                "user config",
                &path,
            )?;
        }
        if let Some(path) =
            find_nearest_named(&env::current_dir().unwrap_or_default(), PROJECT_CONFIG)
        {
            push_file_layer(
                &mut layers,
                harn_vm::config::ConfigLayerKind::ProjectConfig,
                "project config",
                &path,
            )?;
        }
        if let Some(path) = find_nearest_named(&env::current_dir().unwrap_or_default(), MANIFEST) {
            push_manifest_config_layer(&mut layers, &path)?;
        }
    }

    if args.no_discovery {
        if let Some(url) = remote_defaults_url(args, false) {
            layers.push(fetch_remote_defaults(&url).await?);
        }
    }

    for path in &args.config_files {
        push_explicit_file_layer(
            &mut layers,
            harn_vm::config::ConfigLayerKind::ProjectConfig,
            "explicit config",
            path,
        )?;
    }
    if !args.no_discovery {
        for path in managed_config_paths() {
            push_file_layer(
                &mut layers,
                harn_vm::config::ConfigLayerKind::ManagedPolicy,
                "managed policy",
                &path,
            )?;
        }
    }
    for path in &args.managed_files {
        push_explicit_file_layer(
            &mut layers,
            harn_vm::config::ConfigLayerKind::ManagedPolicy,
            "explicit managed policy",
            path,
        )?;
    }
    if let Some(layer) =
        harn_vm::config::environment_layer(env::vars()).map_err(|error| error.to_string())?
    {
        layers.push(layer);
    }

    Ok(layers)
}

fn legacy_provider_layer() -> Result<Option<harn_vm::config::ConfigLayer>, String> {
    let path = if let Ok(path) = env::var("HARN_PROVIDERS_CONFIG") {
        PathBuf::from(path)
    } else {
        let Some(path) = harn_vm::config::user_config_path_for_os(
            env::consts::OS,
            env::var("HOME").ok().as_deref(),
            env::var("XDG_CONFIG_HOME").ok().as_deref(),
            env::var("APPDATA").ok().as_deref(),
        ) else {
            return Ok(None);
        };
        path.with_file_name("providers.toml")
    };
    if !path.is_file() {
        return Ok(None);
    }
    let source = path.display().to_string();
    let content = fs::read_to_string(&path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    let providers =
        toml::from_str::<harn_vm::llm_config::ProvidersConfig>(&content).map_err(|error| {
            format!(
                "failed to parse legacy providers config {source}: {}",
                sanitized_error_message(error)
            )
        })?;
    Ok(Some(harn_vm::config::layer_from_providers_config(
        harn_vm::config::ConfigLayerKind::UserConfig,
        "legacy providers config",
        source,
        &providers,
    )))
}

fn sanitized_error_message(error: impl ToString) -> String {
    error
        .to_string()
        .lines()
        .next()
        .unwrap_or("parse error")
        .to_string()
}

fn push_file_layer(
    layers: &mut Vec<harn_vm::config::ConfigLayer>,
    kind: harn_vm::config::ConfigLayerKind,
    name: &str,
    path: &Path,
) -> Result<(), String> {
    push_file_layer_with_required(layers, kind, name, path, false)
}

fn push_explicit_file_layer(
    layers: &mut Vec<harn_vm::config::ConfigLayer>,
    kind: harn_vm::config::ConfigLayerKind,
    name: &str,
    path: &Path,
) -> Result<(), String> {
    push_file_layer_with_required(layers, kind, name, path, true)
}

fn push_file_layer_with_required(
    layers: &mut Vec<harn_vm::config::ConfigLayer>,
    kind: harn_vm::config::ConfigLayerKind,
    name: &str,
    path: &Path,
    required: bool,
) -> Result<(), String> {
    if !path.is_file() {
        if required {
            return Err(format!("config file {} does not exist", path.display()));
        }
        return Ok(());
    }
    let source = path.display().to_string();
    let content = fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    let value =
        harn_vm::config::parse_config_toml(&content, &source).map_err(|error| error.to_string())?;
    layers.push(harn_vm::config::ConfigLayer::new(kind, name, source, value));
    Ok(())
}

fn push_manifest_config_layer(
    layers: &mut Vec<harn_vm::config::ConfigLayer>,
    path: &Path,
) -> Result<(), String> {
    if !path.is_file() {
        return Ok(());
    }
    let source = path.display().to_string();
    let content = fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    let Some(value) = harn_vm::config::parse_manifest_config_table(&content, &source)
        .map_err(|error| error.to_string())?
    else {
        return Ok(());
    };
    layers.push(harn_vm::config::ConfigLayer::new(
        harn_vm::config::ConfigLayerKind::RepoConfig,
        "repo manifest config",
        source,
        value,
    ));
    Ok(())
}

fn install_config_paths() -> Vec<PathBuf> {
    if let Some(paths) = split_env_paths("HARN_CONFIG_INSTALL_DEFAULTS") {
        return paths;
    }
    vec![harn_vm::config::install_config_path_for_os(
        env::consts::OS,
        env::var("PROGRAMDATA").ok().as_deref(),
    )]
}

fn user_config_paths() -> Vec<PathBuf> {
    if let Some(paths) = split_env_paths("HARN_CONFIG_USER") {
        return paths;
    }
    harn_vm::config::user_config_path_for_os(
        env::consts::OS,
        env::var("HOME").ok().as_deref(),
        env::var("XDG_CONFIG_HOME").ok().as_deref(),
        env::var("APPDATA").ok().as_deref(),
    )
    .into_iter()
    .collect()
}

fn managed_config_paths() -> Vec<PathBuf> {
    split_env_paths("HARN_CONFIG_MANAGED").unwrap_or_default()
}

fn split_env_paths(key: &str) -> Option<Vec<PathBuf>> {
    let value = env::var_os(key)?;
    let paths: Vec<PathBuf> = env::split_paths(&value).collect();
    if paths.is_empty() {
        None
    } else {
        Some(paths)
    }
}

fn remote_defaults_url(args: &ConfigInspectArgs, include_env: bool) -> Option<String> {
    args.remote_defaults_url
        .clone()
        .or_else(|| {
            include_env
                .then(|| env::var("HARN_CONFIG_REMOTE_DEFAULTS_URL").ok())
                .flatten()
        })
        .filter(|value| !value.trim().is_empty())
}

async fn fetch_remote_defaults(url: &str) -> Result<harn_vm::config::ConfigLayer, String> {
    ensure_remote_trusted(url)?;
    let display_url = redacted_url(url);
    let client = net::http_client("cli.config.remote_defaults", Duration::from_secs(15))?;
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|error| {
            format!(
                "failed to fetch remote defaults {display_url}: {}",
                net::reqwest_error(&error)
            )
        })?
        .error_for_status()
        .map_err(|error| {
            format!(
                "failed to fetch remote defaults {display_url}: {}",
                net::reqwest_error(&error)
            )
        })?;
    let text = response
        .text()
        .await
        .map_err(|error| format!("failed to read remote defaults {display_url}: {error}"))?;
    let value = if text.trim_start().starts_with('{') {
        harn_vm::config::parse_config_json(&text, url).map_err(|error| error.to_string())?
    } else {
        harn_vm::config::parse_config_toml(&text, url).map_err(|error| error.to_string())?
    };
    Ok(harn_vm::config::ConfigLayer::new(
        harn_vm::config::ConfigLayerKind::RemoteDefaults,
        "remote defaults",
        url,
        value,
    ))
}

fn ensure_remote_trusted(raw_url: &str) -> Result<(), String> {
    let trusted = matches!(
        env::var("HARN_CONFIG_TRUST_REMOTE").ok().as_deref(),
        Some("1" | "true" | "TRUE" | "yes" | "YES")
    );
    remote_url_trust_error(raw_url, trusted).map_err(|reason| match reason {
        RemoteTrustError::Untrusted => {
            "remote defaults require HARN_CONFIG_TRUST_REMOTE=1 and an https or localhost URL"
                .to_string()
        }
        RemoteTrustError::Invalid(error) => {
            format!("invalid remote defaults URL: {error}")
        }
        RemoteTrustError::UnsupportedScheme => format!(
            "remote defaults URL {} is not trusted; use https or http localhost",
            redacted_url(raw_url)
        ),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RemoteTrustError {
    Untrusted,
    Invalid(String),
    UnsupportedScheme,
}

fn remote_url_trust_error(raw_url: &str, trusted: bool) -> Result<(), RemoteTrustError> {
    if !trusted {
        return Err(RemoteTrustError::Untrusted);
    }
    let url =
        url::Url::parse(raw_url).map_err(|error| RemoteTrustError::Invalid(error.to_string()))?;
    let host = url.host_str().unwrap_or_default();
    let local = matches!(host, "localhost" | "127.0.0.1" | "::1");
    if url.scheme() == "https" || (local && url.scheme() == "http") {
        Ok(())
    } else {
        Err(RemoteTrustError::UnsupportedScheme)
    }
}

fn discovered_validation_files() -> Vec<PathBuf> {
    let mut files = Vec::new();
    for path in install_config_paths()
        .into_iter()
        .chain(user_config_paths())
        .chain(managed_config_paths())
    {
        if path.is_file() {
            files.push(path);
        }
    }
    let cwd = env::current_dir().unwrap_or_default();
    if let Some(path) = find_nearest_named(&cwd, PROJECT_CONFIG) {
        files.push(path);
    }
    if let Some(path) = find_nearest_named(&cwd, MANIFEST) {
        files.push(path);
    }
    files.sort();
    files.dedup();
    files
}

/// Locate the nearest ancestor file named `name` via the shared
/// [`manifest_walk`](harn_modules::manifest_walk) walk. Returns its path.
fn find_nearest_named(start: &Path, name: &str) -> Option<PathBuf> {
    harn_modules::manifest_walk::find_nearest_ancestor(start, name).map(|found| found.path)
}

fn redacted_url(value: &str) -> String {
    if url::Url::parse(value).is_err() {
        return "[redacted]".to_string();
    }
    harn_vm::redact::current_policy().redact_url(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_untrusted_remote_defaults() {
        let error = ensure_remote_trusted("http://example.com/.well-known/harn").unwrap_err();
        assert!(error.contains("HARN_CONFIG_TRUST_REMOTE") || error.contains("not trusted"));
    }

    #[test]
    fn remote_defaults_trust_allows_only_https_or_http_localhost() {
        assert!(remote_url_trust_error("https://example.com/.well-known/harn", true).is_ok());
        assert!(remote_url_trust_error("http://localhost:8787/.well-known/harn", true).is_ok());
        assert_eq!(
            remote_url_trust_error("ftp://localhost/.well-known/harn", true),
            Err(RemoteTrustError::UnsupportedScheme)
        );
        assert_eq!(
            remote_url_trust_error("http://example.com/.well-known/harn", true),
            Err(RemoteTrustError::UnsupportedScheme)
        );
    }

    #[test]
    fn explicit_config_files_must_exist() {
        let mut layers = Vec::new();
        let missing = PathBuf::from("__missing_harn_config.toml");
        let error = push_explicit_file_layer(
            &mut layers,
            harn_vm::config::ConfigLayerKind::ProjectConfig,
            "explicit config",
            &missing,
        )
        .unwrap_err();

        assert!(error.contains("does not exist"));
    }

    #[test]
    fn managed_validation_rejects_invalid_policy_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("policy.toml");
        fs::write(&path, "[policy]\nlocked_fields = [\"limits..network\"]\n").unwrap();

        let error = run_validate(ConfigValidateArgs {
            files: vec![path],
            managed: true,
        })
        .unwrap_err();

        assert!(error.contains("invalid config field path"));
    }

    #[test]
    fn managed_validation_rejects_invalid_manifest_policy_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("harn.toml");
        fs::write(
            &path,
            "[package]\nname = \"demo\"\n\n[config.policy]\ndenied_fields = [\"endpoints..mcp\"]\n",
        )
        .unwrap();

        let error = run_validate(ConfigValidateArgs {
            files: vec![path],
            managed: true,
        })
        .unwrap_err();

        assert!(error.contains("invalid config field path"));
    }
}
