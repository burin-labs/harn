//! File-backed secret store.
//!
//! Stores per-account `{key: value}` maps as a flat JSON dictionary. The
//! root directory cascade is:
//!
//! 1. `$HARN_SECRET_STORE_FILE_ROOT` (test/eval hook).
//! 2. `%APPDATA%` on Windows.
//! 3. `$XDG_CONFIG_HOME` on every other OS.
//! 4. `~/.config` if `$HOME` is set.
//! 5. `./.harn-secret-store` as a last resort.
//!
//! `<account>/credentials.json` is appended underneath whichever root
//! resolves first. On Unix the file is chmod-ed to `0o600` and the
//! account subdirectory to `0o700`. On Windows the file sits inside
//! `%APPDATA%`, which is already user-scoped, so no explicit chmod is
//! issued.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::secret_store::Backend;

/// File-backed secret store. Stateless: every operation reloads from disk
/// so concurrent processes see each other's writes immediately.
pub(super) struct FileStore;

impl FileStore {
    pub(super) fn new() -> Self {
        FileStore
    }

    fn credentials_path(account: &str) -> PathBuf {
        config_dir().join(account).join("credentials.json")
    }

    fn read_map(account: &str) -> io::Result<BTreeMap<String, String>> {
        let path = Self::credentials_path(account);
        match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|err| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "credentials file at {} is not a JSON object: {err}",
                        path.display()
                    ),
                )
            }),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(err) => Err(err),
        }
    }

    fn write_map(account: &str, map: &BTreeMap<String, String>) -> io::Result<()> {
        let path = Self::credentials_path(account);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
            apply_dir_permissions(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(map)
            .map_err(|err| io::Error::other(format!("serialize credentials map: {err}")))?;
        atomic_write(&path, &bytes)?;
        apply_file_permissions(&path)?;
        Ok(())
    }
}

impl Backend for FileStore {
    fn name(&self) -> &'static str {
        "file"
    }

    fn get(&self, account: &str, key: &str) -> io::Result<Option<String>> {
        Ok(Self::read_map(account)?.get(key).cloned())
    }

    fn set(&self, account: &str, key: &str, value: &str) -> io::Result<()> {
        let mut map = Self::read_map(account)?;
        map.insert(key.to_string(), value.to_string());
        Self::write_map(account, &map)
    }

    fn delete(&self, account: &str, key: &str) -> io::Result<bool> {
        let mut map = Self::read_map(account)?;
        let removed = map.remove(key).is_some();
        if removed {
            Self::write_map(account, &map)?;
        }
        Ok(removed)
    }

    fn list(&self, account: &str) -> io::Result<Vec<String>> {
        Ok(Self::read_map(account)?.into_keys().collect())
    }
}

/// Root config directory. The `<account>` segment is appended by the
/// caller so each application's credentials land in its own subdirectory
/// (matches the host's existing `$XDG_CONFIG_HOME/burin/credentials.json`).
fn config_dir() -> PathBuf {
    if let Some(override_root) = env_nonempty("HARN_SECRET_STORE_FILE_ROOT") {
        return PathBuf::from(override_root);
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(appdata) = env_nonempty("APPDATA") {
            return PathBuf::from(appdata);
        }
    }
    if let Some(xdg) = env_nonempty("XDG_CONFIG_HOME") {
        return PathBuf::from(xdg);
    }
    if let Some(home) = home_dir() {
        return home.join(".config");
    }
    // Last-resort: current directory. Rare in practice (no $HOME, no
    // XDG_CONFIG_HOME, no APPDATA — typically only in broken containers).
    PathBuf::from(".harn-secret-store")
}

fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

fn home_dir() -> Option<PathBuf> {
    harn_vm::user_dirs::home_dir()
}

fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .map(|n| n.to_owned())
        .unwrap_or_else(|| std::ffi::OsString::from("credentials.json"));
    let mut tmp = dir.to_path_buf();
    tmp.push(format!(".{}.tmp", file_name.to_string_lossy()));
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(unix)]
fn apply_dir_permissions(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn apply_dir_permissions(_dir: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn apply_file_permissions(file: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(file, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn apply_file_permissions(_file: &Path) -> io::Result<()> {
    Ok(())
}
