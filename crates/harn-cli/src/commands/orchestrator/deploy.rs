use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::cli::{OrchestratorDeployArgs, OrchestratorDeployProvider, OrchestratorLocalArgs};
use crate::package::{Manifest, TriggerKind};

use super::common;

const CONTAINER_MANIFEST_PATH: &str = "/app/harn.toml";

#[derive(Debug)]
struct ValidatedManifest {
    manifest: Manifest,
    manifest_path: PathBuf,
    manifest_dir: PathBuf,
    trigger_count: usize,
    http_trigger_count: usize,
}

#[derive(Debug)]
struct DeployBundle {
    provider_dir: PathBuf,
    context_dir: PathBuf,
    dockerfile_path: PathBuf,
    spec_path: PathBuf,
    spec_contents: String,
}

#[derive(Debug)]
struct DeployEnv {
    public: BTreeMap<String, String>,
    secrets: BTreeMap<String, String>,
    missing_secret_env: Vec<String>,
}

pub(crate) async fn run(args: OrchestratorDeployArgs) -> Result<(), String> {
    let validated = validate_manifest(&args).await?;
    let env = collect_deploy_env(&args, &validated.manifest)?;
    let bundle = write_bundle(&args, &validated, &env.public)?;

    println!(
        "validated {} trigger(s) from {} ({} HTTP-managed)",
        validated.trigger_count,
        validated.manifest_path.display(),
        validated.http_trigger_count
    );
    println!("wrote deploy bundle: {}", bundle.provider_dir.display());

    if !env.missing_secret_env.is_empty() {
        eprintln!(
            "warning: {} manifest secret env var(s) were not set locally and were not synced: {}",
            env.missing_secret_env.len(),
            env.missing_secret_env.join(", ")
        );
    }

    if args.print {
        println!("{}", bundle.spec_contents);
    }

    let mut plan = Vec::new();
    if args.build {
        plan.push(build_image_command(&args, &bundle));
    }
    plan.extend(public_env_sync_commands(&args, &env.public));
    if !args.no_secret_sync && !env.secrets.is_empty() {
        plan.extend(secret_sync_commands(&args, &env.secrets));
    }
    plan.push(provider_deploy_command(&args, &bundle));

    if args.dry_run {
        println!("dry run; commands not executed:");
        for command in &plan {
            println!("  {}", command.display());
        }
        return Ok(());
    }

    for command in plan {
        run_checked(command)?;
    }

    if let Some(url) = args
        .health_url
        .clone()
        .or_else(|| default_health_url(&args.provider, &args.name))
    {
        probe_health(&url)?;
    }

    Ok(())
}

async fn validate_manifest(args: &OrchestratorDeployArgs) -> Result<ValidatedManifest, String> {
    let manifest_path = absolutize_from_cwd(&args.manifest)?;
    let manifest_dir = manifest_path
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            format!(
                "manifest has no parent directory: {}",
                manifest_path.display()
            )
        })?;
    let manifest = read_manifest(&manifest_path)?;
    let validation_state = tempfile::Builder::new()
        .prefix("harn-deploy-validate-")
        .tempdir()
        .map_err(|error| {
            format!("failed to create temporary deploy validation state dir: {error}")
        })?;
    let local = OrchestratorLocalArgs {
        config: manifest_path.clone(),
        state_dir: validation_state.path().to_path_buf(),
    };
    let ctx = common::load_local_runtime(&local).await?;
    let trigger_count = ctx.collected_triggers.len();
    let http_trigger_count = ctx
        .collected_triggers
        .iter()
        .filter(|trigger| {
            matches!(
                trigger.config.kind,
                TriggerKind::Webhook | TriggerKind::A2aPush
            )
        })
        .count();
    Ok(ValidatedManifest {
        manifest,
        manifest_path,
        manifest_dir,
        trigger_count,
        http_trigger_count,
    })
}

fn write_bundle(
    args: &OrchestratorDeployArgs,
    validated: &ValidatedManifest,
    public_env: &BTreeMap<String, String>,
) -> Result<DeployBundle, String> {
    let provider_dir = validated
        .manifest_dir
        .join(&args.deploy_dir)
        .join(args.provider.as_str());
    fs::create_dir_all(&provider_dir)
        .map_err(|error| format!("failed to create {}: {error}", provider_dir.display()))?;

    let dockerfile_path = provider_dir.join("Dockerfile");
    let dockerfile = render_dockerfile();
    write_if_changed(&dockerfile_path, &dockerfile)?;

    let spec_contents = match args.provider {
        OrchestratorDeployProvider::Render => render_render_yaml(args, public_env),
        OrchestratorDeployProvider::Fly => render_fly_toml(args, public_env),
        OrchestratorDeployProvider::Railway => render_railway_json(args, public_env)?,
    };
    let spec_path = provider_dir.join(provider_spec_file(args.provider));
    write_if_changed(&spec_path, &spec_contents)?;

    Ok(DeployBundle {
        provider_dir,
        context_dir: validated.manifest_dir.clone(),
        dockerfile_path,
        spec_path,
        spec_contents,
    })
}

fn collect_deploy_env(
    args: &OrchestratorDeployArgs,
    manifest: &Manifest,
) -> Result<DeployEnv, String> {
    let state_dir = format!("{}/state", args.data_dir.trim_end_matches('/'));
    let sqlite_path = format!("{}/events.sqlite", args.data_dir.trim_end_matches('/'));
    let mut public = BTreeMap::from([
        (
            "HARN_ORCHESTRATOR_MANIFEST".to_string(),
            CONTAINER_MANIFEST_PATH.to_string(),
        ),
        (
            "HARN_ORCHESTRATOR_LISTEN".to_string(),
            format!("0.0.0.0:{}", args.port),
        ),
        ("HARN_ORCHESTRATOR_STATE_DIR".to_string(), state_dir),
        ("HARN_EVENT_LOG_BACKEND".to_string(), "sqlite".to_string()),
        ("HARN_EVENT_LOG_SQLITE_PATH".to_string(), sqlite_path),
        ("HARN_SECRET_PROVIDERS".to_string(), "env".to_string()),
        ("RUST_LOG".to_string(), "info".to_string()),
    ]);

    let mut secrets = BTreeMap::new();
    let mut missing_secret_env = Vec::new();

    for pair in &args.env {
        let (key, value) = parse_key_value(pair)?;
        public.insert(key, value);
    }
    for pair in &args.secret {
        let (key, value) = parse_key_value(pair)?;
        secrets.insert(key, value);
    }

    for key in [
        "HARN_ORCHESTRATOR_API_KEYS",
        "HARN_ORCHESTRATOR_HMAC_SECRET",
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "OPENROUTER_API_KEY",
        "HF_TOKEN",
        "HUGGINGFACE_API_KEY",
        "TOGETHER_AI_API_KEY",
    ] {
        if secrets.contains_key(key) {
            continue;
        }
        if let Ok(value) = std::env::var(key) {
            if !value.is_empty() {
                secrets.insert(key.to_string(), value);
            }
        }
    }

    for trigger in &manifest.triggers {
        for secret_ref in trigger.secrets.values() {
            let Some(env_name) = secret_ref_env_name(secret_ref) else {
                continue;
            };
            if secrets.contains_key(&env_name) {
                continue;
            }
            match std::env::var(&env_name) {
                Ok(value) if !value.is_empty() => {
                    secrets.insert(env_name, value);
                }
                _ => missing_secret_env.push(env_name),
            }
        }
    }
    missing_secret_env.sort();
    missing_secret_env.dedup();

    Ok(DeployEnv {
        public,
        secrets,
        missing_secret_env,
    })
}

fn render_dockerfile() -> String {
    r#"FROM ghcr.io/burin-labs/harn:latest

WORKDIR /app
COPY . /app

ENV HARN_ORCHESTRATOR_MANIFEST=/app/harn.toml

ENTRYPOINT ["/usr/local/bin/harn", "orchestrator", "serve"]
"#
    .to_string()
}

fn render_render_yaml(
    args: &OrchestratorDeployArgs,
    public_env: &BTreeMap<String, String>,
) -> String {
    let env_vars = public_env
        .iter()
        .map(|(key, value)| {
            format!(
                "      - key: {}\n        value: {}\n",
                yaml_plain(key),
                serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
            )
        })
        .collect::<String>();
    format!(
        r#"services:
  - type: web
    name: {name}
    runtime: image
    image:
      url: {image}
    disk:
      name: harn-data
      mountPath: {data_dir}
      sizeGB: {disk_size_gb}
    envVars:
{env_vars}      - fromGroup: harn-secrets
    healthCheckPath: /healthz
"#,
        name = yaml_plain(&args.name),
        image = yaml_plain(&args.image),
        data_dir = yaml_plain(&args.data_dir),
        disk_size_gb = args.disk_size_gb,
        env_vars = env_vars,
    )
}

fn render_fly_toml(args: &OrchestratorDeployArgs, public_env: &BTreeMap<String, String>) -> String {
    let region = args
        .region
        .as_ref()
        .map(|region| format!("primary_region = {}\n", toml_string(region)))
        .unwrap_or_default();
    let env_vars = public_env
        .iter()
        .map(|(key, value)| format!("  {key} = {}\n", toml_string(value)))
        .collect::<String>();
    format!(
        r#"app = {name}
{region}kill_signal = "SIGTERM"
kill_timeout = "{shutdown_timeout}s"

[build]
  image = {image}

[env]
{env_vars}

[mounts]
  source = "harn_data"
  destination = {data_dir}

[http_service]
  internal_port = {port}
  force_https = true
  auto_stop_machines = false
  auto_start_machines = true
  min_machines_running = 1

  [[http_service.checks]]
    grace_period = "10s"
    interval = "30s"
    method = "GET"
    path = "/healthz"
    timeout = "5s"

[metrics]
  port = {port}
  path = "/metrics"
"#,
        name = toml_string(&args.name),
        region = region,
        shutdown_timeout = args.shutdown_timeout,
        image = toml_string(&args.image),
        port = args.port,
        env_vars = env_vars,
        data_dir = toml_string(&args.data_dir),
    )
}

fn render_railway_json(
    args: &OrchestratorDeployArgs,
    public_env: &BTreeMap<String, String>,
) -> Result<String, String> {
    let value = serde_json::json!({
        "$schema": "https://railway.app/railway.schema.json",
        "build": {
            "builder": "DOCKERFILE",
            "dockerfilePath": "deploy/railway/Dockerfile"
        },
        "deploy": {
            "startCommand": format!("harn orchestrator serve --shutdown-timeout {}", args.shutdown_timeout),
            "healthcheckPath": "/healthz",
            "healthcheckTimeout": 30,
            "restartPolicyType": "ON_FAILURE",
            "restartPolicyMaxRetries": 10
        },
        "environments": {
            "production": {
                "variables": public_env
            }
        }
    });
    serde_json::to_string_pretty(&value)
        .map(|json| format!("{json}\n"))
        .map_err(|error| format!("failed to render railway.json: {error}"))
}

fn build_image_command(args: &OrchestratorDeployArgs, bundle: &DeployBundle) -> PlannedCommand {
    let mut command = PlannedCommand::new("docker");
    if args.no_push {
        command.args(["build", "-f"]);
        command.arg(bundle.dockerfile_path.as_os_str());
        command.args(["-t", args.image.as_str(), "."]);
    } else {
        command.args([
            "buildx",
            "build",
            "--platform",
            "linux/amd64,linux/arm64",
            "-f",
        ]);
        command.arg(bundle.dockerfile_path.as_os_str());
        command.args(["-t", args.image.as_str(), "--push", "."]);
    }
    command.cwd = Some(bundle.context_dir.clone());
    command
}

fn secret_sync_commands(
    args: &OrchestratorDeployArgs,
    secrets: &BTreeMap<String, String>,
) -> Vec<PlannedCommand> {
    match args.provider {
        OrchestratorDeployProvider::Fly => {
            let mut command = PlannedCommand::new("fly");
            command.args(["secrets", "set"]);
            for (key, value) in secrets {
                command.arg_sensitive(format!("{key}={value}"));
            }
            command.args(["--app", args.name.as_str()]);
            vec![command]
        }
        OrchestratorDeployProvider::Railway => secrets
            .iter()
            .map(|(key, value)| {
                let mut command = PlannedCommand::new("railway");
                command.args(["variable", "set"]);
                command.arg_sensitive(format!("{key}={value}"));
                command.arg("--skip-deploys");
                if let Some(service) = &args.railway_service {
                    command.args(["--service", service.as_str()]);
                }
                if let Some(environment) = &args.railway_environment {
                    command.args(["--environment", environment.as_str()]);
                }
                command
            })
            .collect(),
        OrchestratorDeployProvider::Render => {
            let Some(service) = &args.render_service else {
                return Vec::new();
            };
            secrets
                .iter()
                .map(|(key, value)| {
                    let mut command = PlannedCommand::new("render");
                    command.args(["services", "update", service.as_str()]);
                    command.arg("--env-var");
                    command.arg_sensitive(format!("{key}={value}"));
                    command.arg("--confirm");
                    command
                })
                .collect()
        }
    }
}

fn public_env_sync_commands(
    args: &OrchestratorDeployArgs,
    public_env: &BTreeMap<String, String>,
) -> Vec<PlannedCommand> {
    if args.provider != OrchestratorDeployProvider::Railway {
        return Vec::new();
    }

    let mut vars = public_env.clone();
    vars.insert(
        "RAILWAY_DOCKERFILE_PATH".to_string(),
        "deploy/railway/Dockerfile".to_string(),
    );

    vars.iter()
        .map(|(key, value)| {
            let mut command = PlannedCommand::new("railway");
            command.args(["variable", "set"]);
            command.arg(format!("{key}={value}"));
            command.arg("--skip-deploys");
            if let Some(service) = &args.railway_service {
                command.args(["--service", service.as_str()]);
            }
            if let Some(environment) = &args.railway_environment {
                command.args(["--environment", environment.as_str()]);
            }
            command
        })
        .collect()
}

fn provider_deploy_command(args: &OrchestratorDeployArgs, bundle: &DeployBundle) -> PlannedCommand {
    match args.provider {
        OrchestratorDeployProvider::Render => {
            if let Some(service) = &args.render_service {
                let mut command = PlannedCommand::new("render");
                command.args(["deploys", "create", service.as_str()]);
                command.args(["--image", args.image.as_str(), "--wait", "--confirm"]);
                command
            } else {
                let mut command = PlannedCommand::new("render");
                command.args(["blueprints", "validate"]);
                command.arg(bundle.spec_path.as_os_str());
                command
            }
        }
        OrchestratorDeployProvider::Fly => {
            let mut command = PlannedCommand::new("fly");
            command.args(["deploy", "--config"]);
            command.arg(bundle.spec_path.as_os_str());
            command.args(["--app", args.name.as_str()]);
            command
        }
        OrchestratorDeployProvider::Railway => {
            let mut command = PlannedCommand::new("railway");
            command.args(["up", "--yes"]);
            if let Some(service) = &args.railway_service {
                command.args(["--service", service.as_str()]);
            }
            if let Some(environment) = &args.railway_environment {
                command.args(["--environment", environment.as_str()]);
            }
            command.cwd = Some(bundle.context_dir.clone());
            command
        }
    }
}

#[derive(Debug, Clone)]
struct PlannedCommand {
    program: String,
    args: Vec<String>,
    sensitive_args: BTreeSet<usize>,
    cwd: Option<PathBuf>,
}

impl PlannedCommand {
    fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            sensitive_args: BTreeSet::new(),
            cwd: None,
        }
    }

    fn arg(&mut self, arg: impl AsRef<OsStr>) -> &mut Self {
        self.args.push(arg.as_ref().to_string_lossy().into_owned());
        self
    }

    fn arg_sensitive(&mut self, arg: impl AsRef<OsStr>) -> &mut Self {
        let index = self.args.len();
        self.arg(arg);
        self.sensitive_args.insert(index);
        self
    }

    fn args<I, S>(&mut self, args: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        for arg in args {
            self.arg(arg);
        }
        self
    }

    fn display(&self) -> String {
        let mut rendered = shell_quote(&self.program);
        for (index, arg) in self.args.iter().enumerate() {
            rendered.push(' ');
            let display_arg = if self.sensitive_args.contains(&index) {
                redact_arg(arg)
            } else {
                arg.to_string()
            };
            rendered.push_str(&shell_quote(&display_arg));
        }
        if let Some(cwd) = &self.cwd {
            format!(
                "(cd {} && {rendered})",
                shell_quote(&cwd.display().to_string())
            )
        } else {
            rendered
        }
    }
}

fn run_checked(command: PlannedCommand) -> Result<(), String> {
    println!("running: {}", command.display());
    let mut process = Command::new(&command.program);
    process.args(&command.args);
    if let Some(cwd) = &command.cwd {
        process.current_dir(cwd);
    }
    let status = process
        .status()
        .map_err(|error| format!("failed to run {}: {error}", command.program))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "command failed with {status}: {}",
            command.display()
        ))
    }
}

fn probe_health(url: &str) -> Result<(), String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|error| format!("failed to create health probe client: {error}"))?;
    let response = client
        .get(url)
        .send()
        .map_err(|error| format!("health probe failed for {url}: {error}"))?;
    if response.status().is_success() {
        println!("health check passed: {url}");
        Ok(())
    } else {
        Err(format!(
            "health probe failed for {url}: HTTP {}",
            response.status()
        ))
    }
}

fn default_health_url(provider: &OrchestratorDeployProvider, name: &str) -> Option<String> {
    match provider {
        OrchestratorDeployProvider::Fly => Some(format!("https://{name}.fly.dev/healthz")),
        OrchestratorDeployProvider::Render | OrchestratorDeployProvider::Railway => None,
    }
}

fn provider_spec_file(provider: OrchestratorDeployProvider) -> &'static str {
    match provider {
        OrchestratorDeployProvider::Render => "render.yaml",
        OrchestratorDeployProvider::Fly => "fly.toml",
        OrchestratorDeployProvider::Railway => "railway.json",
    }
}

fn read_manifest(path: &Path) -> Result<Manifest, String> {
    if !path.is_file() {
        return Err(format!("manifest not found: {}", path.display()));
    }
    let content = fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    toml::from_str(&content).map_err(|error| format!("failed to parse {}: {error}", path.display()))
}

fn absolutize_from_cwd(path: &Path) -> Result<PathBuf, String> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(path))
        .map_err(|error| format!("failed to read current directory: {error}"))
}

fn write_if_changed(path: &Path, content: &str) -> Result<(), String> {
    if fs::read_to_string(path).is_ok_and(|existing| existing == content) {
        return Ok(());
    }
    fs::write(path, content).map_err(|error| format!("failed to write {}: {error}", path.display()))
}

fn parse_key_value(raw: &str) -> Result<(String, String), String> {
    let Some((key, value)) = raw.split_once('=') else {
        return Err(format!("expected KEY=VALUE, got '{raw}'"));
    };
    let key = key.trim();
    if key.is_empty() {
        return Err(format!("expected non-empty KEY in '{raw}'"));
    }
    Ok((key.to_string(), value.to_string()))
}

fn secret_ref_env_name(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let (base, version) = match trimmed.rsplit_once('@') {
        Some((base, version_text)) => Some((base, version_text.parse::<u64>().ok()?)),
        None => None,
    }
    .unwrap_or((trimmed, 0));
    let (namespace, name) = base.split_once('/')?;
    if namespace.is_empty() || name.is_empty() {
        return None;
    }
    let prefix = format!(
        "HARN_SECRET_{}_{}",
        normalize_env_component(namespace),
        normalize_env_component(name)
    );
    if version == 0 {
        Some(prefix)
    } else {
        Some(format!("{prefix}_V{version}"))
    }
}

fn normalize_env_component(value: &str) -> String {
    let mut normalized = String::new();
    let mut last_was_underscore = false;
    for ch in value.chars() {
        let mapped = if ch.is_ascii_alphanumeric() {
            ch.to_ascii_uppercase()
        } else {
            '_'
        };
        if mapped == '_' {
            if !last_was_underscore {
                normalized.push(mapped);
            }
            last_was_underscore = true;
        } else {
            normalized.push(mapped);
            last_was_underscore = false;
        }
    }
    normalized.trim_matches('_').to_string()
}

fn toml_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
}

fn yaml_plain(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | ':' | '/'))
    {
        value.to_string()
    } else {
        serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
    }
}

fn shell_quote(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '/' | '.' | '-' | '_' | ':' | '='))
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn redact_arg(value: &str) -> String {
    match value.split_once('=') {
        Some((key, _)) => format!("{key}=***"),
        None => "***".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(provider: OrchestratorDeployProvider) -> OrchestratorDeployArgs {
        OrchestratorDeployArgs {
            provider,
            manifest: PathBuf::from("harn.toml"),
            name: "harn-prod".to_string(),
            image: "ghcr.io/acme/harn-prod:latest".to_string(),
            deploy_dir: PathBuf::from("deploy"),
            port: 8080,
            data_dir: "/data".to_string(),
            disk_size_gb: 10,
            shutdown_timeout: 45,
            region: Some("sjc".to_string()),
            render_service: Some("srv-123".to_string()),
            railway_service: Some("harn-prod".to_string()),
            railway_environment: Some("production".to_string()),
            build: false,
            no_push: false,
            env: vec![],
            secret: vec![],
            no_secret_sync: false,
            dry_run: true,
            print: false,
            health_url: None,
        }
    }

    fn env() -> BTreeMap<String, String> {
        BTreeMap::from([
            (
                "HARN_ORCHESTRATOR_MANIFEST".to_string(),
                CONTAINER_MANIFEST_PATH.to_string(),
            ),
            (
                "HARN_ORCHESTRATOR_LISTEN".to_string(),
                "0.0.0.0:8080".to_string(),
            ),
            (
                "HARN_ORCHESTRATOR_STATE_DIR".to_string(),
                "/data/state".to_string(),
            ),
            ("HARN_EVENT_LOG_BACKEND".to_string(), "sqlite".to_string()),
            (
                "HARN_EVENT_LOG_SQLITE_PATH".to_string(),
                "/data/events.sqlite".to_string(),
            ),
            ("HARN_SECRET_PROVIDERS".to_string(), "env".to_string()),
            ("RUST_LOG".to_string(), "info".to_string()),
        ])
    }

    #[test]
    fn render_template_uses_current_orchestrator_env_names() {
        let rendered = render_render_yaml(&args(OrchestratorDeployProvider::Render), &env());
        assert!(rendered.contains("HARN_ORCHESTRATOR_LISTEN"));
        assert!(rendered.contains("HARN_EVENT_LOG_BACKEND"));
        assert!(rendered.contains("healthCheckPath: /healthz"));
    }

    #[test]
    fn fly_template_keeps_one_instance_for_cron_and_metrics() {
        let rendered = render_fly_toml(&args(OrchestratorDeployProvider::Fly), &env());
        assert!(rendered.contains("min_machines_running = 1"));
        assert!(rendered.contains("[metrics]"));
        assert!(rendered.contains("kill_timeout = \"45s\""));
    }

    #[test]
    fn railway_template_is_valid_json() {
        let rendered =
            render_railway_json(&args(OrchestratorDeployProvider::Railway), &env()).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&rendered).unwrap();
        assert_eq!(parsed["deploy"]["healthcheckPath"], "/healthz");
        assert_eq!(
            parsed["environments"]["production"]["variables"]["HARN_SECRET_PROVIDERS"],
            "env"
        );
    }

    #[test]
    fn manifest_secret_refs_map_to_env_vars() {
        assert_eq!(
            secret_ref_env_name("github/installation-123/private-key"),
            Some("HARN_SECRET_GITHUB_INSTALLATION_123_PRIVATE_KEY".to_string())
        );
        assert_eq!(
            secret_ref_env_name("slack/signing-secret@7"),
            Some("HARN_SECRET_SLACK_SIGNING_SECRET_V7".to_string())
        );
    }

    #[test]
    fn provider_commands_are_rendered_without_secrets_in_specs() {
        let mut secrets = BTreeMap::new();
        secrets.insert("OPENAI_API_KEY".to_string(), "sk-test".to_string());
        let commands = secret_sync_commands(&args(OrchestratorDeployProvider::Fly), &secrets);
        assert_eq!(commands.len(), 1);
        assert!(commands[0].display().contains("fly secrets set"));
        assert!(commands[0].display().contains("OPENAI_API_KEY=***"));
        assert!(!commands[0].display().contains("sk-test"));
    }

    #[test]
    fn railway_syncs_public_env_and_custom_dockerfile_path() {
        let commands = public_env_sync_commands(&args(OrchestratorDeployProvider::Railway), &env());
        assert!(commands
            .iter()
            .any(|command| command.display().contains("RAILWAY_DOCKERFILE_PATH")));
        assert!(commands
            .iter()
            .any(|command| command.display().contains("HARN_ORCHESTRATOR_LISTEN")));
    }

    #[test]
    fn build_command_uses_manifest_context_even_with_nested_deploy_dir() {
        let mut args = args(OrchestratorDeployProvider::Fly);
        args.deploy_dir = PathBuf::from("ops/deploy");
        args.build = true;
        let bundle = DeployBundle {
            provider_dir: PathBuf::from("/repo/ops/deploy/fly"),
            context_dir: PathBuf::from("/repo"),
            dockerfile_path: PathBuf::from("/repo/ops/deploy/fly/Dockerfile"),
            spec_path: PathBuf::from("/repo/ops/deploy/fly/fly.toml"),
            spec_contents: String::new(),
        };
        let command = build_image_command(&args, &bundle);
        assert_eq!(command.cwd.as_deref(), Some(Path::new("/repo")));
        assert!(command
            .display()
            .contains("/repo/ops/deploy/fly/Dockerfile"));
    }
}
