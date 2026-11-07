//! Installed budget-owner identity for a credential-confined native GitHub read.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::stdlib::sandbox::workspace_env::grant_spawn;

const OWNER_FILE: &str = ".harn-github-budget-owner.json";

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Executable {
    path: PathBuf,
    sha256: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InstalledOwner {
    schema: String,
    wrapper: Executable,
    implementation: Executable,
    adapter: Executable,
    interpreter: Executable,
    native_gh: Executable,
    state_directory: PathBuf,
    account_config: PathBuf,
    #[serde(rename = "policy_environment")]
    _policy_environment: std::collections::BTreeMap<String, String>,
}

/// Validated files belonging to the installed, token-free budget adapter.
/// Harn requests cannot select an adapter or change these paths.
pub struct GitHubBudgetOwner {
    pub interpreter: PathBuf,
    pub adapter: PathBuf,
    pub implementation: PathBuf,
    pub native_gh: PathBuf,
    pub state_directory: PathBuf,
    pub account_config_digest: String,
    pub credential_digest: String,
    manifest: Executable,
    installed_files: Vec<Executable>,
    account_config: PathBuf,
}

fn canonical(path: &Path, untrusted: &[PathBuf]) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err("budget-owner paths must be absolute".into());
    }
    let canonical = std::fs::canonicalize(path)
        .map_err(|_| "a budget-owner file is unavailable".to_string())?;
    if untrusted.iter().any(|root| canonical.starts_with(root)) {
        return Err("a budget-owner file is inside a writable or workspace root".into());
    }
    Ok(canonical)
}

fn executable(file: &Executable, untrusted: &[PathBuf]) -> Result<PathBuf, String> {
    let path = canonical(&file.path, untrusted)?;
    if path != file.path || !path.is_file() {
        return Err("a budget-owner executable identity changed".into());
    }
    let bytes =
        std::fs::read(&path).map_err(|_| "a budget-owner executable cannot be read".to_string())?;
    if file.sha256.len() != 64 || format!("{:x}", Sha256::digest(&bytes)) != file.sha256 {
        return Err("a budget-owner executable digest changed; reinstall its owner".into());
    }
    Ok(path)
}

fn account_fingerprint(path: &Path, untrusted: &[PathBuf]) -> Result<String, String> {
    match std::fs::read(path) {
        Ok(bytes) => {
            if canonical(path, untrusted)? != path {
                return Err("the GitHub account configuration identity changed".into());
            }
            Ok(format!("{:x}", Sha256::digest(&bytes)))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut parent = path
                .parent()
                .ok_or("the GitHub account configuration has no parent")?;
            while !parent.exists() {
                parent = parent
                    .parent()
                    .ok_or("the GitHub account configuration has no existing ancestor")?;
            }
            if canonical(parent, untrusted)? != parent {
                return Err("the GitHub account configuration ancestor changed".into());
            }
            Ok("missing".to_string())
        }
        Err(_) => Err("the GitHub account configuration cannot be identified".into()),
    }
}

impl GitHubBudgetOwner {
    pub(crate) fn same_enrollment(&self, other: &Self) -> bool {
        self.manifest.path == other.manifest.path && self.manifest.sha256 == other.manifest.sha256
    }

    /// The enrolled manifest the token-free adapter reads for its state root.
    pub fn manifest_path(&self) -> &Path {
        &self.manifest.path
    }

    pub(crate) fn discover(wrapper: &Path, untrusted: &[PathBuf]) -> Result<Option<Self>, String> {
        let directory = wrapper.parent().ok_or("the GitHub program has no parent")?;
        let manifest = directory.join(OWNER_FILE);
        let bytes = match std::fs::read(&manifest) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err("the installed GitHub budget owner cannot be read".into()),
        };
        if bytes.len() > 16 * 1024 {
            return Err("the installed GitHub budget owner is oversized".into());
        }
        let manifest = Executable {
            path: canonical(&manifest, untrusted)?,
            sha256: format!("{:x}", Sha256::digest(&bytes)),
        };
        let owner: InstalledOwner = serde_json::from_slice(&bytes)
            .map_err(|_| "the installed GitHub budget owner is invalid".to_string())?;
        if owner.schema != "github.budget-owner.v1"
            || executable(&owner.wrapper, untrusted)? != wrapper
        {
            return Err("the GitHub budget owner does not bind the selected program".into());
        }
        let implementation = executable(&owner.implementation, untrusted)?;
        let adapter = executable(&owner.adapter, untrusted)?;
        let interpreter = executable(&owner.interpreter, untrusted)?;
        let native_gh = executable(&owner.native_gh, untrusted)?;
        if implementation.parent() != Some(directory)
            || adapter.parent() != Some(directory)
            || !grant_spawn::is_native_executable(&interpreter)
            || !grant_spawn::is_native_executable(&native_gh)
            || !matches!(
                native_gh.file_name().and_then(|name| name.to_str()),
                Some("gh" | "gh.exe")
            )
            || native_gh == wrapper
        {
            return Err("the GitHub budget owner has invalid executable boundaries".into());
        }
        let state_directory = canonical(&owner.state_directory, untrusted)?;
        let home =
            crate::user_dirs::home_dir().ok_or("the GitHub budget owner requires a user home")?;
        let protected = grant_spawn::trusted_config_paths(&home)
            .into_iter()
            .chain(std::iter::once(grant_spawn::grant_spawns_root()));
        if state_directory != owner.state_directory
            || !state_directory.is_dir()
            || state_directory.parent().is_none()
            || [wrapper, &implementation, &adapter, &interpreter, &native_gh]
                .iter()
                .any(|path| path.starts_with(&state_directory))
            || protected.into_iter().any(|path| {
                state_directory.starts_with(&path) || path.starts_with(&state_directory)
            })
        {
            return Err("the GitHub budget owner requires a dedicated state directory".into());
        }
        let account_config_digest = account_fingerprint(&owner.account_config, untrusted)?;
        Ok(Some(Self {
            interpreter,
            adapter,
            implementation,
            native_gh,
            state_directory,
            account_config_digest,
            credential_digest: String::new(),
            manifest,
            installed_files: vec![
                owner.wrapper,
                owner.implementation,
                owner.adapter,
                owner.interpreter,
                owner.native_gh,
            ],
            account_config: owner.account_config,
        }))
    }

    /// Refuse a plan whose installed code, account source or writable-root
    /// relationship changed while the command awaited assessment.
    pub fn revalidate(&self, cwd: &Path) -> Result<(), String> {
        let untrusted = grant_spawn::untrusted_roots(cwd);
        executable(&self.manifest, &untrusted)?;
        for file in &self.installed_files {
            executable(file, &untrusted)?;
        }
        if canonical(&self.state_directory, &untrusted)? != self.state_directory
            || !self.state_directory.is_dir()
            || account_fingerprint(&self.account_config, &untrusted)? != self.account_config_digest
        {
            return Err("the GitHub budget plan changed; prepare the command again".into());
        }
        Ok(())
    }

    /// Construct the literal native invocation before command assessment.
    pub fn native_argv(&self, args: &[String], etag: Option<&str>) -> Result<Vec<String>, String> {
        if args.len() != 2 || args[0] != "api" {
            return Err("budgeted GitHub commands require exactly `gh api <path>`".into());
        }
        let mut argv = vec![self.native_gh.display().to_string()];
        argv.extend_from_slice(args);
        argv.push("--include".into());
        if let Some(etag) = etag {
            if etag.is_empty()
                || etag.len() > 8192
                || !etag.bytes().all(|byte| (32..=126).contains(&byte))
            {
                return Err("the prepared GitHub ETag is unusable".into());
            }
            argv.extend(["--header".into(), format!("If-None-Match: {etag}")]);
        }
        Ok(argv)
    }
}
