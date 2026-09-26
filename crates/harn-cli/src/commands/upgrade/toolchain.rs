//! Version-pinned release binaries. Download and checksum authority stays in
//! `upgrade`; this module owns only the per-version cache and CLI projection.

use std::cmp::Reverse;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde_json::Value;

use crate::cli::{SelfArgs, SelfCommand};

use super::{
    download, file_sha256_hex, find_expected_sha, harn_binary_name, install_verified_archive,
    normalize_version, target_triple, RELEASES_BASE,
};

struct CachedBinary {
    path: PathBuf,
    version: String,
    revision: String,
}

pub(crate) async fn run(args: SelfArgs) -> Result<i32, String> {
    tokio::task::spawn_blocking(move || run_blocking(args))
        .await
        .map_err(|error| format!("self toolchain task failed: {error}"))?
}

fn run_blocking(args: SelfArgs) -> Result<i32, String> {
    let root = toolchains_root()?;
    match args.command {
        SelfCommand::Install { version } => {
            let (binary, downloaded) = ensure_installed(&root, &version)?;
            println!(
                "{} {} ({}) at {}",
                if downloaded { "Installed" } else { "Cached" },
                binary.version,
                binary.revision,
                binary.path.display()
            );
            Ok(0)
        }
        SelfCommand::Run { version, args } => run_version(&root, &version, &args),
        SelfCommand::List => {
            for (version, path) in cached_versions(&root)? {
                println!("{version}\t{}", path.display());
            }
            Ok(0)
        }
        SelfCommand::Prune { keep } => {
            let versions = cached_versions(&root)?;
            for (version, path) in versions.into_iter().skip(keep) {
                let _lock = lock_version(&root, &version, harn_flock::LockMode::Exclusive)?;
                if valid_version_dir(&root, &version, &path)? {
                    fs::remove_dir_all(&path)
                        .map_err(|error| format!("failed to prune {}: {error}", path.display()))?;
                    println!("Pruned {version}");
                }
            }
            Ok(0)
        }
    }
}

fn toolchains_root() -> Result<PathBuf, String> {
    let home = harn_vm::user_dirs::home_dir().ok_or("could not locate home directory")?;
    let root = home.join(".harn/toolchains");
    fs::create_dir_all(&root).map_err(|error| format!("failed to create cache: {error}"))?;
    root.canonicalize()
        .map_err(|error| format!("failed to resolve cache: {error}"))
}

fn lock_version(
    root: &Path,
    version: &str,
    mode: harn_flock::LockMode,
) -> Result<fs::File, String> {
    let lock_dir = root.join(".locks");
    fs::create_dir_all(&lock_dir).map_err(|error| error.to_string())?;
    let path = lock_dir.join(format!("{version}.lock"));
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|error| format!("failed to open {}: {error}", path.display()))?;
    harn_flock::lock_with_deadline(&lock, &path, mode, Duration::from_mins(1))
        .map_err(|error| error.to_string())?;
    Ok(lock)
}

fn version_dir(root: &Path, version: &str) -> Result<(String, PathBuf), String> {
    let version = normalize_version(version)?;
    Ok((version.clone(), root.join(version)))
}

fn valid_version_dir(root: &Path, version: &str, path: &Path) -> Result<bool, String> {
    if path != root.join(version) {
        return Ok(false);
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(metadata.is_dir() && !metadata.file_type().is_symlink()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("failed to inspect {}: {error}", path.display())),
    }
}

fn cached_binary(root: &Path, version: &str) -> Result<Option<CachedBinary>, String> {
    let (_, dir) = version_dir(root, version)?;
    if !valid_version_dir(root, version, &dir)? {
        return Ok(None);
    }
    let manifest = match fs::read(dir.join("install-manifest.json")) {
        Ok(bytes) => serde_json::from_slice::<Value>(&bytes).ok(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(format!("failed to read toolchain receipt: {error}")),
    };
    let Some(manifest) = manifest else {
        return Ok(None);
    };
    let expected_version = version.trim_start_matches('v');
    if manifest["schema_version"] != "harn-install-v1" || manifest["version"] != expected_version {
        return Ok(None);
    }
    let Some(expected_sha) = manifest["binary_sha256"].as_str() else {
        return Ok(None);
    };
    if expected_sha.len() != 64 || !expected_sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Ok(None);
    }
    let path = dir.join(harn_binary_name());
    if manifest["binary_path"].as_str().map(Path::new) != Some(path.as_path()) {
        return Ok(None);
    }
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("failed to inspect {}: {error}", path.display())),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Ok(None);
    }
    if file_sha256_hex(&path)? != expected_sha {
        return Ok(None);
    }
    probe_binary(&path, version).map(Some)
}

fn probe_binary(path: &Path, version: &str) -> Result<CachedBinary, String> {
    let output = Command::new(path)
        .args(["version", "--json"])
        .output()
        .map_err(|error| format!("failed to run {}: {error}", path.display()))?;
    if !output.status.success() {
        return Err(format!("{} did not report a version", path.display()));
    }
    let value: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("invalid version receipt from {}: {error}", path.display()))?;
    let actual = value["data"]["version"]
        .as_str()
        .ok_or("version receipt has no version")?;
    if actual != version.trim_start_matches('v') {
        return Err(format!(
            "cached binary reports v{actual}, expected {version}"
        ));
    }
    let revision = value["data"]["source_revision"]
        .as_str()
        .ok_or("version receipt has no source revision")?;
    if revision.len() != 40 || !revision.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("version receipt has no full source revision".to_string());
    }
    Ok(CachedBinary {
        path: path.to_path_buf(),
        version: version.to_string(),
        revision: revision.to_string(),
    })
}

fn ensure_installed(root: &Path, requested: &str) -> Result<(CachedBinary, bool), String> {
    let (version, dir) = version_dir(root, requested)?;
    let _lock = lock_version(root, &version, harn_flock::LockMode::Exclusive)?;
    if let Some(binary) = cached_binary(root, &version)? {
        return Ok((binary, false));
    }
    if dir.exists() && !valid_version_dir(root, &version, &dir)? {
        return Err(format!(
            "toolchain path is not a directory: {}",
            dir.display()
        ));
    }
    let target = toolchain_target()?;
    let asset = if cfg!(windows) {
        format!("harn-{target}.zip")
    } else {
        format!("harn-{target}.tar.gz")
    };
    let staging = tempfile::tempdir().map_err(|error| error.to_string())?;
    let archive = staging.path().join(&asset);
    let sums = staging.path().join("SHA256SUMS");
    download(
        &format!("{RELEASES_BASE}/download/{version}/{asset}"),
        &archive,
    )?;
    download(
        &format!("{RELEASES_BASE}/download/{version}/SHA256SUMS"),
        &sums,
    )?;
    let sums = fs::read_to_string(&sums).map_err(|error| error.to_string())?;
    let checksum = find_expected_sha(&sums, &asset)
        .ok_or_else(|| format!("SHA256SUMS has no entry for {asset}"))?;
    install_verified_archive(&archive, &checksum, &dir, &version)?;
    let binary = cached_binary(root, &version)?
        .ok_or_else(|| format!("installed {version} but its receipt did not verify"))?;
    Ok((binary, true))
}

fn toolchain_target() -> Result<&'static str, String> {
    if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Ok("x86_64-pc-windows-msvc")
    } else {
        target_triple()
    }
}

fn run_version(root: &Path, requested: &str, args: &[OsString]) -> Result<i32, String> {
    let (version, _) = version_dir(root, requested)?;
    for _ in 0..2 {
        ensure_installed(root, &version)?;
        let _lock = lock_version(root, &version, harn_flock::LockMode::Shared)?;
        if let Some(binary) = cached_binary(root, &version)? {
            eprintln!("harn self run: {} {}", binary.version, binary.revision);
            let status = Command::new(&binary.path)
                .args(args)
                .status()
                .map_err(|error| format!("failed to run {}: {error}", binary.path.display()))?;
            return Ok(status.code().unwrap_or(1));
        }
        // A concurrent prune can win between installation and the shared run lock.
    }
    Err(format!(
        "cached {version} was repeatedly pruned before launch"
    ))
}

fn cached_versions(root: &Path) -> Result<Vec<(String, PathBuf)>, String> {
    let mut versions = Vec::new();
    for entry in fs::read_dir(root).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let name = entry.file_name();
        let Some(version) = name.to_str() else {
            continue;
        };
        if normalize_version(version).as_deref() != Ok(version) {
            continue;
        }
        let _lock = lock_version(root, version, harn_flock::LockMode::Shared)?;
        if cached_binary(root, version)?.is_some() {
            versions.push((version.to_string(), entry.path()));
        }
    }
    versions.sort_by_key(|entry| Reverse(version_parts(&entry.0)));
    Ok(versions)
}

fn version_parts(version: &str) -> (u64, u64, u64) {
    let mut parts = version.trim_start_matches('v').split('.');
    (
        parts.next().and_then(|part| part.parse().ok()).unwrap_or(0),
        parts.next().and_then(|part| part.parse().ok()).unwrap_or(0),
        parts.next().and_then(|part| part.parse().ok()).unwrap_or(0),
    )
}
