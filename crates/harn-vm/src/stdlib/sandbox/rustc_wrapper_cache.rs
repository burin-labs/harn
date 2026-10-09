//! Filesystem inputs to a wrapper measurement. Cargo still resolves the build;
//! these snapshots only decide whether an earlier measurement can be reused.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(PartialEq, Eq)]
struct Stamp {
    resolved: Option<PathBuf>,
    modified: Option<SystemTime>,
    len: Option<u64>,
    config_digest: Option<[u8; 32]>,
    #[cfg(unix)]
    identity: Option<(u64, u64, i64, i64)>,
}

#[derive(PartialEq, Eq)]
pub(super) struct Inputs(Vec<(PathBuf, Stamp)>);

pub(super) fn capture(cwd: &Path, env: &BTreeMap<String, String>) -> Option<Inputs> {
    let value = |key: &str| {
        env.iter()
            .find(|(name, _)| {
                crate::security::environment_policy::environment_names_equal(name, key)
            })
            .map(|(_, value)| value)
    };
    let mut directories: Vec<_> = cwd.ancestors().map(|dir| dir.join(".cargo")).collect();
    directories.extend(
        value("CARGO_HOME")
            .map(PathBuf::from)
            .or_else(|| value("HOME").map(|home| Path::new(home).join(".cargo")))
            .map(|directory| {
                if directory.is_absolute() {
                    directory
                } else {
                    cwd.join(directory)
                }
            }),
    );
    let mut paths = BTreeSet::new();
    let mut configs = BTreeMap::new();
    let mut wrappers = Vec::new();
    for key in super::RUSTC_WRAPPER_ENV_KEYS {
        if let Some(wrapper) = value(key).filter(|wrapper| !wrapper.is_empty()) {
            wrappers.push((cwd.to_path_buf(), wrapper.clone()));
        }
    }
    for directory in directories {
        for file in [directory.join("config"), directory.join("config.toml")] {
            paths.insert(file.clone());
            let text = match std::fs::read_to_string(&file) {
                Ok(text) => text,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => return None,
            };
            configs.insert(file.clone(), *blake3::hash(text.as_bytes()).as_bytes());
            let config: toml::Value = toml::from_str(&text).ok()?;
            // Includes can introduce arbitrary additional files. Until Cargo
            // reports that dependency graph, remeasure instead of guessing it.
            if config.get("include").is_some() {
                return None;
            }
            if let Some(build) = config.get("build") {
                for key in ["rustc-wrapper", "rustc-workspace-wrapper"] {
                    if let Some(wrapper) = build.get(key) {
                        let wrapper = wrapper.as_str()?;
                        if !wrapper.is_empty() {
                            wrappers.push((cwd.to_path_buf(), wrapper.to_string()));
                            wrappers.push((directory.clone(), wrapper.to_string()));
                            if let Some(parent) = directory.parent() {
                                wrappers.push((parent.to_path_buf(), wrapper.to_string()));
                            }
                        }
                    }
                }
            }
        }
    }
    for (base, wrapper) in wrappers {
        let wrapper = Path::new(&wrapper);
        if wrapper.is_absolute() {
            paths.insert(wrapper.to_path_buf());
        } else if wrapper.components().count() > 1 {
            paths.insert(base.join(wrapper));
        } else if let Some(path) = value("PATH") {
            for directory in std::env::split_paths(path) {
                let directory = if directory.is_absolute() {
                    directory
                } else {
                    cwd.join(directory)
                };
                paths.insert(directory.join(wrapper));
                #[cfg(windows)]
                for extension in value("PATHEXT")
                    .map(String::as_str)
                    .unwrap_or(".COM;.EXE;.BAT;.CMD")
                    .split(';')
                {
                    paths.insert(directory.join(format!("{}{extension}", wrapper.display())));
                }
            }
        } else {
            return None;
        }
    }
    Some(Inputs(
        paths
            .into_iter()
            .map(|path| {
                let metadata = std::fs::metadata(&path).ok();
                let stamp = Stamp {
                    resolved: std::fs::canonicalize(&path).ok(),
                    modified: metadata
                        .as_ref()
                        .and_then(|metadata| metadata.modified().ok()),
                    len: metadata.as_ref().map(|metadata| metadata.len()),
                    config_digest: configs.remove(&path),
                    #[cfg(unix)]
                    identity: metadata.map(|metadata| {
                        use std::os::unix::fs::MetadataExt;
                        (
                            metadata.dev(),
                            metadata.ino(),
                            metadata.ctime(),
                            metadata.ctime_nsec(),
                        )
                    }),
                };
                (path, stamp)
            })
            .collect(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cargo_config_includes_refuse_unverifiable_wrapper_cache_reuse() {
        let temp = tempfile::tempdir().unwrap();
        let env = BTreeMap::from([("CARGO_HOME".into(), temp.path().display().to_string())]);
        assert!(capture(temp.path(), &env).is_some());
        std::fs::write(
            temp.path().join("config.toml"),
            "include = [\"other.toml\"]\n",
        )
        .unwrap();
        assert!(capture(temp.path(), &env).is_none());
    }
}
