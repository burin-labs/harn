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

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::cli::UpgradeArgs;
use crate::json_envelope::{self, JsonEnvelope};
use crate::net;

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

    if args.no_verify {
        eprintln!("warning: SHA256 verification skipped (--no-verify)");
    } else {
        verify_checksum(&checksums_url, &archive_name, &archive_path)?;
    }

    println!("Extracting");
    extract_tarball(&archive_path, staging.path())?;

    let staged_binary = staging.path().join(harn_binary_name());
    if !staged_binary.exists() {
        return Err(format!(
            "archive did not contain {} — refusing to upgrade",
            harn_binary_name(),
        ));
    }

    install_binaries(staging.path(), &install_dir)?;

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

    if args.no_verify {
        eprintln!("warning: SHA256 verification skipped (--no-verify)");
    } else if let Err(error) = verify_checksum(&checksums_url, &archive_name, &archive_path) {
        return emit_upgrade_error_with(&report, "checksum_failed", error);
    }

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

    if let Err(error) = install_binaries(staging.path(), &install_dir) {
        return emit_upgrade_error_with(&report, "install_failed", error);
    }

    report.installed = true;
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

fn verify_checksum(checksums_url: &str, asset: &str, path: &Path) -> Result<(), String> {
    let client = http_client()?;
    let response = client
        .get(checksums_url)
        .send()
        .map_err(|error| format!("failed to fetch SHA256SUMS: {}", net::reqwest_error(&error)))?;
    if !response.status().is_success() {
        // Releases predating the SHA256SUMS workflow step won't have
        // this manifest. Match install.sh's behavior and warn-and-skip
        // rather than refusing — TLS to github.com is still in place.
        eprintln!(
            "warning: no SHA256SUMS published for this release (status {}); skipping verification",
            response.status()
        );
        return Ok(());
    }
    let manifest = response.text().map_err(|error| {
        format!(
            "failed to read SHA256SUMS body: {}",
            net::reqwest_error(&error)
        )
    })?;

    let expected = find_expected_sha(&manifest, asset)
        .ok_or_else(|| format!("SHA256SUMS does not include an entry for {asset}"))?;

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
    let actual = hex_lower(&hasher.finalize());

    if actual != expected {
        return Err(format!(
            "SHA256 mismatch for {asset}: expected {expected}, got {actual}"
        ));
    }
    println!("Verified SHA256 ({asset})");
    Ok(())
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
    if let Err(error) = fs::rename(&temp_in_dest, dest) {
        let _ = fs::remove_file(&temp_in_dest);
        return Err(format!("failed to replace {}: {error}", dest.display()));
    }
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
    Ok(())
}

fn next_upgrade_counter() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

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
