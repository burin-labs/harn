use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use harn_vm::llm_config::{self, AuthEnv, ProviderDef};
use harn_vm::runtime_paths;
use harn_vm::secrets::{
    configured_default_chain, EnvSecretProvider, KeyringSecretProvider, SecretId,
    DEFAULT_SECRET_PROVIDER_CHAIN, SECRET_PROVIDER_CHAIN_ENV,
};
use reqwest::{header::CONTENT_TYPE, Method};

use crate::package;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DoctorStatus {
    Ok,
    Warn,
    Fail,
    Skip,
}

impl DoctorStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Ok => "OK",
            Self::Warn => "WARN",
            Self::Fail => "FAIL",
            Self::Skip => "SKIP",
        }
    }
}

#[derive(Debug, Clone)]
struct DoctorCheck {
    status: DoctorStatus,
    label: String,
    detail: String,
}

pub(crate) async fn run_doctor(network: bool) {
    let mut checks = Vec::new();
    checks.push(check_binary("rustc"));
    checks.push(check_binary("cargo"));
    checks.extend(check_provider_selection());
    checks.extend(check_secret_providers());
    checks.extend(check_manifest().await);
    checks.extend(check_event_log());
    checks.extend(check_notion_connector_state().await);
    checks.extend(check_metadata_cache());
    checks.extend(check_skills());
    checks.extend(check_provider_health(network).await);

    let mut failed = false;
    println!("Harn doctor");
    println!();
    for check in checks {
        if check.status == DoctorStatus::Fail {
            failed = true;
        }
        println!(
            "{:>4}  {:<22} {}",
            check.status.label(),
            check.label,
            check.detail
        );
    }

    if failed {
        std::process::exit(1);
    }
}

fn check_binary(name: &str) -> DoctorCheck {
    match Command::new(name).arg("--version").output() {
        Ok(output) if output.status.success() => DoctorCheck {
            status: DoctorStatus::Ok,
            label: name.to_string(),
            detail: String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .unwrap_or("version detected")
                .to_string(),
        },
        Ok(output) => DoctorCheck {
            status: DoctorStatus::Fail,
            label: name.to_string(),
            detail: format!("command exists but exited with {}", output.status),
        },
        Err(error) => DoctorCheck {
            status: DoctorStatus::Fail,
            label: name.to_string(),
            detail: format!("not found in PATH: {error}"),
        },
    }
}

fn check_provider_selection() -> Vec<DoctorCheck> {
    let mut checks = Vec::new();

    if let Ok(path) = std::env::var("HARN_PROVIDERS_CONFIG") {
        let config_path = PathBuf::from(&path);
        let status = if config_path.is_file() {
            DoctorStatus::Ok
        } else {
            DoctorStatus::Fail
        };
        checks.push(DoctorCheck {
            status,
            label: "providers config".to_string(),
            detail: format!("HARN_PROVIDERS_CONFIG={path}"),
        });
    }

    if let Ok(provider) = std::env::var("HARN_LLM_PROVIDER") {
        let status = if llm_config::provider_config(&provider).is_some() {
            DoctorStatus::Ok
        } else {
            DoctorStatus::Fail
        };
        checks.push(DoctorCheck {
            status,
            label: "selected provider".to_string(),
            detail: format!("HARN_LLM_PROVIDER={provider}"),
        });
    }

    checks
}

fn check_secret_providers() -> Vec<DoctorCheck> {
    let namespace = default_secret_namespace();
    let configured = std::env::var(SECRET_PROVIDER_CHAIN_ENV)
        .unwrap_or_else(|_| DEFAULT_SECRET_PROVIDER_CHAIN.to_string());
    let mut checks = Vec::new();

    match configured_default_chain(namespace.clone()) {
        Ok(chain) => checks.push(DoctorCheck {
            status: if chain.providers().is_empty() {
                DoctorStatus::Fail
            } else {
                DoctorStatus::Ok
            },
            label: "secret providers".to_string(),
            detail: format!(
                "{} (namespace {})",
                configured.replace(',', " -> "),
                namespace
            ),
        }),
        Err(error) => {
            checks.push(DoctorCheck {
                status: DoctorStatus::Fail,
                label: "secret providers".to_string(),
                detail: error.to_string(),
            });
            return checks;
        }
    }

    for provider in configured
        .split(',')
        .map(str::trim)
        .filter(|provider| !provider.is_empty())
    {
        match provider {
            "env" => {
                let env_provider = EnvSecretProvider::new(namespace.clone());
                let sample = env_provider.env_var_name(&SecretId::new("sample", "token"));
                checks.push(DoctorCheck {
                    status: DoctorStatus::Ok,
                    label: "secret:env".to_string(),
                    detail: format!("reads process env via {sample}"),
                });
            }
            "keyring" => {
                let keyring_provider = KeyringSecretProvider::new(namespace.clone());
                match keyring_provider.healthcheck() {
                    Ok(detail) => checks.push(DoctorCheck {
                        status: DoctorStatus::Ok,
                        label: "secret:keyring".to_string(),
                        detail,
                    }),
                    Err(error) => checks.push(DoctorCheck {
                        status: DoctorStatus::Fail,
                        label: "secret:keyring".to_string(),
                        detail: error.to_string(),
                    }),
                }
            }
            other => checks.push(DoctorCheck {
                status: DoctorStatus::Fail,
                label: format!("secret:{other}"),
                detail: format!("unsupported provider '{other}'"),
            }),
        }
    }

    checks
}

async fn check_manifest() -> Vec<DoctorCheck> {
    let Some(path) = find_nearest_manifest(&std::env::current_dir().unwrap_or_default()) else {
        return vec![DoctorCheck {
            status: DoctorStatus::Warn,
            label: "manifest".to_string(),
            detail: "no harn.toml found in the current directory or its parents".to_string(),
        }];
    };

    let manifest_result = read_manifest(&path);
    let manifest = match manifest_result {
        Ok(manifest) => manifest,
        Err(error) => {
            return vec![DoctorCheck {
                status: DoctorStatus::Fail,
                label: "manifest".to_string(),
                detail: format!("{}: {error}", path.display()),
            }];
        }
    };

    let package_name = manifest
        .package
        .as_ref()
        .and_then(|pkg| pkg.name.clone())
        .unwrap_or_else(|| "unnamed package".to_string());

    let mut checks = vec![DoctorCheck {
        status: DoctorStatus::Ok,
        label: "manifest".to_string(),
        detail: format!("{} ({package_name})", path.display()),
    }];

    let mut seen_names = HashSet::new();
    for server in &manifest.mcp {
        let name = server.name.clone();
        if !seen_names.insert(name.clone()) {
            checks.push(DoctorCheck {
                status: DoctorStatus::Fail,
                label: format!("mcp:{name}"),
                detail: "duplicate MCP server name".to_string(),
            });
            continue;
        }
        if server.url.trim().is_empty() && server.command.trim().is_empty() {
            checks.push(DoctorCheck {
                status: DoctorStatus::Warn,
                label: format!("mcp:{name}"),
                detail: "entry has neither url nor command".to_string(),
            });
        } else {
            checks.push(DoctorCheck {
                status: DoctorStatus::Ok,
                label: format!("mcp:{name}"),
                detail: if !server.url.trim().is_empty() {
                    format!("remote {}", server.url)
                } else {
                    format!("stdio {}", server.command)
                },
            });
        }
    }

    let extensions = package::load_runtime_extensions(&path);
    if !extensions.triggers.is_empty() {
        let mut vm = harn_vm::Vm::new();
        harn_vm::register_vm_stdlib(&mut vm);
        harn_vm::clear_trigger_registry();
        match package::install_manifest_triggers(&mut vm, &extensions).await {
            Ok(()) => {
                for trigger in harn_vm::snapshot_trigger_bindings() {
                    checks.push(DoctorCheck {
                        status: DoctorStatus::Ok,
                        label: format!("trigger:{}", trigger.id),
                        detail: format!(
                            "{} via {} handler={} state={} version={} metrics={}",
                            trigger.kind,
                            trigger.provider,
                            trigger.handler_kind,
                            trigger.state.as_str(),
                            trigger.version,
                            format_trigger_metrics(&trigger.metrics),
                        ),
                    });
                }
                let dispatcher = harn_vm::snapshot_dispatcher_stats();
                checks.push(DoctorCheck {
                    status: DoctorStatus::Ok,
                    label: "dispatcher".to_string(),
                    detail: format!(
                        "in_flight={} retry_queue_depth={} dlq_depth={}",
                        dispatcher.in_flight, dispatcher.retry_queue_depth, dispatcher.dlq_depth,
                    ),
                });
                harn_vm::clear_trigger_registry();
            }
            Err(error) => checks.push(DoctorCheck {
                status: DoctorStatus::Fail,
                label: "triggers".to_string(),
                detail: error,
            }),
        }
    }

    checks
}

fn format_trigger_metrics(metrics: &harn_vm::TriggerMetricsSnapshot) -> String {
    format!(
        "received={} dispatched={} failed={} dlq={} in_flight={}",
        metrics.received, metrics.dispatched, metrics.failed, metrics.dlq, metrics.in_flight
    )
}

fn check_skills() -> Vec<DoctorCheck> {
    use crate::skill_loader;

    let loaded = skill_loader::load_skills(&skill_loader::SkillLoaderInputs {
        cli_dirs: Vec::new(),
        source_path: None,
    });

    let mut checks = Vec::new();
    let winners = &loaded.report.winners;
    if winners.is_empty() {
        checks.push(DoctorCheck {
            status: DoctorStatus::Skip,
            label: "skills".to_string(),
            detail: "no SKILL.md files discovered (use --skill-dir, $HARN_SKILLS_PATH, .harn/skills, or harn.toml [skills])".to_string(),
        });
    } else {
        let mut by_layer: std::collections::BTreeMap<&str, usize> =
            std::collections::BTreeMap::new();
        for w in winners {
            *by_layer.entry(w.layer.label()).or_default() += 1;
        }
        let breakdown: Vec<String> = by_layer.iter().map(|(k, v)| format!("{v} {k}")).collect();
        checks.push(DoctorCheck {
            status: DoctorStatus::Ok,
            label: "skills".to_string(),
            detail: format!("{} loaded ({})", winners.len(), breakdown.join(", ")),
        });
    }

    for shadow in &loaded.report.shadowed {
        checks.push(DoctorCheck {
            status: DoctorStatus::Warn,
            label: format!("skill:{}", shadow.id),
            detail: format!(
                "shadowed by {} layer; {} version at {} is hidden",
                shadow.winner.label(),
                shadow.loser.label(),
                shadow.loser_origin,
            ),
        });
    }

    for (id, fields) in &loaded.report.unknown_fields {
        checks.push(DoctorCheck {
            status: DoctorStatus::Warn,
            label: format!("skill:{id}"),
            detail: format!(
                "unknown frontmatter field(s) forwarded as metadata: {}",
                fields.join(", ")
            ),
        });
    }

    for layer in &loaded.report.disabled_layers {
        checks.push(DoctorCheck {
            status: DoctorStatus::Skip,
            label: format!("skills-layer:{}", layer.label()),
            detail: "layer disabled by harn.toml [skills.disable]".to_string(),
        });
    }

    checks
}

fn check_metadata_cache() -> Vec<DoctorCheck> {
    let cwd = std::env::current_dir().unwrap_or_default();
    let metadata_dir = runtime_paths::metadata_dir(&cwd);
    let read_dir = match fs::read_dir(&metadata_dir) {
        Ok(read_dir) => read_dir,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return vec![DoctorCheck {
                status: DoctorStatus::Skip,
                label: "metadata".to_string(),
                detail: format!("no metadata cache under {}", metadata_dir.display()),
            }];
        }
        Err(error) => {
            return vec![DoctorCheck {
                status: DoctorStatus::Warn,
                label: "metadata".to_string(),
                detail: format!("failed to read {}: {error}", metadata_dir.display()),
            }];
        }
    };

    let mut namespace_summaries = Vec::new();
    let mut saw_legacy_root = false;
    for entry in read_dir.flatten() {
        let path = entry.path();
        if path.is_file() && entry.file_name() == "root.json" {
            saw_legacy_root = true;
            continue;
        }
        if !path.is_dir() {
            continue;
        }
        let shard_path = path.join("entries.json");
        let Ok(text) = fs::read_to_string(&shard_path) else {
            continue;
        };
        let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let Some(namespace) = parsed.get("namespace").and_then(|value| value.as_str()) else {
            continue;
        };
        let count = parsed
            .get("entries")
            .and_then(|value| value.as_object())
            .map(|entries| entries.len())
            .unwrap_or(0);
        namespace_summaries.push(format!("{namespace} ({count} dirs)"));
    }

    namespace_summaries.sort();
    let detail = if namespace_summaries.is_empty() {
        if saw_legacy_root {
            format!(
                "legacy metadata shard present at {}",
                metadata_dir.join("root.json").display()
            )
        } else {
            format!(
                "metadata directory present at {} but no namespace shards found",
                metadata_dir.display()
            )
        }
    } else {
        namespace_summaries.join(", ")
    };

    vec![DoctorCheck {
        status: DoctorStatus::Ok,
        label: "metadata".to_string(),
        detail,
    }]
}

fn check_event_log() -> Vec<DoctorCheck> {
    let cwd = std::env::current_dir().unwrap_or_default();
    match harn_vm::event_log::describe_for_base_dir(&cwd) {
        Ok(description) => {
            let detail = match description.location {
                Some(path) => format!(
                    "{} ({}, {} B)",
                    description.backend,
                    path.display(),
                    description.size_bytes.unwrap_or(0)
                ),
                None => format!("{} (in-memory)", description.backend),
            };
            vec![DoctorCheck {
                status: DoctorStatus::Ok,
                label: "event log".to_string(),
                detail,
            }]
        }
        Err(error) => vec![DoctorCheck {
            status: DoctorStatus::Fail,
            label: "event log".to_string(),
            detail: error.to_string(),
        }],
    }
}

async fn check_notion_connector_state() -> Vec<DoctorCheck> {
    let cwd = std::env::current_dir().unwrap_or_default();
    let config = match harn_vm::event_log::EventLogConfig::for_base_dir(&cwd) {
        Ok(config) => config,
        Err(error) => {
            return vec![DoctorCheck {
                status: DoctorStatus::Warn,
                label: "notion".to_string(),
                detail: format!("failed to resolve event log config: {error}"),
            }]
        }
    };
    let log = match harn_vm::event_log::open_event_log(&config) {
        Ok(log) => log,
        Err(error) => {
            return vec![DoctorCheck {
                status: DoctorStatus::Warn,
                label: "notion".to_string(),
                detail: format!("failed to open event log: {error}"),
            }]
        }
    };
    let handshakes = match harn_vm::load_pending_webhook_handshakes(log.as_ref()).await {
        Ok(handshakes) => handshakes,
        Err(error) => {
            return vec![DoctorCheck {
                status: DoctorStatus::Warn,
                label: "notion".to_string(),
                detail: format!("failed to inspect webhook handshake state: {error}"),
            }]
        }
    };
    if handshakes.is_empty() {
        return vec![DoctorCheck {
            status: DoctorStatus::Skip,
            label: "notion".to_string(),
            detail: "no pending webhook verification tokens recorded".to_string(),
        }];
    }
    handshakes
        .into_values()
        .map(|handshake| DoctorCheck {
            status: DoctorStatus::Warn,
            label: format!("notion:{}", handshake.binding_id),
            detail: format!(
                "captured verification_token={} at {}{}",
                handshake.verification_token,
                handshake.captured_at,
                handshake
                    .path
                    .as_deref()
                    .map(|path| format!(" (path {path})"))
                    .unwrap_or_default(),
            ),
        })
        .collect()
}

async fn check_provider_health(network: bool) -> Vec<DoctorCheck> {
    let mut providers = llm_config::provider_names();
    providers.sort();

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("reqwest client");

    let mut checks = Vec::new();
    for provider_name in providers {
        let Some(def) = llm_config::provider_config(&provider_name) else {
            continue;
        };

        let auth = resolve_auth_env(&def.auth_env);
        if !network {
            checks.push(DoctorCheck {
                status: DoctorStatus::Skip,
                label: format!("provider:{provider_name}"),
                detail: "network checks disabled".to_string(),
            });
            continue;
        }

        if !auth.available && !matches!(def.auth_env, AuthEnv::None) {
            checks.push(DoctorCheck {
                status: DoctorStatus::Warn,
                label: format!("provider:{provider_name}"),
                detail: format!("missing credentials ({})", auth.candidates.join(", ")),
            });
            continue;
        }

        let Some(healthcheck) = def.healthcheck.as_ref() else {
            checks.push(DoctorCheck {
                status: DoctorStatus::Skip,
                label: format!("provider:{provider_name}"),
                detail: "no healthcheck configured".to_string(),
            });
            continue;
        };

        checks.push(run_healthcheck(&client, &provider_name, &def, &auth, healthcheck).await);
    }
    checks
}

async fn run_healthcheck(
    client: &reqwest::Client,
    provider_name: &str,
    def: &ProviderDef,
    auth: &ResolvedAuth,
    healthcheck: &harn_vm::llm_config::HealthcheckDef,
) -> DoctorCheck {
    let url = build_healthcheck_url(def, healthcheck);
    let method = Method::from_bytes(healthcheck.method.as_bytes()).unwrap_or(Method::GET);
    let mut request = client.request(method, &url);

    match def.auth_style.as_str() {
        "bearer" => {
            if let Some(value) = auth.value.as_deref() {
                request = request.bearer_auth(value);
            }
        }
        "header" => {
            if let (Some(header), Some(value)) = (def.auth_header.as_deref(), auth.value.as_deref())
            {
                request = request.header(header, value);
            }
        }
        _ => {}
    }

    for (name, value) in &def.extra_headers {
        request = request.header(name, value);
    }

    if let Some(body) = &healthcheck.body {
        request = request
            .header(CONTENT_TYPE, "application/json")
            .body(body.clone());
    }

    match request.send().await {
        Ok(response) if response.status().is_success() => DoctorCheck {
            status: DoctorStatus::Ok,
            label: format!("provider:{provider_name}"),
            detail: format!("{} {}", response.status().as_u16(), url),
        },
        Ok(response) if response.status().as_u16() == 401 || response.status().as_u16() == 403 => {
            DoctorCheck {
                status: DoctorStatus::Fail,
                label: format!("provider:{provider_name}"),
                detail: format!(
                    "auth rejected with {} at {}",
                    response.status().as_u16(),
                    url
                ),
            }
        }
        Ok(response) => DoctorCheck {
            status: DoctorStatus::Warn,
            label: format!("provider:{provider_name}"),
            detail: format!("unexpected HTTP {} at {}", response.status().as_u16(), url),
        },
        Err(error) => DoctorCheck {
            status: DoctorStatus::Fail,
            label: format!("provider:{provider_name}"),
            detail: format!("request failed for {}: {error}", url),
        },
    }
}

#[derive(Debug, Clone)]
struct ResolvedAuth {
    available: bool,
    value: Option<String>,
    candidates: Vec<String>,
}

fn resolve_auth_env(auth_env: &AuthEnv) -> ResolvedAuth {
    match auth_env {
        AuthEnv::None => ResolvedAuth {
            available: true,
            value: None,
            candidates: Vec::new(),
        },
        AuthEnv::Single(env_name) => {
            let value = std::env::var(env_name)
                .ok()
                .filter(|value| !value.trim().is_empty());
            ResolvedAuth {
                available: value.is_some(),
                value,
                candidates: vec![env_name.clone()],
            }
        }
        AuthEnv::Multiple(env_names) => {
            let value = env_names.iter().find_map(|env_name| {
                std::env::var(env_name)
                    .ok()
                    .filter(|value| !value.trim().is_empty())
            });
            ResolvedAuth {
                available: value.is_some(),
                value,
                candidates: env_names.clone(),
            }
        }
    }
}

fn build_healthcheck_url(
    def: &ProviderDef,
    healthcheck: &harn_vm::llm_config::HealthcheckDef,
) -> String {
    if let Some(url) = &healthcheck.url {
        return url.clone();
    }

    let base = llm_config::resolve_base_url(def);
    let path = healthcheck.path.as_deref().unwrap_or("");
    if path.starts_with('/') {
        format!("{}{}", base.trim_end_matches('/'), path)
    } else if path.is_empty() {
        base
    } else {
        format!("{}/{}", base.trim_end_matches('/'), path)
    }
}

fn find_nearest_manifest(start: &Path) -> Option<PathBuf> {
    let mut dir = start.to_path_buf();
    loop {
        let manifest = dir.join("harn.toml");
        if manifest.is_file() {
            return Some(manifest);
        }
        if !dir.pop() {
            return None;
        }
    }
}

fn default_secret_namespace() -> String {
    if let Ok(namespace) = std::env::var("HARN_SECRET_NAMESPACE") {
        if !namespace.trim().is_empty() {
            return namespace;
        }
    }

    let cwd = std::env::current_dir().unwrap_or_default();
    let leaf = cwd
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("workspace");
    format!("harn/{leaf}")
}

fn read_manifest(path: &Path) -> Result<package::Manifest, String> {
    let content =
        fs::read_to_string(path).map_err(|error| format!("failed to read manifest: {error}"))?;
    toml::from_str::<package::Manifest>(&content)
        .map_err(|error| format!("failed to parse manifest: {error}"))
}

#[cfg(test)]
mod tests {
    use super::{
        build_healthcheck_url, check_event_log, check_manifest, find_nearest_manifest,
        format_trigger_metrics, read_manifest, DoctorStatus,
    };
    use harn_vm::llm_config::{AuthEnv, HealthcheckDef, ProviderDef};

    #[test]
    fn build_healthcheck_url_uses_base_and_path() {
        let def = ProviderDef {
            base_url: "https://example.com/api".to_string(),
            ..Default::default()
        };
        let healthcheck = HealthcheckDef {
            method: "GET".to_string(),
            path: Some("/health".to_string()),
            url: None,
            body: None,
        };

        assert_eq!(
            build_healthcheck_url(&def, &healthcheck),
            "https://example.com/api/health"
        );
    }

    #[test]
    fn find_nearest_manifest_walks_up() {
        let root = tempfile::tempdir().expect("tempdir");
        let nested = root.path().join("a/b/c");
        std::fs::create_dir_all(&nested).expect("create nested dirs");
        std::fs::write(
            root.path().join("harn.toml"),
            "[package]\nname = \"demo\"\n",
        )
        .expect("write manifest");

        let found = find_nearest_manifest(&nested).expect("manifest");
        assert_eq!(found, root.path().join("harn.toml"));
    }

    #[test]
    fn read_manifest_accepts_basic_package() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("harn.toml");
        std::fs::write(&path, "[package]\nname = \"demo\"\n").expect("write manifest");

        let manifest = read_manifest(&path).expect("manifest parses");
        assert_eq!(
            manifest.package.and_then(|pkg| pkg.name),
            Some("demo".to_string())
        );
    }

    #[test]
    fn auth_env_multiple_variant_exists_for_provider_checks() {
        let auth = AuthEnv::Multiple(vec!["FIRST".to_string(), "SECOND".to_string()]);
        let AuthEnv::Multiple(names) = auth else {
            panic!("expected multiple auth envs");
        };
        assert_eq!(names, vec!["FIRST".to_string(), "SECOND".to_string()]);
    }

    #[test]
    fn event_log_check_reports_backend_and_location() {
        let _state_guard = crate::tests::common::harn_state_lock::lock_harn_state();
        let dir = tempfile::tempdir().expect("tempdir");
        let sqlite_path = dir.path().join(".harn/events.sqlite");
        std::env::set_var(harn_vm::event_log::HARN_EVENT_LOG_BACKEND_ENV, "sqlite");
        std::env::set_var(
            harn_vm::event_log::HARN_EVENT_LOG_SQLITE_PATH_ENV,
            &sqlite_path,
        );
        let checks = check_event_log();
        std::env::remove_var(harn_vm::event_log::HARN_EVENT_LOG_BACKEND_ENV);
        std::env::remove_var(harn_vm::event_log::HARN_EVENT_LOG_SQLITE_PATH_ENV);
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].status, super::DoctorStatus::Ok);
        assert!(checks[0].detail.contains("sqlite"));
        assert!(checks[0]
            .detail
            .contains(&sqlite_path.display().to_string()));
    }

    #[test]
    fn format_trigger_metrics_renders_snapshot() {
        let rendered = format_trigger_metrics(&harn_vm::TriggerMetricsSnapshot {
            received: 1,
            dispatched: 2,
            failed: 3,
            dlq: 4,
            in_flight: 5,
            last_received_ms: None,
            cost_total_usd_micros: 0,
            cost_today_usd_micros: 0,
            cost_hour_usd_micros: 0,
        });
        assert_eq!(
            rendered,
            "received=1 dispatched=2 failed=3 dlq=4 in_flight=5"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn check_manifest_reports_loaded_triggers() {
        let _cwd_guard = crate::tests::common::cwd_lock::lock_cwd_async().await;
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join(".git")).expect("git dir");
        std::fs::write(
            dir.path().join("harn.toml"),
            r#"
[package]
name = "workspace"

[exports]
handlers = "lib.harn"

[[triggers]]
id = "github-new-issue"
kind = "webhook"
provider = "github"
match = { events = ["issues.opened"] }
handler = "handlers::on_new_issue"
budget = { daily_cost_usd = 5.0, max_concurrent = 10 }
secrets = { signing_secret = "github/webhook-secret" }
"#,
        )
        .expect("write manifest");
        std::fs::write(
            dir.path().join("lib.harn"),
            r#"
import "std/triggers"

pub fn on_new_issue(event: TriggerEvent) {
  log(event.kind)
}
"#,
        )
        .expect("write lib");

        let previous = std::env::current_dir().expect("cwd");
        std::env::set_current_dir(dir.path()).expect("set cwd");
        let checks = check_manifest().await;
        std::env::set_current_dir(previous).expect("restore cwd");

        let trigger = checks
            .iter()
            .find(|check| check.label == "trigger:github-new-issue")
            .expect("trigger check");
        assert_eq!(trigger.status, DoctorStatus::Ok);
        assert!(trigger.detail.contains("webhook via github"));
        assert!(trigger.detail.contains("handler=local"));
        assert!(trigger.detail.contains("state=active"));
        assert!(trigger.detail.contains("version=1"));
        assert!(trigger.detail.contains("metrics=received=0"));

        let dispatcher = checks
            .iter()
            .find(|check| check.label == "dispatcher")
            .expect("dispatcher check");
        assert_eq!(dispatcher.status, DoctorStatus::Ok);
        assert_eq!(
            dispatcher.detail,
            "in_flight=0 retry_queue_depth=0 dlq_depth=0"
        );
    }
}
