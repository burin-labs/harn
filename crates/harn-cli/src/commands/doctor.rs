use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use harn_vm::llm_config::{self, AuthEnv, ProviderDef};
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
    checks.extend(check_manifest());
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

fn check_manifest() -> Vec<DoctorCheck> {
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

    checks
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

        checks.push(run_healthcheck(&client, &provider_name, def, &auth, healthcheck).await);
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

fn read_manifest(path: &Path) -> Result<package::Manifest, String> {
    let content =
        fs::read_to_string(path).map_err(|error| format!("failed to read manifest: {error}"))?;
    toml::from_str::<package::Manifest>(&content)
        .map_err(|error| format!("failed to parse manifest: {error}"))
}

#[cfg(test)]
mod tests {
    use super::{build_healthcheck_url, find_nearest_manifest, read_manifest};
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
}
