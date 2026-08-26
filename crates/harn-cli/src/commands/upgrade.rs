//! Self-update for the `harn` CLI.
//!
//! Mirrors the logic in `install.sh`: resolves a target release on
//! GitHub, downloads the matching tarball, verifies it against the
//! release's `SHA256SUMS` manifest, then atomically replaces the
//! currently running binary. macOS binaries are notarized at release
//! time, so Gatekeeper does the cryptographic identity check on first
//! launch after replacement.

use std::env;
use std::fs;
use std::io::Read;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::cli::UpgradeArgs;
use crate::json_envelope::{self, JsonEnvelope};
use crate::net;

mod hook_runtime;

/// Schema version for `harn upgrade --json`.
pub(crate) const UPGRADE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize)]
pub(crate) struct UpgradeReport {
    /// Currently installed `harn` version (without leading `v`).
    pub current: String,
    /// Resolved target release tag (with leading `v`).
    pub target: String,
    /// `true` when the resolved target differs from the installed version.
    pub needs_upgrade: bool,
    /// Whether this invocation only resolved the target (`--check`) or
    /// actually performed an install.
    pub mode: UpgradeMode,
    /// `true` when an install actually took place. Always `false` for
    /// `--check`; `false` for re-runs against the same version unless
    /// `--force` was set.
    pub installed: bool,
    /// Resolved target archive URL, populated for both modes so agents
    /// can plan side-band downloads if desired.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archive_url: Option<String>,
    /// Resolved SHA256SUMS URL paired with `archive_url`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checksums_url: Option<String>,
    /// Target triple resolved at compile time, for log/telemetry use.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_triple: Option<String>,
    /// Outcome for the independently enrolled standalone hook runtime.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hook_runtime: Option<hook_runtime::HookRuntimeRefreshReport>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum UpgradeMode {
    Check,
    Install,
}

const REPO: &str = "burin-labs/harn";
const RELEASES_BASE: &str = "https://github.com/burin-labs/harn/releases";
const USER_AGENT: &str = concat!("harn-cli/", env!("CARGO_PKG_VERSION"));
const REQUEST_TIMEOUT: Duration = Duration::from_mins(1);
const MAX_TAG_PEEL_DEPTH: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChecksumVerification {
    Verified,
    Unavailable(u16),
}

/// `harn upgrade` lives on tokio's blocking pool so the synchronous
/// `reqwest::blocking` / `tar::Archive` / `fs::rename` calls below can
/// share a single straight-line implementation. The CLI dispatcher
/// runs inside a tokio multi-thread runtime, where dropping a nested
/// blocking client on the async thread itself panics.
pub(crate) async fn run(args: UpgradeArgs) -> Result<(), String> {
    if args.json {
        let exit = tokio::task::spawn_blocking(move || run_blocking_json(args))
            .await
            .map_err(|error| format!("upgrade task failed: {error}"))?;
        if exit != 0 {
            std::process::exit(exit);
        }
        return Ok(());
    }
    tokio::task::spawn_blocking(move || run_blocking(args))
        .await
        .map_err(|error| format!("upgrade task failed: {error}"))?
}

fn run_blocking(args: UpgradeArgs) -> Result<(), String> {
    let triple = target_triple()?;
    let current = env!("CARGO_PKG_VERSION");

    let target = match args.version.as_deref() {
        Some(v) => normalize_version(v)?,
        None => fetch_latest_tag()?,
    };

    println!("Installed: v{current}");
    println!("Target:    {target}");

    if args.check {
        return Ok(());
    }

    if !args.force && target.trim_start_matches('v') == current {
        println!("Already on the latest release.");
        return Ok(());
    }

    let current_exe =
        env::current_exe().map_err(|error| format!("failed to resolve current exe: {error}"))?;
    let current_exe = current_exe
        .canonicalize()
        .unwrap_or_else(|_| current_exe.clone());
    let install_dir = current_exe
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", current_exe.display()))?
        .to_path_buf();

    let archive_name = format!("harn-{triple}.tar.gz");
    let archive_url = format!("{RELEASES_BASE}/download/{target}/{archive_name}");
    let checksums_url = format!("{RELEASES_BASE}/download/{target}/SHA256SUMS");

    let staging = tempfile::tempdir()
        .map_err(|error| format!("failed to create staging directory: {error}"))?;
    let archive_path = staging.path().join(&archive_name);

    println!("Downloading {archive_name}");
    download(&archive_url, &archive_path)?;

    let verification = if args.no_verify {
        eprintln!("warning: SHA256 verification skipped (--no-verify)");
        ChecksumVerification::Unavailable(0)
    } else {
        let result = verify_checksum(&checksums_url, &archive_name, &archive_path)?;
        match result {
            ChecksumVerification::Verified => println!("Verified SHA256 ({archive_name})"),
            ChecksumVerification::Unavailable(status) => eprintln!(
                "warning: no SHA256SUMS published for this release (status {status}); skipping verification"
            ),
        }
        result
    };

    println!("Extracting");
    extract_tarball(&archive_path, staging.path())?;

    let staged_binary = staging.path().join(harn_binary_name());
    if !staged_binary.exists() {
        return Err(format!(
            "archive did not contain {} — refusing to upgrade",
            harn_binary_name(),
        ));
    }

    let early_hook_report = refresh_hook_runtime_before_shared_install(
        verification,
        &target,
        &staged_binary,
        &install_dir.join(harn_binary_name()),
    )?;
    install_binaries(staging.path(), &install_dir)?;

    let hook_report = match early_hook_report {
        Some(report) => report,
        None => refresh_hook_runtime(verification, &target, &staged_binary).map_err(|error| {
            format!(
                "harn {target} was installed, but its enrolled hook runtime was not refreshed: {error}"
            )
        })?,
    };
    print_hook_runtime_outcome(&hook_report);

    println!("Upgraded harn to {target}. Re-run your last command to use the new binary.");
    Ok(())
}

/// JSON variant. Mirrors `run_blocking` but emits a [`JsonEnvelope`]
/// to stdout and routes every log line to stderr to keep stdout
/// single-document parseable.
fn run_blocking_json(args: UpgradeArgs) -> i32 {
    let triple = match target_triple() {
        Ok(t) => t,
        Err(error) => return emit_upgrade_error("unsupported_target", error),
    };
    let current = env!("CARGO_PKG_VERSION");

    let target = match args.version.as_deref() {
        Some(v) => match normalize_version(v) {
            Ok(t) => t,
            Err(error) => return emit_upgrade_error("invalid_version", error),
        },
        None => match fetch_latest_tag() {
            Ok(t) => t,
            Err(error) => return emit_upgrade_error("resolve_failed", error),
        },
    };

    let archive_name = format!("harn-{triple}.tar.gz");
    let archive_url = format!("{RELEASES_BASE}/download/{target}/{archive_name}");
    let checksums_url = format!("{RELEASES_BASE}/download/{target}/SHA256SUMS");
    let needs_upgrade = target.trim_start_matches('v') != current;
    let mode = if args.check {
        UpgradeMode::Check
    } else {
        UpgradeMode::Install
    };
    let mut report = UpgradeReport {
        current: current.to_string(),
        target: target.clone(),
        needs_upgrade,
        mode,
        installed: false,
        archive_url: Some(archive_url.clone()),
        checksums_url: Some(checksums_url.clone()),
        target_triple: Some(triple.to_string()),
        hook_runtime: None,
    };

    if args.check {
        emit_upgrade_ok(report);
        return 0;
    }
    if !args.force && !needs_upgrade {
        emit_upgrade_ok(report);
        return 0;
    }

    // Logs go to stderr so stdout stays a single parseable envelope.
    eprintln!("Installed: v{current}");
    eprintln!("Target:    {target}");

    let current_exe = match env::current_exe()
        .map_err(|error| format!("failed to resolve current exe: {error}"))
    {
        Ok(p) => p,
        Err(error) => return emit_upgrade_error_with(&report, "resolve_exe_failed", error),
    };
    let current_exe = current_exe
        .canonicalize()
        .unwrap_or_else(|_| current_exe.clone());
    let install_dir = match current_exe
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", current_exe.display()))
    {
        Ok(p) => p.to_path_buf(),
        Err(error) => return emit_upgrade_error_with(&report, "resolve_exe_failed", error),
    };

    let staging = match tempfile::tempdir()
        .map_err(|error| format!("failed to create staging directory: {error}"))
    {
        Ok(s) => s,
        Err(error) => return emit_upgrade_error_with(&report, "staging_failed", error),
    };
    let archive_path = staging.path().join(&archive_name);

    eprintln!("Downloading {archive_name}");
    if let Err(error) = download(&archive_url, &archive_path) {
        return emit_upgrade_error_with(&report, "download_failed", error);
    }

    let verification = if args.no_verify {
        eprintln!("warning: SHA256 verification skipped (--no-verify)");
        ChecksumVerification::Unavailable(0)
    } else {
        match verify_checksum(&checksums_url, &archive_name, &archive_path) {
            Ok(ChecksumVerification::Verified) => {
                eprintln!("Verified SHA256 ({archive_name})");
                ChecksumVerification::Verified
            }
            Ok(ChecksumVerification::Unavailable(status)) => {
                eprintln!(
                    "warning: no SHA256SUMS published for this release (status {status}); skipping verification"
                );
                ChecksumVerification::Unavailable(status)
            }
            Err(error) => return emit_upgrade_error_with(&report, "checksum_failed", error),
        }
    };

    eprintln!("Extracting");
    if let Err(error) = extract_tarball(&archive_path, staging.path()) {
        return emit_upgrade_error_with(&report, "extract_failed", error);
    }

    let staged_binary = staging.path().join(harn_binary_name());
    if !staged_binary.exists() {
        return emit_upgrade_error_with(
            &report,
            "archive_missing_binary",
            format!(
                "archive did not contain {} — refusing to upgrade",
                harn_binary_name()
            ),
        );
    }

    let early_hook_report = match refresh_hook_runtime_before_shared_install(
        verification,
        &target,
        &staged_binary,
        &install_dir.join(harn_binary_name()),
    ) {
        Ok(report) => report,
        Err(error) => {
            return emit_upgrade_error_with(&report, "hook_runtime_refresh_failed", error);
        }
    };
    if let Some(hook_report) = early_hook_report.clone() {
        report.hook_runtime = Some(hook_report);
    }
    if let Err(error) = install_binaries(staging.path(), &install_dir) {
        return emit_upgrade_error_with(&report, "install_failed", error);
    }

    report.installed = true;
    let hook_refresh = match early_hook_report {
        Some(hook_report) => Ok(hook_report),
        None => refresh_hook_runtime(verification, &target, &staged_binary),
    };
    match hook_refresh {
        Ok(hook_report) => {
            print_hook_runtime_outcome(&hook_report);
            report.hook_runtime = Some(hook_report);
        }
        Err(error) => {
            return emit_upgrade_error_with(&report, "hook_runtime_refresh_failed", error);
        }
    }
    emit_upgrade_ok(report);
    0
}

fn emit_upgrade_ok(report: UpgradeReport) {
    let envelope = JsonEnvelope::ok(UPGRADE_SCHEMA_VERSION, report);
    println!("{}", json_envelope::to_string_pretty(&envelope));
}

fn emit_upgrade_error(code: &str, message: String) -> i32 {
    let envelope: JsonEnvelope<UpgradeReport> =
        JsonEnvelope::err(UPGRADE_SCHEMA_VERSION, code, message);
    println!("{}", json_envelope::to_string_pretty(&envelope));
    1
}

fn emit_upgrade_error_with(report: &UpgradeReport, code: &str, message: String) -> i32 {
    let envelope = JsonEnvelope {
        schema_version: UPGRADE_SCHEMA_VERSION,
        ok: false,
        data: Some(report.clone()),
        error: Some(json_envelope::JsonError {
            code: code.to_string(),
            message,
            details: serde_json::Value::Null,
        }),
        warnings: Vec::new(),
    };
    println!("{}", json_envelope::to_string_pretty(&envelope));
    1
}

/// Compile-time host target triple for selecting the matching release
/// archive. Keep this list in sync with the targets produced by
/// `.github/workflows/build-release-binaries.yml`.
fn target_triple() -> Result<&'static str, String> {
    let triple = if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "x86_64-apple-darwin"
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "aarch64-apple-darwin"
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "x86_64-unknown-linux-gnu"
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        "aarch64-unknown-linux-gnu"
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        return Err(
            "`harn upgrade` is not implemented for Windows yet; download the zip from \
                    https://github.com/burin-labs/harn/releases and replace harn.exe manually"
                .to_string(),
        );
    } else {
        return Err("self-update is not supported on this target".to_string());
    };
    Ok(triple)
}

fn harn_binary_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "harn.exe"
    } else {
        "harn"
    }
}

fn extra_binary_names() -> &'static [&'static str] {
    if cfg!(target_os = "windows") {
        &["harn-dap.exe", "harn-lsp.exe"]
    } else {
        &["harn-dap", "harn-lsp"]
    }
}

fn normalize_version(input: &str) -> Result<String, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("--version cannot be empty".to_string());
    }
    let normalized = if trimmed.starts_with('v') {
        trimmed.to_string()
    } else {
        format!("v{trimmed}")
    };
    let rest = normalized.trim_start_matches('v');
    let parts: Vec<&str> = rest.split('.').collect();
    if parts.len() != 3
        || parts
            .iter()
            .any(|p| p.is_empty() || !p.chars().all(|c| c.is_ascii_digit()))
    {
        return Err(format!("invalid version: {input} (expected vX.Y.Z)"));
    }
    Ok(normalized)
}

fn http_client() -> Result<reqwest::blocking::Client, String> {
    net::blocking_http_client_builder("cli.upgrade")
        .user_agent(USER_AGENT)
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|error| {
            format!(
                "failed to build HTTP client: {}",
                net::reqwest_error(&error)
            )
        })
}

fn fetch_latest_tag() -> Result<String, String> {
    let client = http_client()?;
    let response = client
        .get(format!(
            "https://api.github.com/repos/{REPO}/releases/latest"
        ))
        .header("Accept", "application/vnd.github+json")
        .send()
        .map_err(|error| {
            format!(
                "failed to query latest release: {}",
                net::reqwest_error(&error)
            )
        })?;
    if !response.status().is_success() {
        return Err(format!(
            "GitHub API returned status {} when resolving latest release",
            response.status()
        ));
    }
    let body: serde_json::Value = response
        .json()
        .map_err(|error| format!("failed to parse latest release response: {error}"))?;
    let tag = body
        .get("tag_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "GitHub API response did not include a tag_name".to_string())?;
    Ok(tag.to_string())
}

#[derive(Debug, Deserialize)]
struct GitObjectEnvelope {
    object: GitObject,
}

#[derive(Clone, Debug, Deserialize)]
struct GitObject {
    #[serde(rename = "type")]
    kind: String,
    sha: String,
}

fn fetch_release_source_revision(tag: &str) -> Result<String, String> {
    let client = http_client()?;
    peel_release_tag(tag, |kind, identifier| {
        let url = format!("https://api.github.com/repos/{REPO}/git/{kind}/{identifier}");
        let response = client
            .get(&url)
            .header("Accept", "application/vnd.github+json")
            .send()
            .map_err(|error| {
                format!(
                    "failed to resolve release source revision: {}",
                    net::reqwest_error(&error)
                )
            })?;
        if !response.status().is_success() {
            return Err(format!(
                "GitHub API returned status {} when resolving release source revision",
                response.status()
            ));
        }
        response
            .json::<GitObjectEnvelope>()
            .map(|body| body.object)
            .map_err(|error| format!("failed to parse release tag response: {error}"))
    })
}

fn peel_release_tag(
    tag: &str,
    mut fetch: impl FnMut(&str, &str) -> Result<GitObject, String>,
) -> Result<String, String> {
    let mut object = fetch("ref/tags", tag)?;
    let mut seen = std::collections::HashSet::new();
    for _ in 0..MAX_TAG_PEEL_DEPTH {
        validate_git_sha(&object.sha)?;
        if !seen.insert(object.sha.clone()) {
            return Err("release tag contains a cycle".to_string());
        }
        match object.kind.as_str() {
            "commit" => return Ok(object.sha),
            "tag" => object = fetch("tags", &object.sha)?,
            kind => {
                return Err(format!(
                    "release tag points to unsupported Git object type {kind:?}"
                ));
            }
        }
    }
    Err(format!(
        "release tag exceeds the maximum peel depth of {MAX_TAG_PEEL_DEPTH}"
    ))
}

fn validate_git_sha(sha: &str) -> Result<(), String> {
    if sha.len() == 40
        && sha
            .chars()
            .all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase())
    {
        Ok(())
    } else {
        Err("release tag object SHA must be 40 lowercase hex characters".to_string())
    }
}

fn refresh_hook_runtime(
    verification: ChecksumVerification,
    target: &str,
    candidate: &Path,
) -> Result<hook_runtime::HookRuntimeRefreshReport, String> {
    let runtime_path = hook_runtime::enrolled_runtime_path()?;
    refresh_hook_runtime_at(
        runtime_path.as_deref(),
        verification,
        target,
        candidate,
        fetch_release_source_revision,
    )
}

fn refresh_hook_runtime_before_shared_install(
    verification: ChecksumVerification,
    target: &str,
    candidate: &Path,
    install_destination: &Path,
) -> Result<Option<hook_runtime::HookRuntimeRefreshReport>, String> {
    let runtime_path = hook_runtime::configured_runtime_path();
    refresh_hook_runtime_before_shared_install_at(
        runtime_path.as_deref(),
        verification,
        target,
        candidate,
        install_destination,
        fetch_release_source_revision,
    )
}

fn refresh_hook_runtime_before_shared_install_at(
    runtime_path: Option<&Path>,
    verification: ChecksumVerification,
    target: &str,
    candidate: &Path,
    install_destination: &Path,
    resolve_source_revision: impl FnOnce(&str) -> Result<String, String>,
) -> Result<Option<hook_runtime::HookRuntimeRefreshReport>, String> {
    let Some(runtime_path) = runtime_path else {
        return Ok(None);
    };
    if !paths_refer_to_same_file(runtime_path, install_destination) {
        return Ok(None);
    }
    if !hook_runtime::runtime_is_enrolled(runtime_path)? {
        return Ok(None);
    }
    if !matches!(verification, ChecksumVerification::Verified) {
        return Err(
            "refusing an unverified upgrade because the current executable is the enrolled standalone hook runtime"
                .to_string(),
        );
    }
    refresh_hook_runtime_at(
        Some(runtime_path),
        verification,
        target,
        candidate,
        resolve_source_revision,
    )
    .map(Some)
}

fn paths_refer_to_same_file(left: &Path, right: &Path) -> bool {
    left == right
        || left
            .canonicalize()
            .ok()
            .zip(right.canonicalize().ok())
            .is_some_and(|(left, right)| left == right)
}

fn refresh_hook_runtime_at(
    runtime_path: Option<&Path>,
    verification: ChecksumVerification,
    target: &str,
    candidate: &Path,
    resolve_source_revision: impl FnOnce(&str) -> Result<String, String>,
) -> Result<hook_runtime::HookRuntimeRefreshReport, String> {
    let Some(runtime_path) = runtime_path else {
        return Ok(hook_runtime::HookRuntimeRefreshReport::not_enrolled());
    };
    if !matches!(verification, ChecksumVerification::Verified) {
        return Ok(hook_runtime::HookRuntimeRefreshReport::skipped_unverified());
    }
    let release = hook_runtime::HookRuntimeRelease {
        version: target.to_string(),
        source_revision: resolve_source_revision(target)?,
    };
    hook_runtime::refresh_runtime_if_enrolled(runtime_path, candidate, &release)
}

fn print_hook_runtime_outcome(report: &hook_runtime::HookRuntimeRefreshReport) {
    use hook_runtime::HookRuntimeRefreshStatus;

    match report.status {
        HookRuntimeRefreshStatus::NotEnrolled => {}
        HookRuntimeRefreshStatus::Refreshed => {
            eprintln!("Refreshed enrolled standalone hook runtime.");
        }
        HookRuntimeRefreshStatus::SkippedUnverified => eprintln!(
            "warning: enrolled standalone hook runtime was not refreshed because the archive was not checksum-verified"
        ),
    }
}

fn download(url: &str, dest: &Path) -> Result<(), String> {
    let client = http_client()?;
    let mut response = client.get(url).send().map_err(|error| {
        format!(
            "failed to download {}: {}",
            net::diagnostic_text(url),
            net::reqwest_error(&error)
        )
    })?;
    if !response.status().is_success() {
        return Err(format!(
            "download {} returned status {}",
            net::diagnostic_text(url),
            response.status()
        ));
    }
    let mut file = fs::File::create(dest)
        .map_err(|error| format!("failed to create {}: {error}", dest.display()))?;
    response
        .copy_to(&mut file)
        .map_err(|error| format!("failed to write {}: {error}", dest.display()))?;
    Ok(())
}

fn verify_checksum(
    checksums_url: &str,
    asset: &str,
    path: &Path,
) -> Result<ChecksumVerification, String> {
    let client = http_client()?;
    let response = client
        .get(checksums_url)
        .send()
        .map_err(|error| format!("failed to fetch SHA256SUMS: {}", net::reqwest_error(&error)))?;
    if !response.status().is_success() {
        // Releases predating the SHA256SUMS workflow step won't have
        // this manifest. Match install.sh's behavior and warn-and-skip
        // rather than refusing — TLS to github.com is still in place.
        return Ok(ChecksumVerification::Unavailable(
            response.status().as_u16(),
        ));
    }
    let manifest = response.text().map_err(|error| {
        format!(
            "failed to read SHA256SUMS body: {}",
            net::reqwest_error(&error)
        )
    })?;

    let expected = find_expected_sha(&manifest, asset)
        .ok_or_else(|| format!("SHA256SUMS does not include an entry for {asset}"))?;

    let actual = file_sha256_hex(path)?;
    if actual != expected {
        return Err(format!(
            "SHA256 mismatch for {asset}: expected {expected}, got {actual}"
        ));
    }
    Ok(ChecksumVerification::Verified)
}

fn file_sha256_hex(path: &Path) -> Result<String, String> {
    let mut file = fs::File::open(path)
        .map_err(|error| format!("failed to open {}: {error}", path.display()))?;
    let mut hasher = Sha256::new();
    // 64 KiB I/O buffer — fine on the stack on every platform we ship to.
    #[allow(clippy::large_stack_arrays)]
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex_lower(&hasher.finalize()))
}

/// Parse a coreutils-style SHA256SUMS manifest and return the digest
/// for `asset`. Tolerates the leading `*` binary-mode marker and any
/// directory prefix on the filename.
fn find_expected_sha(manifest: &str, asset: &str) -> Option<String> {
    for line in manifest.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.splitn(2, char::is_whitespace);
        let digest = parts.next()?.trim();
        let name = parts.next()?.trim().trim_start_matches('*');
        let basename = Path::new(name)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(name);
        if basename == asset {
            return Some(digest.to_ascii_lowercase());
        }
    }
    None
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn extract_tarball(archive: &Path, dest: &Path) -> Result<(), String> {
    let file = fs::File::open(archive)
        .map_err(|error| format!("failed to open archive {}: {error}", archive.display()))?;
    let gz = flate2::read::GzDecoder::new(file);
    let mut tar = tar::Archive::new(gz);
    tar.unpack(dest)
        .map_err(|error| format!("failed to extract archive: {error}"))?;
    Ok(())
}

fn install_binaries(staging: &Path, install_dir: &Path) -> Result<(), String> {
    let main = harn_binary_name();
    let main_src = staging.join(main);
    if !main_src.exists() {
        return Err(format!("staging directory missing {main}"));
    }
    atomic_replace(&main_src, &install_dir.join(main))?;

    // harn-lsp / harn-dap are the same multi-call `harn` binary reached via
    // argv[0] dispatch. Install them as symlinks to `harn` so an upgrade
    // leaves one real binary plus two tiny links rather than three full
    // copies. Sibling tools are best-effort (skip if absent from staging).
    // Windows lacks dependable unprivileged symlinks, so copy there — the
    // staging dir already holds real per-name copies on that platform.
    for name in extra_binary_names() {
        let src = staging.join(name);
        if !src.exists() {
            continue;
        }
        let dest = install_dir.join(name);
        #[cfg(unix)]
        atomic_symlink(main, &dest)?;
        #[cfg(not(unix))]
        atomic_replace(&src, &dest)?;
    }
    Ok(())
}

fn atomic_replace(src: &Path, dest: &Path) -> Result<(), String> {
    // Stage a sibling temp file inside the destination directory so the
    // rename is intra-filesystem and therefore atomic on POSIX. Copying
    // from the (potentially tmpfs) staging dir into the same directory
    // as `dest` ensures the final rename stays inside one filesystem.
    let parent = dest
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", dest.display()))?;
    let dest_basename = dest
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| format!("{} has no file name", dest.display()))?;
    let temp_in_dest = parent.join(format!(
        ".{dest_basename}.harn-upgrade-{pid}-{counter}",
        pid = std::process::id(),
        counter = next_upgrade_counter(),
    ));
    if let Err(error) = fs::copy(src, &temp_in_dest) {
        return Err(format!(
            "failed to stage {} -> {}: {error}",
            src.display(),
            temp_in_dest.display(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o755);
        if let Err(error) = fs::set_permissions(&temp_in_dest, perms) {
            let _ = fs::remove_file(&temp_in_dest);
            return Err(format!("failed to chmod staged binary: {error}"));
        }
    }
    if let Err(error) = fs::File::open(&temp_in_dest).and_then(|file| file.sync_all()) {
        let _ = fs::remove_file(&temp_in_dest);
        return Err(format!("failed to sync staged binary: {error}"));
    }
    if let Err(error) = fs::rename(&temp_in_dest, dest) {
        let _ = fs::remove_file(&temp_in_dest);
        return Err(format!("failed to replace {}: {error}", dest.display()));
    }
    sync_parent_directory(parent)
}

#[cfg(unix)]
fn sync_parent_directory(parent: &Path) -> Result<(), String> {
    fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("failed to sync {}: {error}", parent.display()))
}

#[cfg(not(unix))]
fn sync_parent_directory(_parent: &Path) -> Result<(), String> {
    Ok(())
}

/// Atomically place a relative symlink at `dest` pointing at `target_name`
/// (a sibling in the same directory). Mirrors `atomic_replace`'s stage-then-
/// rename so a concurrent reader never observes a half-written link.
#[cfg(unix)]
fn atomic_symlink(target_name: &str, dest: &Path) -> Result<(), String> {
    use std::os::unix::fs::symlink;
    let parent = dest
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", dest.display()))?;
    let dest_basename = dest
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| format!("{} has no file name", dest.display()))?;
    let temp_in_dest = parent.join(format!(
        ".{dest_basename}.harn-upgrade-{pid}-{counter}",
        pid = std::process::id(),
        counter = next_upgrade_counter(),
    ));
    let _ = fs::remove_file(&temp_in_dest);
    // Relative target so the link resolves to the sibling `harn` wherever the
    // install dir lives.
    if let Err(error) = symlink(target_name, &temp_in_dest) {
        return Err(format!(
            "failed to stage symlink {} -> {target_name}: {error}",
            temp_in_dest.display(),
        ));
    }
    if let Err(error) = fs::rename(&temp_in_dest, dest) {
        let _ = fs::remove_file(&temp_in_dest);
        return Err(format!("failed to replace {}: {error}", dest.display()));
    }
    sync_parent_directory(parent)
}

fn next_upgrade_counter() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    const SHA_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const SHA_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    #[test]
    fn normalize_version_accepts_v_prefix() {
        assert_eq!(normalize_version("v0.8.19").unwrap(), "v0.8.19");
    }

    #[test]
    fn normalize_version_adds_v_prefix() {
        assert_eq!(normalize_version("0.8.19").unwrap(), "v0.8.19");
    }

    #[test]
    fn normalize_version_rejects_garbage() {
        assert!(normalize_version("not-a-version").is_err());
        assert!(normalize_version("v0.8").is_err());
        assert!(normalize_version("").is_err());
        assert!(normalize_version("v0.8.x").is_err());
    }

    #[test]
    fn release_tag_peeling_handles_lightweight_and_annotated_tags() {
        let lightweight = peel_release_tag("v1.2.3", |kind, identifier| {
            assert_eq!((kind, identifier), ("ref/tags", "v1.2.3"));
            Ok(GitObject {
                kind: "commit".to_string(),
                sha: SHA_A.to_string(),
            })
        })
        .expect("lightweight tag");
        assert_eq!(lightweight, SHA_A);

        let calls = Cell::new(0);
        let annotated = peel_release_tag("v1.2.3", |kind, identifier| {
            let call = calls.get();
            calls.set(call + 1);
            match call {
                0 => {
                    assert_eq!((kind, identifier), ("ref/tags", "v1.2.3"));
                    Ok(GitObject {
                        kind: "tag".to_string(),
                        sha: SHA_A.to_string(),
                    })
                }
                1 => {
                    assert_eq!((kind, identifier), ("tags", SHA_A));
                    Ok(GitObject {
                        kind: "commit".to_string(),
                        sha: SHA_B.to_string(),
                    })
                }
                _ => panic!("unexpected peel request"),
            }
        })
        .expect("annotated tag");
        assert_eq!(annotated, SHA_B);
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn release_tag_peeling_rejects_cycles_and_untrusted_object_shapes() {
        let cycle = peel_release_tag("v1.2.3", |_kind, _identifier| {
            Ok(GitObject {
                kind: "tag".to_string(),
                sha: SHA_A.to_string(),
            })
        })
        .expect_err("cycle");
        assert!(cycle.contains("cycle"), "{cycle}");

        let malformed = peel_release_tag("v1.2.3", |_kind, _identifier| {
            Ok(GitObject {
                kind: "commit".to_string(),
                sha: "A".repeat(40),
            })
        })
        .expect_err("uppercase SHA");
        assert!(malformed.contains("lowercase hex"), "{malformed}");
    }

    #[test]
    fn unverified_upgrade_does_not_resolve_or_mutate_enrolled_runtime() {
        let root = tempfile::tempdir().expect("temp dir");
        let runtime = root.path().join("harn");
        let candidate = root.path().join("candidate");
        fs::write(&runtime, b"old-runtime").expect("runtime");
        fs::write(&candidate, b"new-runtime").expect("candidate");
        fs::write(
            root.path().join("harn.standalone-v1"),
            b"harn-run-standalone-v1\n",
        )
        .expect("marker");

        let report = refresh_hook_runtime_at(
            Some(&runtime),
            ChecksumVerification::Unavailable(404),
            "v1.2.3",
            &candidate,
            |_| panic!("unverified bytes must not resolve source provenance"),
        )
        .expect("unverified refresh is a typed skip");
        assert_eq!(
            report.status,
            hook_runtime::HookRuntimeRefreshStatus::SkippedUnverified
        );
        assert_eq!(fs::read(&runtime).expect("runtime"), b"old-runtime");
        assert!(!root.path().join("provenance-v1").exists());
        assert!(!root.path().join(".upgrade.lock").exists());
    }

    #[test]
    fn unverified_self_upgrade_of_enrolled_runtime_is_refused_before_mutation() {
        let root = tempfile::tempdir().expect("temp dir");
        let runtime = root.path().join("harn");
        let candidate = root.path().join("candidate");
        fs::write(&runtime, b"old-runtime").expect("runtime");
        fs::write(&candidate, b"new-runtime").expect("candidate");
        fs::write(
            root.path().join("harn.standalone-v1"),
            b"harn-run-standalone-v1\n",
        )
        .expect("marker");

        let error = refresh_hook_runtime_before_shared_install_at(
            Some(&runtime),
            ChecksumVerification::Unavailable(0),
            "v1.2.3",
            &candidate,
            &runtime,
            |_| panic!("unverified bytes must not resolve source provenance"),
        )
        .expect_err("unverified shared install");
        assert!(error.contains("refusing an unverified upgrade"), "{error}");
        assert_eq!(fs::read(&runtime).expect("runtime"), b"old-runtime");
        assert!(!root.path().join("provenance-v1").exists());
        assert!(!root.path().join(".upgrade.lock").exists());
    }

    #[test]
    fn unverified_self_upgrade_without_enrollment_remains_an_ordinary_install() {
        let root = tempfile::tempdir().expect("temp dir");
        let runtime = root.path().join("harn");
        let candidate = root.path().join("candidate");
        fs::write(&runtime, b"old-runtime").expect("runtime");
        fs::write(&candidate, b"new-runtime").expect("candidate");

        let report = refresh_hook_runtime_before_shared_install_at(
            Some(&runtime),
            ChecksumVerification::Unavailable(0),
            "v1.2.3",
            &candidate,
            &runtime,
            |_| panic!("unenrolled bytes must not resolve source provenance"),
        )
        .expect("unenrolled shared destination");
        assert!(report.is_none());
        assert_eq!(fs::read(&runtime).expect("runtime"), b"old-runtime");
        assert!(!root.path().join(".upgrade.lock").exists());
    }

    #[test]
    fn unrelated_hook_cache_is_not_read_before_the_primary_install() {
        let root = tempfile::tempdir().expect("temp dir");
        let runtime = root.path().join("hook-harn");
        let install_destination = root.path().join("installed-harn");
        let candidate = root.path().join("candidate");
        fs::write(&runtime, b"old-hook").expect("runtime");
        fs::write(&install_destination, b"old-main").expect("main");
        fs::write(&candidate, b"new-runtime").expect("candidate");
        fs::create_dir(root.path().join("hook-harn.standalone-v1"))
            .expect("unreadable marker shape");

        let report = refresh_hook_runtime_before_shared_install_at(
            Some(&runtime),
            ChecksumVerification::Verified,
            "v1.2.3",
            &candidate,
            &install_destination,
            |_| panic!("unrelated cache must not resolve source provenance"),
        )
        .expect("unrelated cache is deferred");
        assert!(report.is_none());
    }

    #[test]
    fn verified_self_upgrade_publishes_hook_pair_before_main_install() {
        let root = tempfile::tempdir().expect("temp dir");
        let runtime = root.path().join("harn");
        let candidate = root.path().join("candidate");
        fs::write(&runtime, b"old-runtime").expect("runtime");
        fs::write(&candidate, b"new-runtime").expect("candidate");
        fs::write(
            root.path().join("harn.standalone-v1"),
            b"harn-run-standalone-v1\n",
        )
        .expect("marker");

        let report = refresh_hook_runtime_before_shared_install_at(
            Some(&runtime),
            ChecksumVerification::Verified,
            "v1.2.3",
            &candidate,
            &runtime,
            |_| Ok(SHA_A.to_string()),
        )
        .expect("verified shared install")
        .expect("early hook refresh");
        assert_eq!(
            report.status,
            hook_runtime::HookRuntimeRefreshStatus::Refreshed
        );
        assert_eq!(fs::read(&runtime).expect("runtime"), b"new-runtime");
        assert!(root.path().join("provenance-v1").is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn shared_install_detection_resolves_symlink_aliases() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().expect("temp dir");
        let executable = root.path().join("installed-harn");
        let alias = root.path().join("hook-harn");
        fs::write(&executable, b"runtime").expect("runtime");
        symlink(&executable, &alias).expect("symlink");
        assert!(paths_refer_to_same_file(&alias, &executable));
    }

    #[test]
    fn find_expected_sha_parses_coreutils_format() {
        let manifest = "\
deadbeef00000000000000000000000000000000000000000000000000000000  harn-x86_64-apple-darwin.tar.gz
cafef00d00000000000000000000000000000000000000000000000000000000  harn-aarch64-apple-darwin.tar.gz
";
        assert_eq!(
            find_expected_sha(manifest, "harn-aarch64-apple-darwin.tar.gz").as_deref(),
            Some("cafef00d00000000000000000000000000000000000000000000000000000000"),
        );
    }

    #[test]
    fn find_expected_sha_tolerates_binary_mode_marker() {
        let manifest =
            "deadbeef00000000000000000000000000000000000000000000000000000000 *harn-x86_64-pc-windows-msvc.zip\n";
        assert_eq!(
            find_expected_sha(manifest, "harn-x86_64-pc-windows-msvc.zip").as_deref(),
            Some("deadbeef00000000000000000000000000000000000000000000000000000000"),
        );
    }

    #[test]
    fn find_expected_sha_misses_unknown_asset() {
        let manifest = "abc  some-other.tar.gz\n";
        assert!(find_expected_sha(manifest, "harn-x86_64-apple-darwin.tar.gz").is_none());
    }
}
