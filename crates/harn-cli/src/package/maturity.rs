//! Maturity surface for the package manager: outdated/audit/artifacts.
//!
//! These commands let downstream automation (Burin Code, Harn Cloud, CI) ask
//! a single Harn binary "is this checkout still current?" — covering registry
//! versions, branch HEAD drift, lockfile provenance, package compatibility,
//! and the protocol-artifact contract that hosts vendor.
//!
//! All three reports are JSON-serializable so the same surface drives both
//! human output and machine consumers without a second code path.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process;

use serde::Serialize;

use super::*;

#[derive(Debug, Clone, Serialize)]
pub struct OutdatedReport {
    pub manifest_path: String,
    pub generator_version: String,
    pub current_harn: String,
    pub entries: Vec<OutdatedEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OutdatedEntry {
    pub alias: String,
    pub kind: String,
    pub source: String,
    pub current_rev: Option<String>,
    pub current_version: Option<String>,
    pub latest_rev: Option<String>,
    pub latest_version: Option<String>,
    pub status: OutdatedStatus,
    pub registry_name: Option<String>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum OutdatedStatus {
    Current,
    Outdated,
    Unknown,
    Skipped,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuditReport {
    pub manifest_path: String,
    pub lock_path: String,
    pub current_harn: String,
    pub generator_version: String,
    pub protocol_artifact_version: String,
    pub findings: Vec<AuditFinding>,
    pub ok: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuditFinding {
    pub alias: Option<String>,
    pub severity: AuditSeverity,
    pub code: AuditCode,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuditSeverity {
    Error,
    Warning,
    Info,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuditCode {
    LockfileMissing,
    LockfileStale,
    LockfileGeneratorMismatch,
    LockfileProtocolMismatch,
    EntryMissingProvenance,
    HarnCompatViolation,
    PathDependencyInPublishable,
    YankedRegistryVersion,
    ContentHashMismatch,
    ManifestDigestMismatch,
    PackageMissing,
    RegistryUnavailable,
}

#[derive(Debug, Clone, Serialize)]
pub struct ArtifactDriftReport {
    pub current_artifact_version: String,
    pub vendored_artifact_version: Option<String>,
    pub schema_version: u32,
    pub vendored_schema_version: Option<u32>,
    pub differences: Vec<String>,
    pub ok: bool,
}

pub fn outdated_packages(refresh: bool, remote: bool, registry_override: Option<&str>, json: bool) {
    let result = (|| -> Result<OutdatedReport, PackageError> {
        let workspace = PackageWorkspace::from_current_dir()?;
        outdated_packages_in(&workspace, refresh, remote, registry_override)
    })();

    match result {
        Ok(report) if json => print_json(&report),
        Ok(report) => print_outdated_report(&report),
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    }
}

pub(crate) fn outdated_packages_in(
    workspace: &PackageWorkspace,
    refresh: bool,
    remote: bool,
    registry_override: Option<&str>,
) -> Result<OutdatedReport, PackageError> {
    let ctx = workspace.load_manifest_context()?;
    let lock = LockFile::load(&ctx.lock_path())?.ok_or_else(|| {
        format!(
            "{} is missing; run `harn install`",
            ctx.lock_path().display()
        )
    })?;

    // Defer registry loading — `harn package outdated` on a repo with only
    // path or non-registry git deps shouldn't reach for the network just
    // to print "skipped" rows.
    let needs_registry = lock
        .packages
        .iter()
        .any(|entry| entry.registry.is_some() || registry_override.is_some());
    let registry_index = if needs_registry {
        try_load_registry_index(workspace, registry_override, refresh).unwrap_or(None)
    } else {
        None
    };

    let mut entries = Vec::new();
    for entry in &lock.packages {
        let kind = lock_entry_kind(entry);
        let alias = entry.name.clone();
        let mut report = OutdatedEntry {
            alias: alias.clone(),
            kind: kind.to_string(),
            source: entry.source.clone(),
            current_rev: entry.commit.clone(),
            current_version: entry.package_version.clone(),
            latest_rev: None,
            latest_version: None,
            status: OutdatedStatus::Unknown,
            registry_name: entry.registry.as_ref().map(|reg| reg.name.clone()),
            note: None,
        };

        match kind {
            "path" => {
                report.status = OutdatedStatus::Skipped;
                report.note = Some(
                    "path dependencies are live-linked; rebuild to pick up changes".to_string(),
                );
            }
            "registry" => {
                let reg = entry
                    .registry
                    .as_ref()
                    .expect("registry kind requires registry provenance");
                match registry_index.as_ref() {
                    Some(index) => match latest_registry_version_for(index, &reg.name) {
                        Some(latest) => {
                            report.latest_version = Some(latest.clone());
                            report.status = if latest == reg.version {
                                OutdatedStatus::Current
                            } else {
                                OutdatedStatus::Outdated
                            };
                        }
                        None => {
                            report.status = OutdatedStatus::Unknown;
                            report.note = Some(format!("registry has no entry for {}", reg.name));
                        }
                    },
                    None => {
                        report.status = OutdatedStatus::Unknown;
                        report.note = Some("registry index unavailable".to_string());
                    }
                }
            }
            "git" => {
                if remote {
                    match resolve_remote_branch_head(entry) {
                        Ok(Some(head)) => {
                            report.latest_rev = Some(head.clone());
                            report.status = if Some(head) == entry.commit {
                                OutdatedStatus::Current
                            } else {
                                OutdatedStatus::Outdated
                            };
                        }
                        Ok(None) => {
                            report.status = OutdatedStatus::Skipped;
                            report.note = Some(
                                "git rev pin: pass --remote to probe upstream tags".to_string(),
                            );
                        }
                        Err(error) => {
                            report.status = OutdatedStatus::Unknown;
                            report.note = Some(format!("git probe failed: {error}"));
                        }
                    }
                } else {
                    report.status = OutdatedStatus::Skipped;
                    report.note = Some(
                        "pass --remote to probe git remotes for branch HEAD drift".to_string(),
                    );
                }
            }
            other => {
                report.status = OutdatedStatus::Unknown;
                report.note = Some(format!("unsupported lock kind '{other}'"));
            }
        }
        entries.push(report);
    }

    Ok(OutdatedReport {
        manifest_path: ctx.manifest_path().display().to_string(),
        generator_version: lock.generator_version.clone(),
        current_harn: env!("CARGO_PKG_VERSION").to_string(),
        entries,
    })
}

pub fn audit_packages(registry_override: Option<&str>, skip_materialized: bool, json: bool) {
    let result = (|| -> Result<AuditReport, PackageError> {
        let workspace = PackageWorkspace::from_current_dir()?;
        audit_packages_in(&workspace, registry_override, skip_materialized)
    })();

    match result {
        Ok(report) => {
            let ok = report.ok;
            if json {
                print_json(&report);
            } else {
                print_audit_report(&report);
            }
            if !ok {
                process::exit(1);
            }
        }
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    }
}

pub(crate) fn audit_packages_in(
    workspace: &PackageWorkspace,
    registry_override: Option<&str>,
    skip_materialized: bool,
) -> Result<AuditReport, PackageError> {
    let ctx = workspace.load_manifest_context()?;
    let lock_path = ctx.lock_path();
    let manifest_path = ctx.manifest_path();

    let mut findings = Vec::new();

    let lock = match LockFile::load(&lock_path)? {
        Some(lock) => lock,
        None => {
            findings.push(AuditFinding {
                alias: None,
                severity: AuditSeverity::Error,
                code: AuditCode::LockfileMissing,
                message: format!("{} is missing; run `harn install`", lock_path.display()),
            });
            return Ok(AuditReport {
                manifest_path: manifest_path.display().to_string(),
                lock_path: lock_path.display().to_string(),
                current_harn: env!("CARGO_PKG_VERSION").to_string(),
                generator_version: String::new(),
                protocol_artifact_version: String::new(),
                ok: false,
                findings,
            });
        }
    };

    let current_harn = env!("CARGO_PKG_VERSION").to_string();
    if lock.generator_version != current_harn {
        findings.push(AuditFinding {
            alias: None,
            severity: AuditSeverity::Warning,
            code: AuditCode::LockfileGeneratorMismatch,
            message: format!(
                "harn.lock generator_version {} != current Harn {current_harn}; rerun `harn install` to refresh provenance",
                lock.generator_version
            ),
        });
    }
    if lock.protocol_artifact_version != current_harn {
        findings.push(AuditFinding {
            alias: None,
            severity: AuditSeverity::Warning,
            code: AuditCode::LockfileProtocolMismatch,
            message: format!(
                "harn.lock protocol_artifact_version {} != current Harn {current_harn}; downstream protocol bindings may regenerate",
                lock.protocol_artifact_version
            ),
        });
    }

    if let Err(error) = validate_lock_matches_manifest(workspace, &ctx, &lock) {
        findings.push(AuditFinding {
            alias: None,
            severity: AuditSeverity::Error,
            code: AuditCode::LockfileStale,
            message: error.to_string(),
        });
    }

    let needs_registry = lock
        .packages
        .iter()
        .any(|entry| entry.registry.is_some() || registry_override.is_some());
    let registry_index = if needs_registry {
        try_load_registry_index(workspace, registry_override, false).unwrap_or_else(|error| {
            findings.push(AuditFinding {
                alias: None,
                severity: AuditSeverity::Info,
                code: AuditCode::RegistryUnavailable,
                message: format!("registry probe skipped: {error}"),
            });
            None
        })
    } else {
        None
    };

    let manifest_aliases: BTreeMap<&String, &Dependency> =
        ctx.manifest.dependencies.iter().collect();

    for entry in &lock.packages {
        let alias = entry.name.clone();
        let kind = lock_entry_kind(entry);

        if entry.manifest_digest.is_none() || entry.package_version.is_none() {
            findings.push(AuditFinding {
                alias: Some(alias.clone()),
                severity: AuditSeverity::Warning,
                code: AuditCode::EntryMissingProvenance,
                message: "lock entry has no resolved package version or manifest digest; run `harn install` to backfill".to_string(),
            });
        }

        if let Some(range) = entry.harn_compat.as_deref() {
            if !supports_current_harn(range) {
                findings.push(AuditFinding {
                    alias: Some(alias.clone()),
                    severity: AuditSeverity::Error,
                    code: AuditCode::HarnCompatViolation,
                    message: format!(
                        "{alias} declares harn = \"{range}\" which does not include the current Harn {current_harn}"
                    ),
                });
            }
        }

        if matches!(kind, "git" | "registry") {
            if let Err(error) = audit_git_entry_integrity(workspace, entry, skip_materialized) {
                findings.push(AuditFinding {
                    alias: Some(alias.clone()),
                    severity: AuditSeverity::Error,
                    code: AuditCode::ContentHashMismatch,
                    message: error.to_string(),
                });
            }
            if !skip_materialized {
                if let Some((expected, actual)) =
                    detect_manifest_digest_drift(&ctx, entry, workspace)
                {
                    findings.push(AuditFinding {
                        alias: Some(alias.clone()),
                        severity: AuditSeverity::Error,
                        code: AuditCode::ManifestDigestMismatch,
                        message: format!(
                            "{alias} harn.toml digest drifted: lock recorded {expected}, materialized package now {actual}"
                        ),
                    });
                }
            }
        }

        if let (Some(reg), Some(index)) = (entry.registry.as_ref(), registry_index.as_ref()) {
            if registry_version_is_yanked(index, &reg.name, &reg.version) {
                findings.push(AuditFinding {
                    alias: Some(alias.clone()),
                    severity: AuditSeverity::Error,
                    code: AuditCode::YankedRegistryVersion,
                    message: format!("registry now lists {}@{} as yanked", reg.name, reg.version),
                });
            }
        }

        if let Some(dep) = manifest_aliases.get(&alias) {
            if dep.local_path().is_some() && manifest_is_publishable(&ctx.manifest) {
                findings.push(AuditFinding {
                    alias: Some(alias.clone()),
                    severity: AuditSeverity::Warning,
                    code: AuditCode::PathDependencyInPublishable,
                    message: format!(
                        "{alias} is a path dependency; replace with a git or registry pin before publishing"
                    ),
                });
            }
        }
    }

    let ok = !findings
        .iter()
        .any(|finding| matches!(finding.severity, AuditSeverity::Error));

    Ok(AuditReport {
        manifest_path: manifest_path.display().to_string(),
        lock_path: lock_path.display().to_string(),
        current_harn,
        generator_version: lock.generator_version.clone(),
        protocol_artifact_version: lock.protocol_artifact_version.clone(),
        findings,
        ok,
    })
}

pub fn artifacts_manifest(output: Option<&Path>) {
    let body = match crate::commands::dump_protocol_artifacts::manifest_json() {
        Ok(body) => body,
        Err(error) => {
            eprintln!("error: failed to render protocol manifest: {error}");
            process::exit(1);
        }
    };
    let body = if body.ends_with('\n') {
        body
    } else {
        format!("{body}\n")
    };
    if let Some(path) = output {
        if let Err(error) = harn_vm::atomic_io::atomic_write(path, body.as_bytes()) {
            eprintln!("error: failed to write {}: {error}", path.display());
            process::exit(1);
        }
    } else {
        print!("{body}");
    }
}

pub fn artifacts_check(manifest: &Path, json: bool) {
    let report = match check_artifact_manifest(manifest) {
        Ok(report) => report,
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    };
    let ok = report.ok;
    if json {
        print_json(&report);
    } else {
        print_artifact_drift_report(&report);
    }
    if !ok {
        process::exit(1);
    }
}

pub(crate) fn check_artifact_manifest(
    manifest_path: &Path,
) -> Result<ArtifactDriftReport, PackageError> {
    let body = fs::read_to_string(manifest_path).map_err(|error| {
        PackageError::Ops(format!(
            "failed to read {}: {error}",
            manifest_path.display()
        ))
    })?;
    let vendored: serde_json::Value = serde_json::from_str(&body)
        .map_err(|error| format!("failed to parse {}: {error}", manifest_path.display()))?;
    let current_text =
        crate::commands::dump_protocol_artifacts::manifest_json().map_err(|error| {
            PackageError::Ops(format!("failed to render protocol manifest: {error}"))
        })?;
    let current: serde_json::Value = serde_json::from_str(&current_text).map_err(|error| {
        PackageError::Ops(format!("failed to parse generated manifest: {error}"))
    })?;

    let current_artifact_version = current
        .get("artifactVersion")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    let vendored_artifact_version = vendored
        .get("artifactVersion")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let schema_version = current
        .get("schemaVersion")
        .and_then(|value| value.as_u64())
        .unwrap_or(1) as u32;
    let vendored_schema_version = vendored
        .get("schemaVersion")
        .and_then(|value| value.as_u64())
        .map(|value| value as u32);

    let mut differences = diff_json("", &vendored, &current);
    differences.sort();
    differences.dedup();
    let ok = differences.is_empty();
    Ok(ArtifactDriftReport {
        current_artifact_version,
        vendored_artifact_version,
        schema_version,
        vendored_schema_version,
        differences,
        ok,
    })
}

fn diff_json(path: &str, left: &serde_json::Value, right: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    match (left, right) {
        (serde_json::Value::Object(left_map), serde_json::Value::Object(right_map)) => {
            let mut keys: Vec<&String> =
                left_map.keys().chain(right_map.keys()).collect::<Vec<_>>();
            keys.sort();
            keys.dedup();
            for key in keys {
                let next = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                match (left_map.get(key), right_map.get(key)) {
                    (Some(left_value), Some(right_value)) => {
                        out.extend(diff_json(&next, left_value, right_value));
                    }
                    (Some(_), None) => out.push(format!("{next}: only in vendored manifest")),
                    (None, Some(_)) => out.push(format!("{next}: only in current Harn")),
                    (None, None) => {}
                }
            }
        }
        (serde_json::Value::Array(left_arr), serde_json::Value::Array(right_arr)) => {
            if left_arr != right_arr {
                out.push(format!("{path}: array contents differ"));
            }
        }
        _ => {
            if left != right {
                out.push(format!(
                    "{path}: vendored {left} -> current {right}",
                    left = compact_value(left),
                    right = compact_value(right)
                ));
            }
        }
    }
    out
}

fn compact_value(value: &serde_json::Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "<unprintable>".to_string())
}

fn lock_entry_kind(entry: &LockEntry) -> &'static str {
    if entry.source.starts_with("path+") {
        "path"
    } else if entry.source.starts_with("git+") {
        if entry.registry.is_some() {
            "registry"
        } else {
            "git"
        }
    } else {
        "unknown"
    }
}

fn try_load_registry_index(
    workspace: &PackageWorkspace,
    registry_override: Option<&str>,
    _refresh: bool,
) -> Result<Option<PackageRegistryIndex>, PackageError> {
    match load_package_registry_in(workspace, registry_override) {
        Ok((_, index)) => Ok(Some(index)),
        Err(error) => Err(error),
    }
}

fn latest_registry_version_for(index: &PackageRegistryIndex, name: &str) -> Option<String> {
    index
        .latest_unyanked_version(name)
        .map(|version| version.to_string())
}

fn registry_version_is_yanked(index: &PackageRegistryIndex, name: &str, version: &str) -> bool {
    index.is_version_yanked(name, version)
}

fn resolve_remote_branch_head(entry: &LockEntry) -> Result<Option<String>, PackageError> {
    let Some(rev) = entry.rev_request.as_deref() else {
        return Ok(None);
    };
    if !entry.source.starts_with("git+") {
        return Ok(None);
    }
    let url = entry.source.trim_start_matches("git+");
    let head = git_ls_remote_ref(url, rev)?;
    Ok(head)
}

fn git_ls_remote_ref(url: &str, refname: &str) -> Result<Option<String>, PackageError> {
    let output = git_output(["ls-remote", url, refname], None)?;
    if !output.status.success() {
        return Err(format!(
            "git ls-remote {url} {refname} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let head = stdout
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().next())
        .map(str::to_string);
    Ok(head)
}

fn audit_git_entry_integrity(
    workspace: &PackageWorkspace,
    entry: &LockEntry,
    skip_materialized: bool,
) -> Result<(), PackageError> {
    let Some(commit) = entry.commit.as_deref() else {
        return Err(format!("{} is missing a locked commit", entry.name).into());
    };
    let Some(expected_hash) = entry.content_hash.as_deref() else {
        return Err(format!("{} is missing a content hash", entry.name).into());
    };
    let cache_dir = git_cache_dir_in(workspace, &entry.source, commit)?;
    if !cache_dir.exists() {
        return Err(format!(
            "{}: git cache entry missing at {}",
            entry.name,
            cache_dir.display()
        )
        .into());
    }
    verify_content_hash_or_compute(&cache_dir, expected_hash)?;
    if !skip_materialized {
        let workspace_pkg = workspace.manifest_dir().join(PKG_DIR).join(&entry.name);
        if workspace_pkg.exists() {
            verify_content_hash_or_compute(&workspace_pkg, expected_hash)?;
        }
    }
    Ok(())
}

fn detect_manifest_digest_drift(
    ctx: &ManifestContext,
    entry: &LockEntry,
    workspace: &PackageWorkspace,
) -> Option<(String, String)> {
    let expected = entry.manifest_digest.as_deref()?;
    let materialized = ctx.packages_dir().join(&entry.name);
    let manifest_path = materialized.join(MANIFEST);
    let bytes = fs::read(&manifest_path).ok()?;
    let actual = format!("sha256:{}", sha256_hex(&bytes));
    if actual == expected {
        return None;
    }
    let _ = workspace; // workspace reserved for future cache-only audit modes
    Some((expected.to_string(), actual))
}

fn manifest_is_publishable(manifest: &Manifest) -> bool {
    manifest
        .package
        .as_ref()
        .and_then(|pkg| pkg.name.as_deref())
        .is_some()
}

fn print_outdated_report(report: &OutdatedReport) {
    if report.entries.is_empty() {
        println!("No dependencies recorded in harn.lock.");
        return;
    }
    println!("alias\tkind\tcurrent\tlatest\tstatus\tnote");
    for entry in &report.entries {
        let current = entry
            .current_version
            .as_deref()
            .or(entry.current_rev.as_deref())
            .unwrap_or("-");
        let latest = entry
            .latest_version
            .as_deref()
            .or(entry.latest_rev.as_deref())
            .unwrap_or("-");
        let status = match entry.status {
            OutdatedStatus::Current => "current",
            OutdatedStatus::Outdated => "outdated",
            OutdatedStatus::Unknown => "unknown",
            OutdatedStatus::Skipped => "skipped",
        };
        println!(
            "{}\t{}\t{}\t{}\t{}\t{}",
            entry.alias,
            entry.kind,
            current,
            latest,
            status,
            entry.note.as_deref().unwrap_or("")
        );
    }
}

fn print_audit_report(report: &AuditReport) {
    println!("manifest: {}", report.manifest_path);
    println!("lock:     {}", report.lock_path);
    println!(
        "harn:     {} (lock generator {} / protocol {})",
        report.current_harn, report.generator_version, report.protocol_artifact_version
    );
    if report.findings.is_empty() {
        println!("No issues found.");
        return;
    }
    for finding in &report.findings {
        let severity = match finding.severity {
            AuditSeverity::Error => "error",
            AuditSeverity::Warning => "warn",
            AuditSeverity::Info => "info",
        };
        if let Some(alias) = &finding.alias {
            println!("[{severity}] {alias}: {}", finding.message);
        } else {
            println!("[{severity}] {}", finding.message);
        }
    }
}

fn print_artifact_drift_report(report: &ArtifactDriftReport) {
    println!(
        "current artifact version: {}",
        report.current_artifact_version
    );
    if let Some(version) = &report.vendored_artifact_version {
        println!("vendored artifact version: {version}");
    } else {
        println!("vendored artifact version: <missing>");
    }
    println!("schema version:           {}", report.schema_version);
    if let Some(version) = report.vendored_schema_version {
        println!("vendored schema version:  {version}");
    }
    if report.differences.is_empty() {
        println!("Vendored manifest matches the current Harn protocol contract.");
    } else {
        println!("Differences:");
        for diff in &report.differences {
            println!("- {diff}");
        }
    }
}

fn print_json<T: Serialize>(value: &T) {
    let body = serde_json::to_string_pretty(value)
        .unwrap_or_else(|error| format!(r#"{{"error":"{error}"}}"#));
    println!("{body}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package::test_support::*;

    #[test]
    fn lockfile_records_generator_protocol_and_per_entry_provenance() {
        let (_repo_tmp, repo, _branch) = create_git_package_repo();
        let project_tmp = tempfile::tempdir().unwrap();
        let root = project_tmp.path();
        let workspace = TestWorkspace::new(root);
        fs::create_dir_all(root.join(".git")).unwrap();
        let git = normalize_git_url(repo.to_string_lossy().as_ref()).unwrap();
        fs::write(
            root.join(MANIFEST),
            format!(
                r#"
[package]
name = "workspace"
version = "0.1.0"

[dependencies]
acme-lib = {{ git = "{git}", rev = "v1.0.0" }}
"#
            ),
        )
        .unwrap();

        install_packages_in(workspace.env(), false, None, false).unwrap();
        let lock = LockFile::load(&root.join(LOCK_FILE)).unwrap().unwrap();
        assert_eq!(lock.version, LOCK_FILE_VERSION);
        assert_eq!(lock.generator_version, env!("CARGO_PKG_VERSION"));
        assert_eq!(lock.protocol_artifact_version, env!("CARGO_PKG_VERSION"));
        let entry = lock.find("acme-lib").unwrap();
        assert_eq!(entry.package_version.as_deref(), Some("0.1.0"));
        assert!(entry
            .manifest_digest
            .as_deref()
            .is_some_and(|digest| digest.starts_with("sha256:")));
    }

    #[test]
    fn lockfile_v1_loads_and_v2_save_backfills_provenance() {
        let (_repo_tmp, repo, _branch) = create_git_package_repo();
        let project_tmp = tempfile::tempdir().unwrap();
        let root = project_tmp.path();
        let workspace = TestWorkspace::new(root);
        fs::create_dir_all(root.join(".git")).unwrap();
        let git = normalize_git_url(repo.to_string_lossy().as_ref()).unwrap();
        fs::write(
            root.join(MANIFEST),
            format!(
                r#"
[package]
name = "workspace"
version = "0.1.0"

[dependencies]
acme-lib = {{ git = "{git}", rev = "v1.0.0" }}
"#
            ),
        )
        .unwrap();

        install_packages_in(workspace.env(), false, None, false).unwrap();
        let lock_path = root.join(LOCK_FILE);
        let lock = LockFile::load(&lock_path).unwrap().unwrap();
        let entry = lock.find("acme-lib").unwrap();

        // Hand-write a v1 lock missing provenance to simulate an older
        // checkout, then re-run install and verify the rewrite includes
        // the new fields without forcing the user to delete the file.
        let v1 = format!(
            "version = 1\n\n[[package]]\nname = \"acme-lib\"\nsource = \"{}\"\nrev_request = \"v1.0.0\"\ncommit = \"{}\"\ncontent_hash = \"{}\"\n",
            entry.source,
            entry.commit.as_deref().unwrap(),
            entry.content_hash.as_deref().unwrap(),
        );
        fs::write(&lock_path, v1).unwrap();

        install_packages_in(workspace.env(), false, None, false).unwrap();
        let upgraded = LockFile::load(&lock_path).unwrap().unwrap();
        assert_eq!(upgraded.version, LOCK_FILE_VERSION);
        let upgraded_entry = upgraded.find("acme-lib").unwrap();
        assert!(upgraded_entry.package_version.is_some());
        assert!(upgraded_entry.manifest_digest.is_some());
    }

    #[test]
    fn outdated_marks_path_dependencies_as_skipped() {
        let dependency_tmp = tempfile::tempdir().unwrap();
        let dep_root = dependency_tmp.path().join("openapi");
        fs::create_dir_all(&dep_root).unwrap();
        fs::write(
            dep_root.join(MANIFEST),
            r#"
[package]
name = "openapi"
version = "0.1.0"
"#,
        )
        .unwrap();
        fs::write(
            dep_root.join("lib.harn"),
            "pub fn version() -> string { return \"v1\" }\n",
        )
        .unwrap();

        let project_tmp = tempfile::tempdir().unwrap();
        let root = project_tmp.path();
        let workspace = TestWorkspace::new(root);
        fs::create_dir_all(root.join(".git")).unwrap();
        let dep_path = dep_root.display().to_string();
        fs::write(
            root.join(MANIFEST),
            format!(
                r#"
[package]
name = "workspace"
version = "0.1.0"

[dependencies]
openapi = {{ path = "{dep_path}" }}
"#
            ),
        )
        .unwrap();

        install_packages_in(workspace.env(), false, None, false).unwrap();
        let report = outdated_packages_in(workspace.env(), false, false, None).unwrap();
        let entry = report
            .entries
            .iter()
            .find(|entry| entry.alias == "openapi")
            .expect("openapi entry");
        assert_eq!(entry.kind, "path");
        assert!(matches!(entry.status, OutdatedStatus::Skipped));
    }

    #[test]
    fn audit_reports_missing_lock_as_error() {
        let project_tmp = tempfile::tempdir().unwrap();
        let root = project_tmp.path();
        let workspace = TestWorkspace::new(root);
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(
            root.join(MANIFEST),
            r#"
[package]
name = "workspace"
version = "0.1.0"
"#,
        )
        .unwrap();

        let report = audit_packages_in(workspace.env(), None, true).unwrap();
        assert!(!report.ok);
        assert!(report
            .findings
            .iter()
            .any(|finding| matches!(finding.code, AuditCode::LockfileMissing)));
    }

    #[test]
    fn audit_flags_content_hash_tampering() {
        let (_repo_tmp, repo, _branch) = create_git_package_repo();
        let project_tmp = tempfile::tempdir().unwrap();
        let root = project_tmp.path();
        let workspace = TestWorkspace::new(root);
        fs::create_dir_all(root.join(".git")).unwrap();
        let git = normalize_git_url(repo.to_string_lossy().as_ref()).unwrap();
        fs::write(
            root.join(MANIFEST),
            format!(
                r#"
[package]
name = "workspace"
version = "0.1.0"

[dependencies]
acme-lib = {{ git = "{git}", rev = "v1.0.0" }}
"#
            ),
        )
        .unwrap();
        install_packages_in(workspace.env(), false, None, false).unwrap();
        let lock = LockFile::load(&root.join(LOCK_FILE)).unwrap().unwrap();
        let entry = lock.find("acme-lib").unwrap();
        let cache_dir = git_cache_dir_in(
            workspace.env(),
            &entry.source,
            entry.commit.as_deref().unwrap(),
        )
        .unwrap();
        fs::write(
            cache_dir.join("lib.harn"),
            "pub fn value() { return \"pwned\" }\n",
        )
        .unwrap();

        let report = audit_packages_in(workspace.env(), None, false).unwrap();
        assert!(!report.ok);
        assert!(report
            .findings
            .iter()
            .any(|finding| matches!(finding.code, AuditCode::ContentHashMismatch)));
    }

    #[test]
    fn artifacts_check_detects_drift_against_stale_vendored_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("manifest.json");
        let stale = serde_json::json!({
            "schemaVersion": 1,
            "artifactVersion": "0.0.0",
            "generatedBy": "harn dump-protocol-artifacts",
        });
        fs::write(&path, serde_json::to_string_pretty(&stale).unwrap() + "\n").unwrap();
        let report = check_artifact_manifest(&path).unwrap();
        assert!(!report.ok);
        assert_eq!(report.vendored_artifact_version.as_deref(), Some("0.0.0"));
        assert!(!report.differences.is_empty());
    }

    #[test]
    fn artifacts_check_passes_for_current_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("manifest.json");
        let current = crate::commands::dump_protocol_artifacts::manifest_json().unwrap();
        fs::write(&path, current).unwrap();
        let report = check_artifact_manifest(&path).unwrap();
        assert!(report.ok, "expected no drift, got {:?}", report.differences);
    }

    #[test]
    fn install_is_deterministic_across_repeated_runs() {
        let (_repo_tmp, repo, _branch) = create_git_package_repo();
        let project_tmp = tempfile::tempdir().unwrap();
        let root = project_tmp.path();
        let workspace = TestWorkspace::new(root);
        fs::create_dir_all(root.join(".git")).unwrap();
        let git = normalize_git_url(repo.to_string_lossy().as_ref()).unwrap();
        fs::write(
            root.join(MANIFEST),
            format!(
                r#"
[package]
name = "workspace"
version = "0.1.0"

[dependencies]
acme-lib = {{ git = "{git}", rev = "v1.0.0" }}
"#
            ),
        )
        .unwrap();

        install_packages_in(workspace.env(), false, None, false).unwrap();
        let first = fs::read(root.join(LOCK_FILE)).unwrap();
        install_packages_in(workspace.env(), false, None, false).unwrap();
        let second = fs::read(root.join(LOCK_FILE)).unwrap();
        assert_eq!(
            first, second,
            "harn.lock must be byte-for-byte stable across repeated installs"
        );
    }

    #[test]
    fn outdated_reports_registry_provenance_when_index_lists_newer_version() {
        let (_repo_tmp, repo, _branch) = create_git_package_repo();
        let project_tmp = tempfile::tempdir().unwrap();
        let root = project_tmp.path();
        let registry_path = root.join("index.toml");
        let workspace =
            TestWorkspace::new(root).with_registry_source(registry_path.display().to_string());
        let git = normalize_git_url(repo.to_string_lossy().as_ref()).unwrap();
        let harn_range = current_harn_range_example();
        fs::write(
            &registry_path,
            format!(
                r#"
version = 1

[[package]]
name = "@burin/acme-lib"
description = "Acme package for tests"
repository = "{git}"
license = "MIT"
harn = "{harn_range}"

[[package.version]]
version = "1.0.0"
git = "{git}"
rev = "v1.0.0"
package = "acme-lib"

[[package.version]]
version = "1.1.0"
git = "{git}"
rev = "v1.0.0"
package = "acme-lib"
"#
            ),
        )
        .unwrap();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(
            root.join(MANIFEST),
            r#"
[package]
name = "workspace"
version = "0.1.0"
"#,
        )
        .unwrap();

        add_package_to(
            workspace.env(),
            "@burin/acme-lib@1.0.0",
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();

        let report = outdated_packages_in(workspace.env(), false, false, None).unwrap();
        let entry = report
            .entries
            .iter()
            .find(|entry| entry.alias == "acme-lib")
            .expect("acme-lib entry");
        assert_eq!(entry.kind, "registry");
        assert_eq!(entry.registry_name.as_deref(), Some("@burin/acme-lib"));
        assert_eq!(entry.latest_version.as_deref(), Some("1.1.0"));
        assert!(matches!(entry.status, OutdatedStatus::Outdated));
    }
}
