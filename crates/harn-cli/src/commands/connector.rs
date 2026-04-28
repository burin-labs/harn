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

use crate::cli::{ConnectorArgs, ConnectorCheckArgs, ConnectorCommand, ConnectorTestArgs};
use crate::package::{self, ConnectorContractFixture, ResolvedProviderConnectorKind};

pub(crate) async fn handle_connector_command(args: ConnectorArgs) -> Result<(), String> {
    match args.command {
        ConnectorCommand::Check(check) => {
            let report = check_connector_package(&check).await?;
            if check.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report)
                        .map_err(|error| format!("failed to render connector report: {error}"))?
                );
            } else {
                print_human_report(&report);
            }
            Ok(())
        }
        ConnectorCommand::Test(test) => {
            let report = test_connector_package(&test).await;
            if test.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report).map_err(|error| format!(
                        "failed to render connector gate report: {error}"
                    ))?
                );
            } else {
                print_gate_report(&report);
            }
            if report.status == "pass" {
                Ok(())
            } else {
                Err("connector package gate failed".to_string())
            }
        }
    }
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
pub(crate) struct ConnectorGateReport {
    pub package: String,
    pub status: String,
    pub summary: ConnectorGateSummary,
    pub checks: Vec<ConnectorGateCheck>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connector_contract: Option<ConnectorCheckReport>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct ConnectorGateSummary {
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
    pub warnings: usize,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ConnectorGateCheck {
    pub name: String,
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

pub(crate) async fn test_connector_package(args: &ConnectorTestArgs) -> ConnectorGateReport {
    let package = PathBuf::from(&args.package);
    let anchor = normalize_anchor(&package);
    let package_dir = package_dir_from_anchor(&package);
    let package_label = package_dir.display().to_string();
    let mut checks = Vec::new();
    let mut connector_contract = None;

    let metadata = validate_connector_package_metadata(&anchor);
    let metadata_ok = metadata.status == "pass";
    checks.push(metadata);

    let package_harn_files = package_harn_file_args(&package_dir);
    checks.push(run_package_harn_file_check(
        "harn check",
        &package_dir,
        "check",
        &package_harn_files,
    ));
    checks.push(run_package_harn_file_check(
        "harn lint",
        &package_dir,
        "lint",
        &package_harn_files,
    ));
    let mut fmt_args = vec!["fmt".to_string(), "--check".to_string()];
    fmt_args.extend(package_harn_files.clone());
    checks.push(run_harn_subcommand_owned(
        "harn fmt --check",
        &package_dir,
        fmt_args,
    ));

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
            checks.push(ConnectorGateCheck {
                name: "connector contract".to_string(),
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
            checks.push(ConnectorGateCheck {
                name: "connector contract".to_string(),
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

    checks.push(run_connector_fixture_tests(&package_dir));
    checks.push(run_install_import_smoke(&package_dir, metadata_ok));
    checks.push(validate_doc_examples(&package_dir));

    let summary = summarize_gate_checks(&checks);
    let status = if summary.failed == 0 { "pass" } else { "fail" }.to_string();
    ConnectorGateReport {
        package: package_label,
        status,
        summary,
        checks,
        connector_contract,
    }
}

fn validate_connector_package_metadata(anchor: &Path) -> ConnectorGateCheck {
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
        Err(error) => failures.push(error),
    }

    gate_check_from_findings("package metadata", started, failures, details)
}

fn require_metadata_field(value: Option<&str>, field: &str, failures: &mut Vec<String>) {
    if value.is_none_or(|value| value.trim().is_empty()) {
        failures.push(format!("{field} is required"));
    }
}

fn gate_check_from_findings(
    name: &str,
    started: Instant,
    failures: Vec<String>,
    details: Vec<String>,
) -> ConnectorGateCheck {
    ConnectorGateCheck {
        name: name.to_string(),
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

fn run_harn_subcommand(name: &str, cwd: &Path, args: &[&str]) -> ConnectorGateCheck {
    run_harn_subcommand_owned(
        name,
        cwd,
        args.iter().map(|arg| (*arg).to_string()).collect(),
    )
}

fn run_harn_subcommand_owned(name: &str, cwd: &Path, args: Vec<String>) -> ConnectorGateCheck {
    let started = Instant::now();
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(error) => {
            return ConnectorGateCheck {
                name: name.to_string(),
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
        Ok(output) => ConnectorGateCheck {
            name: name.to_string(),
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
        Err(error) => ConnectorGateCheck {
            name: name.to_string(),
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

fn run_package_harn_file_check(
    name: &str,
    package_dir: &Path,
    subcommand: &str,
    package_harn_files: &[String],
) -> ConnectorGateCheck {
    if package_harn_files.is_empty() {
        return ConnectorGateCheck {
            name: name.to_string(),
            status: "fail".to_string(),
            command: Vec::new(),
            exit_code: Some(1),
            duration_ms: 0,
            stdout: String::new(),
            stderr: "no package-owned .harn files found".to_string(),
            details: Vec::new(),
        };
    }
    let mut args = vec![subcommand.to_string()];
    args.extend(package_harn_files.iter().cloned());
    run_harn_subcommand_owned(name, package_dir, args)
}

fn package_harn_file_args(package_dir: &Path) -> Vec<String> {
    let mut files = Vec::new();
    collect_package_harn_files(package_dir, &mut files);
    files
        .into_iter()
        .filter_map(|path| {
            path.strip_prefix(package_dir)
                .ok()
                .map(|rel| rel.to_string_lossy().into_owned())
        })
        .collect()
}

fn collect_package_harn_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut entries = entries.filter_map(Result::ok).collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.path());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| matches!(name, ".git" | ".harn" | "target" | "node_modules"))
            {
                continue;
            }
            collect_package_harn_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "harn") {
            out.push(path);
        }
    }
}

fn run_connector_fixture_tests(package_dir: &Path) -> ConnectorGateCheck {
    let started = Instant::now();
    let mut details = Vec::new();
    let mut stderr = Vec::new();
    let tests = connector_test_files(package_dir);
    if tests.is_empty() {
        return ConnectorGateCheck {
            name: "connector fixture tests".to_string(),
            status: "skipped".to_string(),
            command: Vec::new(),
            exit_code: None,
            duration_ms: elapsed_ms(started),
            stdout: String::new(),
            stderr: String::new(),
            details: vec!["no runnable tests/*.harn files found".to_string()],
        };
    }
    let mut failed = false;
    for test in tests {
        let rel = test
            .strip_prefix(package_dir)
            .unwrap_or(test.as_path())
            .to_string_lossy()
            .into_owned();
        let check = run_harn_subcommand(&format!("harn run {rel}"), package_dir, &["run", &rel]);
        if check.status == "pass" {
            details.push(format!("{rel}: pass"));
        } else {
            failed = true;
            stderr.push(format!(
                "{rel}: failed\nstdout:\n{}\nstderr:\n{}",
                check.stdout, check.stderr
            ));
        }
    }
    ConnectorGateCheck {
        name: "connector fixture tests".to_string(),
        status: if failed { "fail" } else { "pass" }.to_string(),
        command: Vec::new(),
        exit_code: Some(if failed { 1 } else { 0 }),
        duration_ms: elapsed_ms(started),
        stdout: String::new(),
        stderr: stderr.join("\n"),
        details,
    }
}

fn connector_test_files(package_dir: &Path) -> Vec<PathBuf> {
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

fn run_install_import_smoke(package_dir: &Path, metadata_ok: bool) -> ConnectorGateCheck {
    let started = Instant::now();
    if !metadata_ok {
        return ConnectorGateCheck {
            name: "package install/import smoke".to_string(),
            status: "skipped".to_string(),
            command: Vec::new(),
            exit_code: None,
            duration_ms: elapsed_ms(started),
            stdout: String::new(),
            stderr: String::new(),
            details: vec!["skipped because package metadata did not pass".to_string()],
        };
    }
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
        return gate_check_from_findings(
            "package install/import smoke",
            started,
            vec!["[exports] is required for install/import smoke".to_string()],
            Vec::new(),
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
        "[package]\nname = \"connector-smoke-consumer\"\nversion = \"0.0.0\"\n\n[dependencies]\n\"{}\" = {{ path = \"{}\" }}\n",
        toml_escape_basic_string(&package_name),
        toml_escape_basic_string(&package_dir.display().to_string())
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
        return ConnectorGateCheck {
            name: "package install/import smoke".to_string(),
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
        let source = format!("import \"{package_name}/{export}\"\n\npipeline default() {{\n}}\n");
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

fn toml_escape_basic_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn validate_doc_examples(package_dir: &Path) -> ConnectorGateCheck {
    let started = Instant::now();
    let mut details = Vec::new();
    let mut failures = Vec::new();
    let mut markdown_files = Vec::new();
    collect_markdown_files(package_dir, &mut markdown_files);
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
        return ConnectorGateCheck {
            name: "doc examples".to_string(),
            status: "skipped".to_string(),
            command: Vec::new(),
            exit_code: None,
            duration_ms: elapsed_ms(started),
            stdout: String::new(),
            stderr: String::new(),
            details: vec!["no Markdown harn examples found".to_string()],
        };
    }
    gate_check_from_findings("doc examples", started, failures, details)
}

fn collect_markdown_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut entries = entries.filter_map(Result::ok).collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.path());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| matches!(name, ".git" | ".harn" | "target" | "node_modules"))
            {
                continue;
            }
            collect_markdown_files(&path, out);
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
    source.contains("pipeline ") || source.contains("pub fn ") || source.contains("\nfn ")
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

fn summarize_gate_checks(checks: &[ConnectorGateCheck]) -> ConnectorGateSummary {
    let mut summary = ConnectorGateSummary::default();
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

async fn check_one_connector(
    provider_id: harn_vm::ProviderId,
    manifest_dir: &Path,
    module: &str,
    fixtures: &[ConnectorContractFixture],
    run_poll_tick: bool,
) -> Result<CheckedConnector, String> {
    use harn_vm::Connector as _;

    let module_path = harn_vm::resolve_module_import_path(manifest_dir, module);
    if !module_path.is_file() {
        return Err(format!(
            "provider '{}' connector module '{}' does not exist",
            provider_id.as_str(),
            module_path.display()
        ));
    }
    let effect_policy_diagnostics = connector_effect_policy_diagnostics(&module_path)?;
    if !effect_policy_diagnostics.is_empty() {
        return Err(format!(
            "provider '{}' connector module '{}' violates connector effect policy:\n{}",
            provider_id.as_str(),
            module_path.display(),
            effect_policy_diagnostics
                .into_iter()
                .map(|diagnostic| format!("- {diagnostic}"))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }

    let contract = harn_vm::load_harn_connector_contract(&module_path)
        .await
        .map_err(|error| {
            format!(
                "failed to load connector module '{}' for provider '{}': {error}",
                module_path.display(),
                provider_id.as_str()
            )
        })?;
    if contract.provider_id != provider_id {
        return Err(format!(
            "provider '{}' resolves to connector module '{}' which declares provider_id '{}'",
            provider_id.as_str(),
            module_path.display(),
            contract.provider_id.as_str()
        ));
    }
    if contract.kinds.is_empty() {
        return Err(format!(
            "provider '{}' kinds() must return at least one trigger kind",
            provider_id.as_str()
        ));
    }
    if contract.payload_schema.harn_schema_name.trim().is_empty() {
        return Err(format!(
            "provider '{}' payload_schema().harn_schema_name must not be empty",
            provider_id.as_str()
        ));
    }
    if !contract.payload_schema.json_schema.is_null()
        && !contract.payload_schema.json_schema.is_object()
    {
        return Err(format!(
            "provider '{}' payload_schema().json_schema must be an object when present",
            provider_id.as_str()
        ));
    }
    if contract.kinds.iter().any(|kind| kind.as_str() == "poll") && !contract.has_poll_tick {
        return Err(format!(
            "provider '{}' declares kind 'poll' but does not export poll_tick(ctx)",
            provider_id.as_str()
        ));
    }

    let mut connector = harn_vm::HarnConnector::load(&module_path)
        .await
        .map_err(|error| {
            format!(
                "failed to instantiate connector module '{}' for provider '{}': {error}",
                module_path.display(),
                provider_id.as_str()
            )
        })?;
    let ctx = connector_ctx().await?;
    connector.init(ctx).await.map_err(|error| {
        format!(
            "provider '{}' init(ctx) failed: {error}",
            provider_id.as_str()
        )
    })?;

    let activation_bindings = contract
        .kinds
        .iter()
        .filter(|kind| run_poll_tick || kind.as_str() != "poll")
        .map(|kind| {
            let mut binding = harn_vm::TriggerBinding::new(
                provider_id.clone(),
                kind.clone(),
                format!("contract-{}-{}", provider_id.as_str(), kind.as_str()),
            );
            binding.dedupe_key = Some("event.dedupe_key".to_string());
            if kind.as_str() == "poll" {
                binding.config = json!({
                    "poll": {
                        "interval_secs": 3600,
                        "state_key": "contract-check",
                        "lease_id": "contract-check",
                        "max_batch_size": 10,
                    }
                });
            }
            binding
        })
        .collect::<Vec<_>>();
    if !activation_bindings.is_empty() {
        connector
            .activate(&activation_bindings)
            .await
            .map_err(|error| {
                format!(
                    "provider '{}' activate(bindings) failed: {error}",
                    provider_id.as_str()
                )
            })?;
        if run_poll_tick {
            tokio::task::yield_now().await;
        }
    }

    match connector
        .client()
        .call("__harn_contract_check__", json!({}))
        .await
    {
        Ok(_) | Err(harn_vm::ClientError::MethodNotFound(_)) => {}
        Err(error) => {
            connector
                .shutdown(StdDuration::ZERO)
                .await
                .map_err(|shutdown_error| shutdown_error.to_string())?;
            return Err(format!(
                "provider '{}' call(method, args) validation failed: {error}",
                provider_id.as_str()
            ));
        }
    }

    let mut checked_fixtures = Vec::new();
    for fixture in fixtures
        .iter()
        .filter(|fixture| fixture.provider == provider_id)
    {
        let raw = raw_from_fixture(fixture)?;
        let result = match connector.normalize_inbound_result(raw).await {
            Ok(result) => {
                if let Some(expected) = fixture.expect_error_contains.as_deref() {
                    return Err(format!(
                        "provider '{}' normalize_inbound fixture '{}' expected error containing '{}' but succeeded",
                        provider_id.as_str(),
                        fixture_name(fixture),
                        expected
                    ));
                }
                result
            }
            Err(error) => {
                if let Some(expected) = fixture.expect_error_contains.as_deref() {
                    let message = error.to_string();
                    if message.contains(expected) {
                        checked_fixtures.push(CheckedFixture {
                            name: fixture_name(fixture),
                            result_type: "error".to_string(),
                            event_count: 0,
                        });
                        continue;
                    }
                    return Err(format!(
                        "provider '{}' normalize_inbound fixture '{}' expected error containing '{}' but got: {message}",
                        provider_id.as_str(),
                        fixture_name(fixture),
                        expected
                    ));
                }
                return Err(format!(
                    "provider '{}' normalize_inbound fixture '{}' failed: {error}",
                    provider_id.as_str(),
                    fixture_name(fixture)
                ));
            }
        };
        let checked = validate_normalize_result(fixture, &result)?;
        checked_fixtures.push(checked);
    }

    connector
        .shutdown(StdDuration::ZERO)
        .await
        .map_err(|error| {
            format!(
                "provider '{}' shutdown() failed: {error}",
                provider_id.as_str()
            )
        })?;

    Ok(CheckedConnector {
        provider: provider_id.as_str().to_string(),
        module: module_path.display().to_string(),
        kinds: contract
            .kinds
            .iter()
            .map(|kind| kind.as_str().to_string())
            .collect(),
        payload_schema: contract.payload_schema.harn_schema_name,
        has_poll_tick: contract.has_poll_tick,
        fixtures: checked_fixtures,
    })
}

async fn connector_ctx() -> Result<harn_vm::ConnectorCtx, String> {
    let event_log = Arc::new(harn_vm::event_log::AnyEventLog::Memory(
        harn_vm::event_log::MemoryEventLog::new(128),
    ));
    let metrics = Arc::new(harn_vm::MetricsRegistry::default());
    let inbox = harn_vm::InboxIndex::new(event_log.clone(), metrics.clone())
        .await
        .map_err(|error| error.to_string())?;
    Ok(harn_vm::ConnectorCtx {
        event_log,
        secrets: Arc::new(ContractSecretProvider::default()),
        inbox: Arc::new(inbox),
        metrics,
        rate_limiter: Arc::new(harn_vm::RateLimiterFactory::default()),
    })
}

fn connector_effect_policy_diagnostics(module_path: &Path) -> Result<Vec<String>, String> {
    let source = std::fs::read_to_string(module_path)
        .map_err(|error| format!("failed to read {}: {error}", module_path.display()))?;
    let program = harn_parser::parse_source(&source)
        .map_err(|error| format!("failed to parse {}: {error}", module_path.display()))?;
    Ok(harn_lint::lint_with_source(&program, &source)
        .into_iter()
        .filter(|diagnostic| diagnostic.rule == "connector-effect-policy")
        .map(|diagnostic| {
            format!(
                "{}:{} [{}]: {}",
                diagnostic.span.line, diagnostic.span.column, diagnostic.rule, diagnostic.message
            )
        })
        .collect())
}

#[derive(Default)]
struct ContractSecretProvider {
    values: BTreeMap<String, String>,
}

#[async_trait]
impl harn_vm::secrets::SecretProvider for ContractSecretProvider {
    async fn get(
        &self,
        id: &harn_vm::secrets::SecretId,
    ) -> Result<harn_vm::secrets::SecretBytes, harn_vm::secrets::SecretError> {
        let value = self
            .values
            .get(&id.to_string())
            .cloned()
            .unwrap_or_else(|| "contract-fixture-secret".to_string());
        Ok(harn_vm::secrets::SecretBytes::from(value))
    }

    async fn put(
        &self,
        _id: &harn_vm::secrets::SecretId,
        _value: harn_vm::secrets::SecretBytes,
    ) -> Result<(), harn_vm::secrets::SecretError> {
        Ok(())
    }

    async fn rotate(
        &self,
        id: &harn_vm::secrets::SecretId,
    ) -> Result<harn_vm::secrets::RotationHandle, harn_vm::secrets::SecretError> {
        Ok(harn_vm::secrets::RotationHandle {
            provider: self.namespace().to_string(),
            id: id.clone(),
            from_version: None,
            to_version: None,
        })
    }

    async fn list(
        &self,
        _prefix: &harn_vm::secrets::SecretId,
    ) -> Result<Vec<harn_vm::secrets::SecretMeta>, harn_vm::secrets::SecretError> {
        Ok(Vec::new())
    }

    fn namespace(&self) -> &str {
        "connector-contract"
    }

    fn supports_versions(&self) -> bool {
        false
    }
}

fn raw_from_fixture(fixture: &ConnectorContractFixture) -> Result<harn_vm::RawInbound, String> {
    if fixture.body.is_some() && fixture.body_json.is_some() {
        return Err(format!(
            "fixture '{}' sets both body and body_json",
            fixture_name(fixture)
        ));
    }
    let body = match (&fixture.body, &fixture.body_json) {
        (Some(body), None) => body.as_bytes().to_vec(),
        (None, Some(value)) => serde_json::to_vec(&toml_to_json(value)?)
            .map_err(|error| format!("failed to serialize fixture body_json: {error}"))?,
        (None, None) => b"{}".to_vec(),
        (Some(_), Some(_)) => unreachable!("checked above"),
    };
    let mut raw = harn_vm::RawInbound::new(
        fixture
            .kind
            .clone()
            .unwrap_or_else(|| "webhook".to_string()),
        fixture.headers.clone(),
        body,
    );
    raw.query = fixture.query.clone();
    raw.received_at = OffsetDateTime::parse("2026-04-22T12:00:00Z", &Rfc3339)
        .map_err(|error| error.to_string())?;
    raw.metadata = match &fixture.metadata {
        Some(value) => toml_to_json(value)?,
        None => json!({
            "binding_id": format!("contract-{}-fixture", fixture.provider.as_str()),
            "binding_version": 1,
            "path": "/harn/connector-contract",
        }),
    };
    Ok(raw)
}

fn toml_to_json(value: &toml::Value) -> Result<JsonValue, String> {
    serde_json::to_value(value).map_err(|error| format!("failed to convert TOML fixture: {error}"))
}

fn validate_normalize_result(
    fixture: &ConnectorContractFixture,
    result: &harn_vm::ConnectorNormalizeResult,
) -> Result<CheckedFixture, String> {
    let (result_type, event_count) = match result {
        harn_vm::ConnectorNormalizeResult::Event(event) => {
            if let Some(expected_kind) = fixture.expect_kind.as_deref() {
                if event.kind != expected_kind {
                    return Err(format!(
                        "fixture '{}' expected event kind '{}' but got '{}'",
                        fixture_name(fixture),
                        expected_kind,
                        event.kind
                    ));
                }
            }
            validate_event_expectations(fixture, event.as_ref())?;
            ("event", 1)
        }
        harn_vm::ConnectorNormalizeResult::Batch(events) => {
            if let Some(expected_kind) = fixture.expect_kind.as_deref() {
                if let Some(event) = events.iter().find(|event| event.kind != expected_kind) {
                    return Err(format!(
                        "fixture '{}' expected all event kinds '{}' but got '{}'",
                        fixture_name(fixture),
                        expected_kind,
                        event.kind
                    ));
                }
            }
            for event in events {
                validate_event_expectations(fixture, event)?;
            }
            ("batch", events.len())
        }
        harn_vm::ConnectorNormalizeResult::ImmediateResponse { response, events } => {
            validate_response_expectations(fixture, "immediate_response", response)?;
            if let Some(expected_kind) = fixture.expect_kind.as_deref() {
                if let Some(event) = events.iter().find(|event| event.kind != expected_kind) {
                    return Err(format!(
                        "fixture '{}' expected all event kinds '{}' but got '{}'",
                        fixture_name(fixture),
                        expected_kind,
                        event.kind
                    ));
                }
            }
            for event in events {
                validate_event_expectations(fixture, event)?;
            }
            ("immediate_response", events.len())
        }
        harn_vm::ConnectorNormalizeResult::Reject(response) => {
            validate_response_expectations(fixture, "reject", response)?;
            ("reject", 0)
        }
    };

    if let Some(expected_type) = fixture.expect_type.as_deref() {
        if result_type != expected_type {
            return Err(format!(
                "fixture '{}' expected NormalizeResult type '{}' but got '{}'",
                fixture_name(fixture),
                expected_type,
                result_type
            ));
        }
    }
    if let Some(expected_event_count) = fixture.expect_event_count {
        if event_count != expected_event_count {
            return Err(format!(
                "fixture '{}' expected {} normalized event(s) but got {}",
                fixture_name(fixture),
                expected_event_count,
                event_count
            ));
        }
    }

    Ok(CheckedFixture {
        name: fixture_name(fixture),
        result_type: result_type.to_string(),
        event_count,
    })
}

fn validate_event_expectations(
    fixture: &ConnectorContractFixture,
    event: &harn_vm::TriggerEvent,
) -> Result<(), String> {
    if let Some(expected_dedupe_key) = fixture.expect_dedupe_key.as_deref() {
        if event.dedupe_key != expected_dedupe_key {
            return Err(format!(
                "fixture '{}' expected dedupe_key '{}' but got '{}'",
                fixture_name(fixture),
                expected_dedupe_key,
                event.dedupe_key
            ));
        }
    }
    if let Some(expected_signature_state) = fixture.expect_signature_state.as_deref() {
        let signature_state = match &event.signature_status {
            harn_vm::SignatureStatus::Verified => "verified",
            harn_vm::SignatureStatus::Unsigned => "unsigned",
            harn_vm::SignatureStatus::Failed { .. } => "failed",
        };
        if signature_state != expected_signature_state {
            return Err(format!(
                "fixture '{}' expected signature state '{}' but got '{}'",
                fixture_name(fixture),
                expected_signature_state,
                signature_state
            ));
        }
    }
    if let Some(expected_payload) = &fixture.expect_payload_contains {
        let expected = toml_to_json(expected_payload)?;
        let actual = serde_json::to_value(&event.provider_payload).map_err(|error| {
            format!(
                "fixture '{}' failed to serialize provider payload: {error}",
                fixture_name(fixture)
            )
        })?;
        assert_json_contains(fixture, "provider_payload", &actual, &expected)?;
    }
    Ok(())
}

fn validate_response_expectations(
    fixture: &ConnectorContractFixture,
    result_type: &str,
    response: &harn_vm::ConnectorHttpResponse,
) -> Result<(), String> {
    if let Some(expected_status) = fixture.expect_response_status {
        if response.status != expected_status {
            return Err(format!(
                "fixture '{}' expected {result_type} HTTP status {} but got {}",
                fixture_name(fixture),
                expected_status,
                response.status
            ));
        }
    }
    if let Some(expected_body) = &fixture.expect_response_body {
        let expected = toml_to_json(expected_body)?;
        if response.body != expected {
            return Err(format!(
                "fixture '{}' expected {result_type} body {} but got {}",
                fixture_name(fixture),
                expected,
                response.body
            ));
        }
    }
    Ok(())
}

fn assert_json_contains(
    fixture: &ConnectorContractFixture,
    path: &str,
    actual: &JsonValue,
    expected: &JsonValue,
) -> Result<(), String> {
    match expected {
        JsonValue::Object(expected_map) => {
            let actual_map = actual.as_object().ok_or_else(|| {
                format!(
                    "fixture '{}' expected {path} to be an object containing {} but got {}",
                    fixture_name(fixture),
                    expected,
                    actual
                )
            })?;
            for (key, expected_value) in expected_map {
                let actual_value = actual_map.get(key).ok_or_else(|| {
                    format!(
                        "fixture '{}' expected {path}.{key} to exist in {}",
                        fixture_name(fixture),
                        actual
                    )
                })?;
                assert_json_contains(
                    fixture,
                    &format!("{path}.{key}"),
                    actual_value,
                    expected_value,
                )?;
            }
            Ok(())
        }
        _ if actual == expected => Ok(()),
        _ => Err(format!(
            "fixture '{}' expected {path} to contain {} but got {}",
            fixture_name(fixture),
            expected,
            actual
        )),
    }
}

fn fixture_name(fixture: &ConnectorContractFixture) -> String {
    fixture
        .name
        .clone()
        .unwrap_or_else(|| format!("{} fixture", fixture.provider.as_str()))
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

fn print_human_report(report: &ConnectorCheckReport) {
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

fn print_gate_report(report: &ConnectorGateReport) {
    println!(
        "Connector package gate {} for {}: {} passed, {} failed, {} skipped.",
        report.status,
        report.package,
        report.summary.passed,
        report.summary.failed,
        report.summary.skipped
    );
    for check in &report.checks {
        println!("- {}: {}", check.name, check.status);
        for detail in &check.details {
            println!("  {detail}");
        }
        if !check.stderr.trim().is_empty() {
            eprintln!("{} failed output:\n{}", check.name, check.stderr.trim());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;
    use std::sync::OnceLock;

    async fn connector_check_test_guard() -> tokio::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await
    }

    fn write_package(manifest_tail: &str, lib: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("harn.toml"),
            format!(
                r#"
[package]
name = "contract-test"
version = "0.1.0"

[[providers]]
id = "echo"
connector = {{ harn = "./lib.harn" }}
{manifest_tail}
"#
            ),
        )
        .unwrap();
        fs::write(dir.path().join("lib.harn"), lib).unwrap();
        dir
    }

    fn check_args(path: &Path) -> ConnectorCheckArgs {
        ConnectorCheckArgs {
            package: path.display().to_string(),
            providers: Vec::new(),
            run_poll_tick: false,
            json: false,
        }
    }

    #[test]
    fn package_dir_from_anchor_finds_manifest_for_nested_file() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src/nested")).unwrap();
        fs::write(dir.path().join("harn.toml"), "[package]\nname = \"demo\"\n").unwrap();
        let nested = dir.path().join("src/nested/lib.harn");
        fs::write(&nested, "").unwrap();

        assert_eq!(package_dir_from_anchor(&nested), dir.path());
    }

    #[tokio::test]
    async fn connector_check_accepts_valid_fixture_package() {
        let _guard = connector_check_test_guard().await;
        let dir = write_package(
            r#"
[connector_contract]
version = 1

[[connector_contract.fixtures]]
provider = "echo"
name = "echo event"
kind = "webhook"
body_json = { id = "evt-1", message = "hello" }
expect_type = "event"
expect_kind = "echo.received"
expect_event_count = 1
"#,
            r#"
var active_bindings = []

pub fn provider_id() {
  return "echo"
}

pub fn kinds() {
  return ["webhook"]
}

pub fn payload_schema() {
  return {
    harn_schema_name: "EchoEventPayload",
    json_schema: {
      type: "object",
      additionalProperties: true,
    },
  }
}

pub fn init(ctx) {
  if ctx.capabilities.secret_get != true {
    throw "secret_get capability missing"
  }
}

pub fn activate(bindings) {
  active_bindings = bindings
  metrics_inc("echo_activate_bindings", len(bindings))
}

pub fn shutdown() {
  metrics_inc("echo_shutdown")
}

pub fn normalize_inbound(raw) {
  let body = raw.body_json ?? json_parse(raw.body_text)
  let token = secret_get("echo/api-token")
  event_log_emit("connectors.echo.contract", "normalize", {
    token: token,
  })
  return {
    type: "event",
    event: {
      kind: "echo.received",
      dedupe_key: "echo:" + body.id,
      payload: body,
    },
  }
}

pub fn call(method, _args) {
  throw "method_not_found:" + method
}
"#,
        );
        let report = check_connector_package(&check_args(dir.path()))
            .await
            .expect("valid package should pass");
        assert_eq!(report.checked_connectors.len(), 1);
        assert_eq!(report.fixture_count, 1);
        assert_eq!(
            report.checked_connectors[0].payload_schema,
            "EchoEventPayload"
        );
    }

    #[tokio::test]
    async fn connector_check_rejects_payload_schema_name_mismatch() {
        let _guard = connector_check_test_guard().await;
        let dir = write_package(
            "",
            r#"
pub fn provider_id() { return "echo" }
pub fn kinds() { return ["webhook"] }
pub fn payload_schema() {
  return {
    name: "EchoEventPayload",
    json_schema: {type: "object"},
  }
}
pub fn normalize_inbound(_raw) {
  return {type: "reject", status: 400}
}
"#,
        );
        let error = check_connector_package(&check_args(dir.path()))
            .await
            .unwrap_err();
        assert!(error.contains("payload_schema() must return { harn_schema_name, json_schema? }"));
    }

    #[tokio::test]
    async fn connector_check_rejects_legacy_immediate_response_wrapper() {
        let _guard = connector_check_test_guard().await;
        let dir = write_package(
            r#"
[[connector_contract.fixtures]]
provider = "echo"
body_json = { id = "evt-1" }
"#,
            r#"
pub fn provider_id() { return "echo" }
pub fn kinds() { return ["webhook"] }
pub fn payload_schema() { return "EchoEventPayload" }
pub fn normalize_inbound(_raw) {
  return {
    immediate_response: {status: 200, body: "ok"},
    event: {
      kind: "echo.received",
      dedupe_key: "echo:evt-1",
      payload: {id: "evt-1"},
    },
  }
}
"#,
        );
        let error = check_connector_package(&check_args(dir.path()))
            .await
            .unwrap_err();
        assert!(error.contains("normalize_inbound fixture"));
    }

    #[tokio::test]
    async fn connector_check_reports_static_effect_policy_violations() {
        let _guard = connector_check_test_guard().await;
        let dir = write_package(
            "",
            r#"
pub fn provider_id() { return "echo" }
pub fn kinds() { return ["webhook"] }
pub fn payload_schema() { return "EchoEventPayload" }
pub fn normalize_inbound(_raw) {
  http_get("https://example.invalid")
  return {type: "reject", status: 400}
}
"#,
        );
        let error = check_connector_package(&check_args(dir.path()))
            .await
            .unwrap_err();
        assert!(error.contains("connector-effect-policy"), "{error}");
        assert!(error.contains("http_get"), "{error}");
    }

    #[tokio::test]
    async fn connector_check_can_assert_runtime_policy_denial_fixture() {
        let _guard = connector_check_test_guard().await;
        let dir = write_package(
            r#"
[connector_contract]
version = 1

[[connector_contract.fixtures]]
provider = "echo"
name = "indirect file read denied"
body_json = { id = "evt-1" }
expect_error_contains = "violated effect policy"
"#,
            r#"
pub fn provider_id() { return "echo" }
pub fn kinds() { return ["webhook"] }
pub fn payload_schema() { return "EchoEventPayload" }

fn read_indirect() {
  return read_file("ambient.txt")
}

pub fn normalize_inbound(raw) {
  let _body = raw.body_json
  read_indirect()
  return {type: "reject", status: 400}
}
"#,
        );
        let report = check_connector_package(&check_args(dir.path()))
            .await
            .expect("expected-error fixture should pass");
        assert_eq!(report.fixture_count, 1);
        assert_eq!(
            report.checked_connectors[0].fixtures[0].result_type,
            "error"
        );
    }
}
