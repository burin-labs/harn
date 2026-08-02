use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::sync::Arc;
use std::time::Duration as StdDuration;
use std::time::Instant;

use async_trait::async_trait;
use serde::Serialize;
use serde_json::{json, Value as JsonValue};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::cli::{ConnectorCheckArgs, PackageVerifyArgs};
use crate::package::{self, ConnectorContractFixture, ResolvedProviderConnectorKind};

mod connector_contract;
use connector_contract::check_one_connector;

pub(crate) const PACKAGE_VERIFY_SCHEMA_VERSION: u32 = 2;

pub(crate) async fn handle_package_verify(args: PackageVerifyArgs) -> Result<(), String> {
    let report = verify_package(&args).await;
    if args.json {
        let envelope = if report.status == "pass" {
            crate::json_envelope::JsonEnvelope::ok(PACKAGE_VERIFY_SCHEMA_VERSION, &report)
        } else {
            crate::json_envelope::JsonEnvelope::err(
                PACKAGE_VERIFY_SCHEMA_VERSION,
                "package_verification_failed",
                "package verification failed",
            )
            .with_details(serde_json::to_value(&report).map_err(|error| {
                format!("failed to render package verification receipt: {error}")
            })?)
        };
        let rendered = crate::json_envelope::to_string_pretty(&envelope);
        println!("{rendered}");
        if let Some(path) = args.receipt_out.as_deref() {
            write_receipt(path, &rendered)?;
        }
    } else {
        print_gate_report(&report);
        if let Some(path) = args.receipt_out.as_deref() {
            let envelope = if report.status == "pass" {
                crate::json_envelope::JsonEnvelope::ok(PACKAGE_VERIFY_SCHEMA_VERSION, &report)
            } else {
                crate::json_envelope::JsonEnvelope::err(
                    PACKAGE_VERIFY_SCHEMA_VERSION,
                    "package_verification_failed",
                    "package verification failed",
                )
                .with_details(serde_json::to_value(&report).map_err(|error| {
                    format!("failed to render package verification receipt: {error}")
                })?)
            };
            write_receipt(path, &crate::json_envelope::to_string_pretty(&envelope))?;
        }
    }
    if report.status == "pass" {
        Ok(())
    } else {
        Err("package verification failed".to_string())
    }
}

fn write_receipt(path: &Path, rendered: &str) -> Result<(), String> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    }
    fs::write(path, format!("{rendered}\n"))
        .map_err(|error| format!("failed to write {}: {error}", path.display()))
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ConnectorCheckReport {
    pub package: String,
    pub checked_connectors: Vec<CheckedConnector>,
    pub fixture_count: usize,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CheckedConnector {
    pub provider: String,
    pub module: String,
    pub kinds: Vec<String>,
    pub payload_schema: String,
    pub has_poll_tick: bool,
    pub fixtures: Vec<CheckedFixture>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CheckedFixture {
    pub name: String,
    pub result_type: String,
    pub event_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct PackageVerifyReport {
    pub package: String,
    pub package_kinds: Vec<String>,
    /// Whether this invocation requested the package-level strict policy.
    /// Per-file manifest strictness remains visible in each recorded command's
    /// outcome rather than being collapsed into one misleading package bit.
    pub strict_requested: bool,
    pub status: String,
    pub summary: PackageVerifySummary,
    pub checks: Vec<PackageVerifyCheck>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connector_contract: Option<ConnectorCheckReport>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct PackageVerifySummary {
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
    pub warnings: usize,
}

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct PackageVerifyCheck {
    pub name: String,
    pub applicable: bool,
    pub reached: bool,
    pub status: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub stdout: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub stderr: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub details: Vec<String>,
}

pub(crate) async fn check_connector_package(
    args: &ConnectorCheckArgs,
) -> Result<ConnectorCheckReport, String> {
    let _provider_schema_guard = package::lock_manifest_provider_schemas().await;
    let package = PathBuf::from(&args.package);
    let anchor = normalize_anchor(&package);
    let extensions = package::try_load_runtime_extensions(&anchor)?;
    package::install_manifest_provider_schemas(&extensions).await?;
    let manifest = extensions
        .root_manifest
        .as_ref()
        .ok_or_else(|| format!("no harn.toml found for {}", anchor.display()))?;
    let fixture_version = manifest.connector_contract.version.unwrap_or(1);
    if fixture_version != 1 {
        return Err(format!(
            "unsupported connector_contract.version {fixture_version}; expected 1"
        ));
    }

    let provider_filter = args.providers.iter().cloned().collect::<BTreeSet<_>>();
    let mut checked_connectors = Vec::new();
    let mut warnings = Vec::new();
    let mut failures = Vec::new();
    let mut fixture_count = 0usize;

    for provider in &extensions.provider_connectors {
        if !provider_filter.is_empty() && !provider_filter.contains(provider.id.as_str()) {
            continue;
        }

        let ResolvedProviderConnectorKind::Harn { module } = &provider.connector else {
            if matches!(
                provider.connector,
                ResolvedProviderConnectorKind::RustBuiltin
            ) {
                warnings.push(format!(
                    "skipped provider '{}' because it uses the Rust builtin connector",
                    provider.id.as_str()
                ));
            } else if let ResolvedProviderConnectorKind::Invalid(message) = &provider.connector {
                failures.push(message.clone());
            }
            continue;
        };
        validate_setup_metadata(provider.id.as_str(), provider.setup.as_ref(), &mut failures);

        match check_one_connector(
            provider.id.clone(),
            &provider.manifest_dir,
            module,
            &manifest.connector_contract.fixtures,
            args.run_poll_tick,
        )
        .await
        {
            Ok(checked) => {
                fixture_count += checked.fixtures.len();
                checked_connectors.push(checked);
            }
            Err(error) => failures.push(error),
        }
    }

    if !provider_filter.is_empty() {
        for provider in &provider_filter {
            if !extensions
                .provider_connectors
                .iter()
                .any(|config| config.id.as_str() == provider)
            {
                failures.push(format!(
                    "provider '{provider}' is not declared in harn.toml"
                ));
            }
        }
    }

    if checked_connectors.is_empty() && failures.is_empty() {
        failures.push(format!(
            "no pure-Harn connector providers found in {}",
            anchor.display()
        ));
    }
    if fixture_count == 0 {
        warnings.push("no connector_contract fixtures were declared; normalize_inbound shape was not exercised".to_string());
    }

    if failures.is_empty() {
        Ok(ConnectorCheckReport {
            package: anchor.display().to_string(),
            checked_connectors,
            fixture_count,
            warnings,
        })
    } else {
        Err(format!(
            "connector contract check failed:\n{}",
            failures
                .into_iter()
                .map(|failure| format!("- {failure}"))
                .collect::<Vec<_>>()
                .join("\n")
        ))
    }
}

pub(crate) async fn verify_package(args: &PackageVerifyArgs) -> PackageVerifyReport {
    let package = PathBuf::from(&args.package);
    let anchor = normalize_anchor(&package);
    let package_dir = package_dir_from_anchor(&package);
    let package_label = package_dir.display().to_string();
    let mut checks = Vec::new();
    let mut connector_contract = None;

    let (metadata, mut package_kinds) = validate_package_metadata(&anchor);
    let metadata_ok = metadata.status == "pass";
    checks.push(metadata);

    checks.push(run_harn_subcommand(
        "locked dependency install",
        &package_dir,
        &["install", "--locked"],
    ));

    let package_harn_files = package_harn_file_args(&package_dir);
    checks.push(run_package_harn_file_check(
        PackageSourceGate::Check,
        &package_dir,
        &package_harn_files,
        args.strict,
    ));
    checks.push(run_package_harn_file_check(
        PackageSourceGate::Lint,
        &package_dir,
        &package_harn_files,
        args.strict,
    ));
    let mut fmt_args = vec!["fmt".to_string(), "--check".to_string()];
    fmt_args.extend(package_harn_files.clone());
    checks.push(run_harn_subcommand_owned(
        "harn fmt --check",
        &package_dir,
        fmt_args,
    ));

    let connector_applicable = package::try_load_runtime_extensions(&anchor)
        .map(|extensions| !extensions.provider_connectors.is_empty())
        .unwrap_or(false);
    if connector_applicable {
        package_kinds.push("connector".to_string());
        let connector_metadata = validate_connector_package_metadata(&anchor);
        checks.push(connector_metadata);

        let check_args = ConnectorCheckArgs {
            package: args.package.clone(),
            providers: args.providers.clone(),
            run_poll_tick: args.run_poll_tick,
            json: false,
        };
        let started = Instant::now();
        match check_connector_package(&check_args).await {
            Ok(report) => {
                let details = report
                    .warnings
                    .iter()
                    .map(|warning| format!("warning: {warning}"))
                    .collect::<Vec<_>>();
                checks.push(PackageVerifyCheck {
                    name: "connector contract".to_string(),
                    applicable: true,
                    reached: true,
                    status: "pass".to_string(),
                    command: connector_check_command(&check_args),
                    exit_code: Some(0),
                    duration_ms: elapsed_ms(started),
                    stdout: String::new(),
                    stderr: String::new(),
                    details,
                });
                connector_contract = Some(report);
            }
            Err(error) => {
                checks.push(PackageVerifyCheck {
                    name: "connector contract".to_string(),
                    applicable: true,
                    reached: true,
                    status: "fail".to_string(),
                    command: connector_check_command(&check_args),
                    exit_code: Some(1),
                    duration_ms: elapsed_ms(started),
                    stdout: String::new(),
                    stderr: error,
                    details: Vec::new(),
                });
            }
        }
    } else {
        let mut details = vec!["package declares no pure-Harn connector providers".to_string()];
        if !args.providers.is_empty() || args.run_poll_tick {
            details.push("connector-only options require a connector package".to_string());
            checks.push(PackageVerifyCheck {
                name: "connector contract".to_string(),
                applicable: true,
                reached: false,
                status: "fail".to_string(),
                command: Vec::new(),
                exit_code: Some(1),
                duration_ms: 0,
                stdout: String::new(),
                stderr: "connector-only options require a connector package".to_string(),
                details,
            });
        } else {
            checks.push(skipped_check("connector contract", false, 0, details));
        }
    }

    checks.push(run_package_tests(&package_dir));
    checks.push(run_install_import_smoke(&package_dir, metadata_ok));
    checks.push(validate_doc_examples(&package_dir));
    checks.push(run_package_docs_check(&package_dir));
    checks.push(run_harn_subcommand(
        "package artifact dry run",
        &package_dir,
        &["package", "pack", "--dry-run"],
    ));

    let summary = summarize_gate_checks(&checks);
    let status = if summary.failed == 0 { "pass" } else { "fail" }.to_string();
    PackageVerifyReport {
        package: package_label,
        package_kinds,
        strict_requested: args.strict,
        status,
        summary,
        checks,
        connector_contract,
    }
}

fn validate_package_metadata(anchor: &Path) -> (PackageVerifyCheck, Vec<String>) {
    let started = Instant::now();
    match package::check_package_impl(Some(anchor)) {
        Ok(report) => {
            let mut details = Vec::new();
            details.extend(
                report
                    .warnings
                    .iter()
                    .map(|warning| format!("warning: {}: {}", warning.field, warning.message)),
            );
            details.push(format!("exports: {}", report.exports.len()));
            details.push(format!("tools: {}", report.tools.len()));
            details.push(format!("skills: {}", report.skills.len()));
            details.push(format!("personas: {}", report.personas.len()));
            let failures = report
                .errors
                .iter()
                .map(|error| format!("{}: {}", error.field, error.message))
                .collect::<Vec<_>>();
            let mut kinds = vec!["package".to_string()];
            if !report.tools.is_empty() {
                kinds.push("tool".to_string());
            }
            if !report.skills.is_empty() {
                kinds.push("skill".to_string());
            }
            if !report.personas.is_empty() {
                kinds.push("persona".to_string());
            }
            (
                gate_check_from_findings("package manifest", started, failures, details),
                kinds,
            )
        }
        Err(error) => (
            gate_check_from_findings(
                "package manifest",
                started,
                vec![error.to_string()],
                Vec::new(),
            ),
            vec!["package".to_string()],
        ),
    }
}

fn validate_connector_package_metadata(anchor: &Path) -> PackageVerifyCheck {
    let started = Instant::now();
    let mut details = Vec::new();
    let mut failures = Vec::new();
    let package_dir = package_dir_from_anchor(anchor);

    match package::try_load_runtime_extensions(anchor) {
        Ok(extensions) => {
            let Some(manifest) = extensions.root_manifest.as_ref() else {
                failures.push(format!("no harn.toml found for {}", anchor.display()));
                return gate_check_from_findings("package metadata", started, failures, details);
            };
            let package = manifest.package.as_ref();
            require_metadata_field(
                package.and_then(|package| package.name.as_deref()),
                "[package].name",
                &mut failures,
            );
            require_metadata_field(
                package.and_then(|package| package.version.as_deref()),
                "[package].version",
                &mut failures,
            );
            require_metadata_field(
                package.and_then(|package| package.description.as_deref()),
                "[package].description",
                &mut failures,
            );
            require_metadata_field(
                package.and_then(|package| package.license.as_deref()),
                "[package].license",
                &mut failures,
            );
            require_metadata_field(
                package.and_then(|package| package.repository.as_deref()),
                "[package].repository",
                &mut failures,
            );
            if manifest.exports.is_empty() {
                failures
                    .push("[exports] must expose at least one stable package module".to_string());
            }
            if extensions.provider_connectors.is_empty() {
                failures
                    .push("[[providers]] must declare at least one connector provider".to_string());
            }
            for provider in &extensions.provider_connectors {
                match &provider.connector {
                    ResolvedProviderConnectorKind::Harn { module } => details.push(format!(
                        "provider '{}' uses Harn connector module {}",
                        provider.id.as_str(),
                        module
                    )),
                    ResolvedProviderConnectorKind::RustBuiltin => failures.push(format!(
                        "provider '{}' uses a Rust builtin connector; connector packages must use connector.harn",
                        provider.id.as_str()
                    )),
                    ResolvedProviderConnectorKind::Invalid(message) => failures.push(message.clone()),
                }
                validate_setup_metadata(
                    provider.id.as_str(),
                    provider.setup.as_ref(),
                    &mut failures,
                );
            }
            if !package_dir.join("README.md").is_file() {
                failures.push("README.md is required".to_string());
            }
            if manifest.connector_contract.version.unwrap_or(1) != 1 {
                failures.push("connector_contract.version must be 1 when present".to_string());
            }
            details.push(format!("exports: {}", manifest.exports.len()));
            details.push(format!(
                "providers: {}",
                extensions.provider_connectors.len()
            ));
        }
        Err(error) => failures.push(error.to_string()),
    }

    gate_check_from_findings("package metadata", started, failures, details)
}

fn require_metadata_field(value: Option<&str>, field: &str, failures: &mut Vec<String>) {
    if value.is_none_or(|value| value.trim().is_empty()) {
        failures.push(format!("{field} is required"));
    }
}

fn validate_setup_metadata(
    provider_id: &str,
    setup: Option<&package::ProviderSetupManifest>,
    failures: &mut Vec<String>,
) {
    let Some(setup) = setup else {
        failures.push(format!(
            "provider '{provider_id}' must declare setup metadata"
        ));
        return;
    };
    if setup.auth_type.as_deref().is_none_or(str::is_empty) {
        failures.push(format!(
            "provider '{provider_id}' setup.auth_type is required"
        ));
    }
    if setup.flow.as_deref().is_none_or(str::is_empty) {
        failures.push(format!("provider '{provider_id}' setup.flow is required"));
    }
    if setup.setup_command.is_empty() {
        failures.push(format!(
            "provider '{provider_id}' setup.setup_command is required"
        ));
    }
    if setup.validation_command.is_empty() {
        failures.push(format!(
            "provider '{provider_id}' setup.validation_command is required"
        ));
    }
    if setup.health_checks.is_empty() {
        failures.push(format!(
            "provider '{provider_id}' setup.health_checks must include at least one health check"
        ));
    }
    for scope in &setup.required_scopes {
        if scope.trim().is_empty() {
            failures.push(format!(
                "provider '{provider_id}' setup.required_scopes cannot include empty values"
            ));
        }
    }
    for secret in &setup.required_secrets {
        if secret.split_once('/').is_none() {
            failures.push(format!(
                "provider '{provider_id}' setup.required_secrets entry '{secret}' must use namespace/name form"
            ));
        }
    }
    validate_recovery_copy(provider_id, &setup.recovery, failures);
    for check in &setup.health_checks {
        if check.id.trim().is_empty() {
            failures.push(format!(
                "provider '{provider_id}' setup health check id is required"
            ));
        }
        match check.kind.as_str() {
            "secret" if check.secret.as_deref().is_none_or(str::is_empty) => {
                failures.push(format!(
                    "provider '{provider_id}' secret health check '{}' must set secret",
                    check.id
                ));
            }
            "command" if check.command.is_empty() => {
                failures.push(format!(
                    "provider '{provider_id}' command health check '{}' must set command",
                    check.id
                ));
            }
            "http" | "mcp" | "resource" if check.url.as_deref().is_none_or(str::is_empty) => {
                failures.push(format!(
                    "provider '{provider_id}' {} health check '{}' must set url",
                    check.kind, check.id
                ));
            }
            "secret" | "command" | "http" | "mcp" | "resource" => {}
            other => failures.push(format!(
                "provider '{provider_id}' setup health check '{}' uses unsupported kind '{other}'",
                check.id
            )),
        }
    }
}

fn validate_recovery_copy(
    provider_id: &str,
    recovery: &package::ConnectorRecoveryCopy,
    failures: &mut Vec<String>,
) {
    let required = [
        ("missing_auth", recovery.missing_auth.as_deref()),
        (
            "expired_credentials",
            recovery.expired_credentials.as_deref(),
        ),
        (
            "revoked_credentials",
            recovery.revoked_credentials.as_deref(),
        ),
        ("missing_scopes", recovery.missing_scopes.as_deref()),
        (
            "inaccessible_resource",
            recovery.inaccessible_resource.as_deref(),
        ),
        (
            "transient_provider_outage",
            recovery.transient_provider_outage.as_deref(),
        ),
    ];
    for (field, value) in required {
        if value.is_none_or(|value| value.trim().is_empty()) {
            failures.push(format!(
                "provider '{provider_id}' setup.recovery.{field} is required"
            ));
        }
    }
}

fn gate_check_from_findings(
    name: &str,
    started: Instant,
    failures: Vec<String>,
    details: Vec<String>,
) -> PackageVerifyCheck {
    PackageVerifyCheck {
        name: name.to_string(),
        applicable: true,
        reached: true,
        status: if failures.is_empty() { "pass" } else { "fail" }.to_string(),
        command: Vec::new(),
        exit_code: if failures.is_empty() {
            Some(0)
        } else {
            Some(1)
        },
        duration_ms: elapsed_ms(started),
        stdout: String::new(),
        stderr: failures.join("\n"),
        details,
    }
}

fn skipped_check(
    name: &str,
    applicable: bool,
    duration_ms: u64,
    details: Vec<String>,
) -> PackageVerifyCheck {
    PackageVerifyCheck {
        name: name.to_string(),
        applicable,
        status: "skipped".to_string(),
        duration_ms,
        details,
        ..PackageVerifyCheck::default()
    }
}

fn run_harn_subcommand(name: &str, cwd: &Path, args: &[&str]) -> PackageVerifyCheck {
    run_harn_subcommand_owned(
        name,
        cwd,
        args.iter().map(|arg| (*arg).to_string()).collect(),
    )
}

fn run_harn_subcommand_owned(name: &str, cwd: &Path, args: Vec<String>) -> PackageVerifyCheck {
    let started = Instant::now();
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(error) => {
            return PackageVerifyCheck {
                name: name.to_string(),
                applicable: true,
                reached: false,
                status: "fail".to_string(),
                command: args,
                exit_code: None,
                duration_ms: elapsed_ms(started),
                stdout: String::new(),
                stderr: format!("failed to resolve current harn executable: {error}"),
                details: Vec::new(),
            };
        }
    };
    let output = ProcessCommand::new(&exe)
        .args(&args)
        .current_dir(cwd)
        .env("HARN_LLM_PROVIDER", "mock")
        .env(harn_vm::llm::LLM_CALLS_DISABLED_ENV, "1")
        .output();
    match output {
        Ok(output) => PackageVerifyCheck {
            name: name.to_string(),
            applicable: true,
            reached: true,
            status: if output.status.success() {
                "pass"
            } else {
                "fail"
            }
            .to_string(),
            command: std::iter::once(exe.display().to_string())
                .chain(args.iter().cloned())
                .collect(),
            exit_code: output.status.code(),
            duration_ms: elapsed_ms(started),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            details: Vec::new(),
        },
        Err(error) => PackageVerifyCheck {
            name: name.to_string(),
            applicable: true,
            reached: false,
            status: "fail".to_string(),
            command: std::iter::once(exe.display().to_string())
                .chain(args)
                .collect(),
            exit_code: None,
            duration_ms: elapsed_ms(started),
            stdout: String::new(),
            stderr: format!("failed to run command: {error}"),
            details: Vec::new(),
        },
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PackageSourceGate {
    Check,
    Lint,
}

impl PackageSourceGate {
    fn name(self) -> &'static str {
        match self {
            Self::Check => "harn check",
            Self::Lint => "harn lint",
        }
    }

    fn command(self, package_harn_files: &[String], strict: bool) -> Vec<String> {
        let (subcommand, strict_flags): (&str, &[&str]) = match self {
            Self::Check => ("check", &["--strict", "--strict-types"]),
            Self::Lint => ("lint", &["--strict"]),
        };
        let mut args = vec![subcommand.to_string()];
        if strict {
            args.extend(strict_flags.iter().map(|flag| (*flag).to_string()));
        }
        args.extend(package_harn_files.iter().cloned());
        args
    }
}

fn run_package_harn_file_check(
    gate: PackageSourceGate,
    package_dir: &Path,
    package_harn_files: &[String],
    strict: bool,
) -> PackageVerifyCheck {
    if package_harn_files.is_empty() {
        return PackageVerifyCheck {
            name: gate.name().to_string(),
            applicable: true,
            reached: false,
            status: "fail".to_string(),
            command: Vec::new(),
            exit_code: Some(1),
            duration_ms: 0,
            stdout: String::new(),
            stderr: "no package-owned .harn files found".to_string(),
            details: Vec::new(),
        };
    }
    run_harn_subcommand_owned(
        gate.name(),
        package_dir,
        gate.command(package_harn_files, strict),
    )
}

fn package_harn_file_args(package_dir: &Path) -> Vec<String> {
    let mut files = Vec::new();
    collect_package_harn_files(package_dir, package_dir, &mut files);
    files
        .into_iter()
        .filter_map(|path| {
            path.strip_prefix(package_dir)
                .ok()
                .map(|rel| rel.to_string_lossy().into_owned())
        })
        .collect()
}

fn collect_package_harn_files(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut entries = entries.filter_map(Result::ok).collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.path());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            if should_skip_package_input_directory(root, &path) {
                continue;
            }
            collect_package_harn_files(root, &path, out);
        } else if path.extension().is_some_and(|ext| ext == "harn") {
            out.push(path);
        }
    }
}

fn should_skip_package_input_directory(root: &Path, path: &Path) -> bool {
    let relative = path.strip_prefix(root).unwrap_or(path);
    package::should_exclude_package_entry(relative, package::PathEntryKind::Directory)
}

fn run_package_tests(package_dir: &Path) -> PackageVerifyCheck {
    let started = Instant::now();
    let tests = package_test_files(package_dir);
    if tests.is_empty() {
        return skipped_check(
            "package tests",
            false,
            elapsed_ms(started),
            vec!["no runnable tests/*.harn files found".to_string()],
        );
    }
    let mut check = run_harn_subcommand(
        "package tests",
        package_dir,
        &["test", "tests/", "--parallel"],
    );
    check
        .details
        .push(format!("discovered {} runnable test file(s)", tests.len()));
    check.duration_ms = elapsed_ms(started);
    check
}

fn package_test_files(package_dir: &Path) -> Vec<PathBuf> {
    let tests_dir = package_dir.join("tests");
    let mut files = match fs::read_dir(&tests_dir) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "harn"))
            .filter(|path| {
                fs::read_to_string(path)
                    .map(|source| source.contains("pipeline ") || source.contains("@test"))
                    .unwrap_or(false)
            })
            .collect::<Vec<_>>(),
        Err(_) => Vec::new(),
    };
    files.sort();
    files
}

fn run_install_import_smoke(package_dir: &Path, metadata_ok: bool) -> PackageVerifyCheck {
    let started = Instant::now();
    if !metadata_ok {
        return skipped_check(
            "package install/import smoke",
            true,
            elapsed_ms(started),
            vec!["skipped because package metadata did not pass".to_string()],
        );
    }
    let package_dependency_path = match package_dependency_path(package_dir) {
        Ok(path) => path,
        Err(message) => {
            return gate_check_from_findings(
                "package install/import smoke",
                started,
                vec![message],
                Vec::new(),
            );
        }
    };
    let package_dir = PathBuf::from(&package_dependency_path);
    let manifest_path = package_dir.join("harn.toml");
    let manifest_source = match fs::read_to_string(&manifest_path) {
        Ok(source) => source,
        Err(error) => {
            return gate_check_from_findings(
                "package install/import smoke",
                started,
                vec![format!(
                    "failed to read {}: {error}",
                    manifest_path.display()
                )],
                Vec::new(),
            );
        }
    };
    let manifest = match toml::from_str::<package::Manifest>(&manifest_source) {
        Ok(manifest) => manifest,
        Err(error) => {
            return gate_check_from_findings(
                "package install/import smoke",
                started,
                vec![format!(
                    "failed to parse {}: {error}",
                    manifest_path.display()
                )],
                Vec::new(),
            );
        }
    };
    let Some(package_name) = manifest
        .package
        .as_ref()
        .and_then(|package| package.name.as_deref())
        .map(str::to_string)
    else {
        return gate_check_from_findings(
            "package install/import smoke",
            started,
            vec!["[package].name is required for install/import smoke".to_string()],
            Vec::new(),
        );
    };
    if manifest.exports.is_empty() {
        return skipped_check(
            "package install/import smoke",
            false,
            elapsed_ms(started),
            vec![
                "package has no module exports; contribution and rule surfaces are verified by their owning gates"
                    .to_string(),
            ],
        );
    }

    let temp = match tempfile::tempdir() {
        Ok(temp) => temp,
        Err(error) => {
            return gate_check_from_findings(
                "package install/import smoke",
                started,
                vec![format!(
                    "failed to create temporary consumer package: {error}"
                )],
                Vec::new(),
            );
        }
    };
    let consumer = temp.path();
    let manifest = format!(
        "[package]\nname = \"connector-smoke-consumer\"\nversion = \"0.0.0\"\n\n[dependencies]\n{} = {{ path = {} }}\n",
        crate::format::toml_basic_string_literal(&package_name),
        crate::format::toml_basic_string_literal(&package_dependency_path)
    );
    if let Err(error) = fs::write(consumer.join("harn.toml"), manifest) {
        return gate_check_from_findings(
            "package install/import smoke",
            started,
            vec![format!("failed to write consumer harn.toml: {error}")],
            Vec::new(),
        );
    }
    let install = run_harn_subcommand("harn install", consumer, &["install"]);
    if install.status != "pass" {
        return PackageVerifyCheck {
            name: "package install/import smoke".to_string(),
            applicable: true,
            reached: true,
            status: "fail".to_string(),
            command: install.command,
            exit_code: install.exit_code,
            duration_ms: elapsed_ms(started),
            stdout: install.stdout,
            stderr: install.stderr,
            details: vec!["consumer package install failed".to_string()],
        };
    }

    let mut details = vec!["consumer package install passed".to_string()];
    let mut failures = Vec::new();
    let mut exports = manifest_exports_sorted(&manifest_source);
    exports.sort();
    for export in exports {
        let smoke_path = consumer.join(format!("smoke-{export}.harn"));
        let source = format!(
            "import \"{package_name}/{export}\"\n\npipeline default(harness: Harness) {{\n}}\n"
        );
        if let Err(error) = fs::write(&smoke_path, source) {
            failures.push(format!("failed to write {}: {error}", smoke_path.display()));
            continue;
        }
        let rel = smoke_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("smoke.harn");
        let check = run_harn_subcommand(
            &format!("harn check {package_name}/{export}"),
            consumer,
            &["check", rel],
        );
        if check.status == "pass" {
            details.push(format!("import \"{package_name}/{export}\": pass"));
        } else {
            failures.push(format!(
                "import \"{package_name}/{export}\" failed\nstdout:\n{}\nstderr:\n{}",
                check.stdout, check.stderr
            ));
        }
    }

    gate_check_from_findings("package install/import smoke", started, failures, details)
}

fn manifest_exports_sorted(manifest_source: &str) -> Vec<String> {
    toml::from_str::<package::Manifest>(manifest_source)
        .map(|manifest| manifest.exports.keys().cloned().collect())
        .unwrap_or_default()
}

fn package_dependency_path(package_dir: &Path) -> Result<String, String> {
    package_dir
        .canonicalize()
        .map(|path| path.display().to_string())
        .map_err(|error| {
            format!(
                "failed to canonicalize package directory {}: {error}",
                package_dir.display()
            )
        })
}

fn validate_doc_examples(package_dir: &Path) -> PackageVerifyCheck {
    let started = Instant::now();
    let mut details = Vec::new();
    let mut failures = Vec::new();
    let mut markdown_files = Vec::new();
    collect_markdown_files(package_dir, package_dir, &mut markdown_files);
    for markdown in markdown_files {
        let Ok(source) = fs::read_to_string(&markdown) else {
            continue;
        };
        for (idx, block) in harn_doc_blocks(&source).into_iter().enumerate() {
            if !is_standalone_harn_doc_example(&block) {
                details.push(format!(
                    "{} harn block {}: skipped non-standalone snippet",
                    markdown.display(),
                    idx + 1
                ));
                continue;
            }
            match harn_parser::parse_source(&block) {
                Ok(_) => details.push(format!(
                    "{} harn block {}: parsed",
                    markdown.display(),
                    idx + 1
                )),
                Err(error) => failures.push(format!(
                    "{} harn block {} failed to parse: {error}",
                    markdown.display(),
                    idx + 1
                )),
            }
        }
    }
    if details.is_empty() && failures.is_empty() {
        return skipped_check(
            "doc examples",
            false,
            elapsed_ms(started),
            vec!["no Markdown harn examples found".to_string()],
        );
    }
    gate_check_from_findings("doc examples", started, failures, details)
}

fn run_package_docs_check(package_dir: &Path) -> PackageVerifyCheck {
    if !package_dir.join("docs/api.md").is_file() {
        return skipped_check(
            "generated API docs",
            false,
            0,
            vec!["docs/api.md is not part of this package".to_string()],
        );
    }
    run_harn_subcommand(
        "generated API docs",
        package_dir,
        &["package", "docs", "--check"],
    )
}

fn collect_markdown_files(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut entries = entries.filter_map(Result::ok).collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.path());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            if should_skip_package_input_directory(root, &path) {
                continue;
            }
            collect_markdown_files(root, &path, out);
        } else if path.extension().is_some_and(|ext| ext == "md") {
            out.push(path);
        }
    }
}

fn harn_doc_blocks(markdown: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current = Vec::new();
    let mut in_harn = false;
    for line in markdown.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            if in_harn {
                blocks.push(current.join("\n"));
                current.clear();
                in_harn = false;
            } else {
                let language = trimmed.trim_start_matches("```").trim();
                in_harn = language == "harn";
            }
            continue;
        }
        if in_harn {
            current.push(line);
        }
    }
    blocks
}

fn is_standalone_harn_doc_example(source: &str) -> bool {
    source.contains("pipeline ")
        || (source.contains('{') && (source.contains("pub fn ") || source.contains("\nfn ")))
}

fn connector_check_command(args: &ConnectorCheckArgs) -> Vec<String> {
    let mut command = vec![
        "harn".to_string(),
        "connector".to_string(),
        "check".to_string(),
        args.package.clone(),
    ];
    for provider in &args.providers {
        command.push("--provider".to_string());
        command.push(provider.clone());
    }
    if args.run_poll_tick {
        command.push("--run-poll-tick".to_string());
    }
    command
}

fn summarize_gate_checks(checks: &[PackageVerifyCheck]) -> PackageVerifySummary {
    let mut summary = PackageVerifySummary::default();
    for check in checks {
        match check.status.as_str() {
            "pass" => summary.passed += 1,
            "fail" => summary.failed += 1,
            "skipped" => summary.skipped += 1,
            _ => {}
        }
        summary.warnings += check
            .details
            .iter()
            .filter(|detail| detail.starts_with("warning:"))
            .count();
    }
    summary
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis() as u64
}

fn normalize_anchor(path: &Path) -> PathBuf {
    if path.is_dir() {
        path.join("harn.toml")
    } else {
        path.to_path_buf()
    }
}

fn package_dir_from_anchor(path: &Path) -> PathBuf {
    let start = if path.is_dir() {
        path.to_path_buf()
    } else {
        path.parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
    };
    for dir in start.ancestors() {
        if dir.join("harn.toml").is_file() {
            return dir.to_path_buf();
        }
    }
    start
}

pub(crate) fn print_connector_report(report: &ConnectorCheckReport) {
    println!(
        "Connector contract check passed for {} connector(s), {} fixture(s).",
        report.checked_connectors.len(),
        report.fixture_count
    );
    for connector in &report.checked_connectors {
        println!(
            "- {}: kinds=[{}], schema={}, fixtures={}",
            connector.provider,
            connector.kinds.join(", "),
            connector.payload_schema,
            connector.fixtures.len()
        );
    }
    for warning in &report.warnings {
        eprintln!("warning: {warning}");
    }
}

fn print_gate_report(report: &PackageVerifyReport) {
    println!("{}", gate_report_header(report));
    for check in &report.checks {
        println!(
            "- {}: {} (applicable={}, reached={})",
            check.name, check.status, check.applicable, check.reached
        );
        for detail in &check.details {
            println!("  {detail}");
        }
        if !check.stderr.trim().is_empty() {
            eprintln!(
                "{} {}:\n{}",
                check.name,
                gate_stderr_label(check),
                check.stderr.trim()
            );
        }
    }
}

fn gate_report_header(report: &PackageVerifyReport) -> String {
    format!(
        "Package verification {} for {} ({}, strict_requested={}): {} passed, {} failed, {} skipped.",
        report.status,
        report.package,
        report.package_kinds.join(", "),
        report.strict_requested,
        report.summary.passed,
        report.summary.failed,
        report.summary.skipped
    )
}

fn gate_stderr_label(check: &PackageVerifyCheck) -> &'static str {
    if check.status == "fail" {
        "failed output"
    } else {
        "diagnostics"
    }
}

#[cfg(test)]
#[path = "package_verify/tests.rs"]
mod tests;
