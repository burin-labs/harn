use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

use serde_json::Value as JsonValue;

use crate::cli::{ConnectSetupPlanArgs, ConnectStatusArgs};
use crate::package::{self, ConnectorRecoveryCopy};
use harn_vm::secrets::SecretProvider;

use super::store::{
    connect_secret_provider, current_unix_timestamp, load_connect_index, parse_secret_id,
};
use super::{
    ConnectIndex, ConnectSetupPlan, ConnectSetupStep, ConnectStatusReport, ConnectorHealthStatus,
    ConnectorStatus,
};

pub(super) async fn run_connect_status(args: &ConnectStatusArgs) -> Result<(), String> {
    let report = connect_status_report(args).await?;
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report)
                .map_err(|error| format!("failed to encode JSON output: {error}"))?
        );
    } else if report.connectors.is_empty() {
        println!("No connector providers declared in the nearest harn.toml.");
    } else {
        for status in &report.connectors {
            println!(
                "{}\t{}\tusable={}\t{}",
                status.id, status.status, status.usable, status.reason
            );
        }
    }
    Ok(())
}

pub(super) fn run_connect_setup_plan(args: &ConnectSetupPlanArgs) -> Result<(), String> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let plan = connect_setup_plan_at(&args.connector, &cwd)?;
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&plan)
                .map_err(|error| format!("failed to encode JSON output: {error}"))?
        );
    } else {
        println!("Connector: {}", plan.connector);
        println!("Installed: {}", plan.installed);
        if let Some(auth_type) = plan.auth_type.as_deref() {
            println!("Auth: {auth_type}");
        }
        for step in &plan.steps {
            println!("- {}", step.title);
            if !step.command.is_empty() {
                println!("  {}", step.command.join(" "));
            }
            if !step.body.is_empty() {
                println!("  {}", step.body);
            }
        }
    }
    Ok(())
}

pub(super) async fn connect_status_report(
    args: &ConnectStatusArgs,
) -> Result<ConnectStatusReport, String> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let extensions = package::try_load_runtime_extensions(&cwd)?;
    let provider = connect_secret_provider()?;
    let (index, credential_backend_error) = match load_connect_index(&provider).await {
        Ok(index) => (index, None),
        Err(error) => (ConnectIndex::default(), Some(error)),
    };
    let now = current_unix_timestamp();
    let manifest = extensions
        .root_manifest_path
        .as_ref()
        .map(|path| path.display().to_string());

    let connectors = if let Some(connector) = args.connector.as_deref() {
        let config = extensions
            .provider_connectors
            .iter()
            .find(|entry| entry.id.as_str() == connector);
        vec![
            connector_status(
                connector,
                config,
                &provider,
                &index,
                now,
                args.run_health_checks,
                credential_backend_error.as_deref(),
            )
            .await,
        ]
    } else {
        let mut connectors = Vec::new();
        for config in &extensions.provider_connectors {
            connectors.push(
                connector_status(
                    config.id.as_str(),
                    Some(config),
                    &provider,
                    &index,
                    now,
                    args.run_health_checks,
                    credential_backend_error.as_deref(),
                )
                .await,
            );
        }
        connectors.sort_by(|left, right| left.id.cmp(&right.id));
        connectors
    };

    Ok(ConnectStatusReport {
        schema_version: 1,
        manifest,
        connectors,
    })
}

pub(super) async fn connector_status(
    connector_id: &str,
    config: Option<&package::ResolvedProviderConnectorConfig>,
    provider: &dyn SecretProvider,
    index: &ConnectIndex,
    now: i64,
    run_health_checks: bool,
    credential_backend_error: Option<&str>,
) -> ConnectorStatus {
    let Some(config) = config else {
        return missing_install_status(connector_id);
    };
    let setup = config.setup.clone().unwrap_or_default();
    let auth_type = setup.auth_type.clone();
    let flow = setup.flow.clone();
    let recovery = setup.recovery.clone();
    let required_scopes = setup.required_scopes.clone();
    let required_secrets = setup.required_secrets.clone();
    let entry = index
        .providers
        .iter()
        .find(|entry| entry.provider == connector_id);
    let mut health_checks = Vec::new();
    let mut missing_secrets = Vec::new();
    let mut status = "healthy".to_string();
    let mut reason = "connector is installed and credentials are present".to_string();

    if let Some(error) = credential_backend_error {
        status = "transient_provider_outage".to_string();
        reason = format!("credential backend was unavailable: {error}");
    }

    if status == "healthy"
        && auth_type.as_deref().is_some_and(|kind| kind != "none")
        && entry.is_none()
        && required_secrets.is_empty()
    {
        status = "missing_auth".to_string();
        reason = format!("no stored credentials found for connector '{connector_id}'");
    }

    let secret_id = entry.map(|entry| entry.secret_id.clone());
    let expires_at_unix = entry.and_then(|entry| entry.expires_at_unix);
    if status == "healthy" {
        if let Some(expires_at) = expires_at_unix {
            if expires_at <= now {
                status = "expired_credentials".to_string();
                reason = "stored credentials are expired".to_string();
            }
        }
    }
    if status == "healthy" {
        if let Some(entry) = entry {
            match parse_secret_id(&entry.secret_id) {
                Some(id) => match provider.get(&id).await {
                    Ok(_) => {}
                    Err(harn_vm::secrets::SecretError::NotFound { .. }) => {
                        status = "revoked_credentials".to_string();
                        reason = format!(
                            "credential index points at missing secret '{}'",
                            entry.secret_id
                        );
                    }
                    Err(error) => {
                        status = "transient_provider_outage".to_string();
                        reason = format!("credential backend was unavailable: {error}");
                    }
                },
                None => {
                    status = "revoked_credentials".to_string();
                    reason = format!(
                        "credential index contains invalid secret id '{}'",
                        entry.secret_id
                    );
                }
            }
        }
    }

    if status == "healthy" {
        for secret in &required_secrets {
            match parse_secret_id(secret) {
                Some(id) => match provider.get(&id).await {
                    Ok(_) => health_checks.push(ConnectorHealthStatus {
                        id: format!("secret:{secret}"),
                        kind: "secret".to_string(),
                        status: "pass".to_string(),
                        detail: String::new(),
                    }),
                    Err(harn_vm::secrets::SecretError::NotFound { .. }) => {
                        missing_secrets.push(secret.clone());
                        health_checks.push(ConnectorHealthStatus {
                            id: format!("secret:{secret}"),
                            kind: "secret".to_string(),
                            status: "fail".to_string(),
                            detail: "missing secret".to_string(),
                        });
                    }
                    Err(error) => {
                        status = "transient_provider_outage".to_string();
                        reason = format!("credential backend was unavailable: {error}");
                        health_checks.push(ConnectorHealthStatus {
                            id: format!("secret:{secret}"),
                            kind: "secret".to_string(),
                            status: "fail".to_string(),
                            detail: error.to_string(),
                        });
                    }
                },
                None => {
                    missing_secrets.push(secret.clone());
                    health_checks.push(ConnectorHealthStatus {
                        id: format!("secret:{secret}"),
                        kind: "secret".to_string(),
                        status: "fail".to_string(),
                        detail: "invalid secret id".to_string(),
                    });
                }
            }
        }
        if !missing_secrets.is_empty() && status == "healthy" {
            status = "missing_auth".to_string();
            reason = format!("missing required secrets: {}", missing_secrets.join(", "));
        }
    }

    let missing_scopes = missing_required_scopes(
        &required_scopes,
        entry.and_then(|entry| entry.scopes.as_deref()),
    );
    if status == "healthy" && !missing_scopes.is_empty() {
        status = "missing_scopes".to_string();
        reason = format!(
            "stored credentials are missing scopes: {}",
            missing_scopes.join(", ")
        );
    }

    if run_health_checks && status == "healthy" {
        for check in &setup.health_checks {
            let check_status = run_manifest_health_check(check);
            if check_status.status == "inaccessible_resource"
                || check_status.status == "transient_provider_outage"
            {
                status = check_status.status.clone();
                reason = check_status.detail.clone();
            }
            health_checks.push(check_status);
        }
    }

    ConnectorStatus {
        id: connector_id.to_string(),
        installed: true,
        usable: status == "healthy",
        status,
        reason,
        auth_type,
        flow,
        required_scopes,
        missing_scopes,
        required_secrets,
        missing_secrets,
        secret_id,
        expires_at_unix,
        health_checks,
        recovery,
    }
}

pub(super) fn missing_install_status(connector_id: &str) -> ConnectorStatus {
    ConnectorStatus {
        id: connector_id.to_string(),
        installed: false,
        usable: false,
        status: "missing_install".to_string(),
        reason: format!("connector '{connector_id}' is not declared in the nearest harn.toml"),
        auth_type: None,
        flow: None,
        required_scopes: Vec::new(),
        missing_scopes: Vec::new(),
        required_secrets: Vec::new(),
        missing_secrets: Vec::new(),
        secret_id: None,
        expires_at_unix: None,
        health_checks: Vec::new(),
        recovery: ConnectorRecoveryCopy {
            missing_install: Some(format!(
                "Add a package or [[providers]] entry for connector '{connector_id}'."
            )),
            ..ConnectorRecoveryCopy::default()
        },
    }
}

pub(super) fn connect_setup_plan_at(
    connector_id: &str,
    cwd: &Path,
) -> Result<ConnectSetupPlan, String> {
    let extensions = package::try_load_runtime_extensions(cwd)?;
    let manifest = extensions
        .root_manifest_path
        .as_ref()
        .map(|path| path.display().to_string());
    let Some(config) = extensions
        .provider_connectors
        .iter()
        .find(|entry| entry.id.as_str() == connector_id)
    else {
        return Ok(ConnectSetupPlan {
            schema_version: 1,
            connector: connector_id.to_string(),
            installed: false,
            manifest,
            auth_type: None,
            flow: None,
            required_scopes: Vec::new(),
            required_secrets: Vec::new(),
            setup_command: Vec::new(),
            validation_command: vec![
                "harn".to_string(),
                "connect".to_string(),
                "status".to_string(),
                "--connector".to_string(),
                connector_id.to_string(),
                "--json".to_string(),
            ],
            health_checks: Vec::new(),
            recovery: ConnectorRecoveryCopy {
                missing_install: Some(format!(
                    "Install a package that declares [[providers]] id = \"{connector_id}\"."
                )),
                ..ConnectorRecoveryCopy::default()
            },
            steps: vec![ConnectSetupStep {
                id: "install".to_string(),
                title: "Install connector package".to_string(),
                command: Vec::new(),
                body: format!(
                    "Add a dependency and [[providers]] entry for connector '{connector_id}'."
                ),
            }],
        });
    };
    let setup = config.setup.clone().unwrap_or_default();
    let mut steps = Vec::new();
    if let Some(copy) = setup.recovery.missing_install.as_ref() {
        steps.push(ConnectSetupStep {
            id: "install".to_string(),
            title: "Install connector package".to_string(),
            command: Vec::new(),
            body: copy.clone(),
        });
    }
    steps.push(ConnectSetupStep {
        id: "authorize".to_string(),
        title: "Authorize connector".to_string(),
        command: setup.setup_command.clone(),
        body: setup.recovery.missing_auth.clone().unwrap_or_default(),
    });
    steps.push(ConnectSetupStep {
        id: "validate".to_string(),
        title: "Validate connector status".to_string(),
        command: setup.validation_command.clone(),
        body: String::new(),
    });

    Ok(ConnectSetupPlan {
        schema_version: 1,
        connector: connector_id.to_string(),
        installed: true,
        manifest,
        auth_type: setup.auth_type,
        flow: setup.flow,
        required_scopes: setup.required_scopes,
        required_secrets: setup.required_secrets,
        setup_command: setup.setup_command,
        validation_command: setup.validation_command,
        health_checks: setup.health_checks,
        recovery: setup.recovery,
        steps,
    })
}

pub(super) fn missing_required_scopes(required: &[String], actual: Option<&str>) -> Vec<String> {
    let actual = actual
        .unwrap_or_default()
        .split(|ch: char| ch.is_ascii_whitespace() || ch == ',')
        .filter(|scope| !scope.trim().is_empty())
        .map(str::trim)
        .collect::<BTreeSet<_>>();
    required
        .iter()
        .filter(|scope| !actual.contains(scope.trim()))
        .cloned()
        .collect()
}

pub(super) fn run_manifest_health_check(
    check: &package::ConnectorHealthCheckManifest,
) -> ConnectorHealthStatus {
    if check.kind == "secret" {
        return ConnectorHealthStatus {
            id: check.id.clone(),
            kind: check.kind.clone(),
            status: "skipped".to_string(),
            detail: "secret checks are evaluated before command health checks".to_string(),
        };
    }
    if check.kind != "command" {
        return ConnectorHealthStatus {
            id: check.id.clone(),
            kind: check.kind.clone(),
            status: "skipped".to_string(),
            detail: "only command health checks are executable by harn connect status".to_string(),
        };
    }
    let Some((program, args)) = check.command.split_first() else {
        return ConnectorHealthStatus {
            id: check.id.clone(),
            kind: check.kind.clone(),
            status: "transient_provider_outage".to_string(),
            detail: "command health check is empty".to_string(),
        };
    };
    let output = ProcessCommand::new(program).args(args).output();
    match output {
        Ok(output) if output.status.success() => ConnectorHealthStatus {
            id: check.id.clone(),
            kind: check.kind.clone(),
            status: "pass".to_string(),
            detail: String::from_utf8_lossy(&output.stdout).trim().to_string(),
        },
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let declared_status = serde_json::from_str::<JsonValue>(stdout.trim())
                .ok()
                .and_then(|value| {
                    value
                        .get("status")
                        .and_then(JsonValue::as_str)
                        .map(str::to_string)
                })
                .filter(|status| {
                    matches!(
                        status.as_str(),
                        "inaccessible_resource" | "transient_provider_outage"
                    )
                })
                .unwrap_or_else(|| "transient_provider_outage".to_string());
            ConnectorHealthStatus {
                id: check.id.clone(),
                kind: check.kind.clone(),
                status: declared_status,
                detail: if stderr.trim().is_empty() {
                    stdout.trim().to_string()
                } else {
                    stderr.trim().to_string()
                },
            }
        }
        Err(error) => ConnectorHealthStatus {
            id: check.id.clone(),
            kind: check.kind.clone(),
            status: "transient_provider_outage".to_string(),
            detail: format!("failed to run health check command: {error}"),
        },
    }
}
