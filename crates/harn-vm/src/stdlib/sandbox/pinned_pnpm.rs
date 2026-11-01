//! Put the project's pinned pnpm first on a confined child's search path.
//!
//! A pnpm that differs from the `packageManager` pin in `package.json` asks the
//! registry for the pinned version's integrity before switching to it, even
//! when that version is already installed under `$PNPM_HOME/.tools`. A
//! confined child has no network, so every `pnpm` command fails or hangs. The
//! installed pinned version runs offline, so the child should find it first.

use std::path::{Path, PathBuf};

use crate::orchestration::CapabilityPolicy;

pub(super) fn inject_env(env: &mut Vec<(String, String)>, policy: &CapabilityPolicy) {
    if !policy.sandbox_profile.confines_processes()
        || super::super::effective_fallback(policy.sandbox_profile)
            == super::super::SandboxFallback::Off
        || !super::super::active_backend_available()
    {
        return;
    }
    let Some(root) = super::super::normalized_workspace_roots(policy)
        .into_iter()
        .next()
    else {
        return;
    };
    let inherited = |name: &str| {
        env.iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
            .or_else(|| crate::stdlib::process::session_env_var(name).ok().flatten())
    };
    let (Some(path), Some(pnpm_home)) = (inherited("PATH"), inherited("PNPM_HOME")) else {
        return;
    };
    if let Some(path) = pinned_path(&root, &path, Path::new(&pnpm_home)) {
        env.retain(|(key, _)| key != "PATH");
        env.push(("PATH".to_string(), path));
    }
}

/// The search path with the pinned pnpm's bin directory first, or `None` when
/// the project pins no pnpm version or that version is not installed.
fn pinned_path(root: &Path, inherited: &str, pnpm_home: &Path) -> Option<String> {
    if inherited.is_empty() || !pnpm_home.is_absolute() {
        return None;
    }
    let bin = pinned_bin_dir(root, pnpm_home)?;
    let dirs = std::iter::once(bin.clone())
        .chain(std::env::split_paths(inherited).filter(|dir| dir != &bin));
    std::env::join_paths(dirs)
        .ok()
        .map(|path| path.to_string_lossy().into_owned())
}

fn pinned_bin_dir(root: &Path, pnpm_home: &Path) -> Option<PathBuf> {
    let manifest = std::fs::read_to_string(root.join("package.json")).ok()?;
    let manifest: serde_json::Value = serde_json::from_str(&manifest).ok()?;
    let pin = manifest
        .get("packageManager")?
        .as_str()?
        .strip_prefix("pnpm@")?;
    let version = pin.split('+').next()?;
    let well_formed = !version.is_empty()
        && !version.starts_with('.')
        && version
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
    if !well_formed {
        return None;
    }
    let bin = pnpm_home.join(".tools/pnpm").join(version).join("bin");
    bin.join("pnpm").is_file().then_some(bin)
}
