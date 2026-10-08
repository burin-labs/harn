//! Process-local Git discovery cache. Grants never come from a child-writable
//! on-disk cache; Git remains the authority for includes and conditionals.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

type Key = (PathBuf, Option<PathBuf>, PathBuf, [u8; 32]);

fn environment_digest(env: &BTreeMap<OsString, OsString>) -> [u8; 32] {
    let mut hash = blake3::Hasher::new();
    for (key, value) in env {
        for bytes in [key.as_encoded_bytes(), value.as_encoded_bytes()] {
            hash.update(&(bytes.len() as u64).to_le_bytes());
            hash.update(bytes);
        }
    }
    *hash.finalize().as_bytes()
}

#[derive(PartialEq, Eq)]
struct Stamp {
    resolved: Option<PathBuf>,
    modified: Option<SystemTime>,
    len: Option<u64>,
}

fn stamp(path: &Path) -> Stamp {
    let metadata = std::fs::metadata(path).ok();
    Stamp {
        resolved: std::fs::canonicalize(path).ok(),
        modified: metadata.as_ref().and_then(|meta| meta.modified().ok()),
        len: metadata.map(|meta| meta.len()),
    }
}

struct Entry {
    roots: Vec<PathBuf>,
    dependencies: Vec<(PathBuf, Stamp)>,
}

#[derive(Default)]
pub(super) struct Cache(BTreeMap<Key, Entry>);

pub(super) fn cache() -> &'static Mutex<Cache> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Cache::default()))
}

impl Cache {
    pub(super) fn roots(
        &mut self,
        workspace: &Path,
        home: Option<&Path>,
        git: &Path,
        env: &BTreeMap<OsString, OsString>,
        query: impl FnOnce() -> (Vec<Vec<u8>>, bool),
    ) -> Vec<PathBuf> {
        let key = (
            workspace.to_path_buf(),
            home.map(Path::to_path_buf),
            git.to_path_buf(),
            environment_digest(env),
        );
        if let Some(entry) = self.0.get(&key) {
            if entry
                .dependencies
                .iter()
                .all(|(path, old)| stamp(path) == *old)
            {
                return entry.roots.clone();
            }
        }
        let mut dependencies = config_inputs(workspace, home, git, env);
        // Do not cache partial or failed queries as a successful empty result.
        let (listings, complete) = query();
        let mut roots = Vec::new();
        for listing in listings {
            roots.extend(super::roots_from_config_listing(&listing, workspace, home));
            dependencies.extend(listing_inputs(&listing, workspace, home));
        }
        if !complete {
            self.0.remove(&key);
            return roots;
        }
        dependencies.sort();
        dependencies.dedup();
        // Bound the number of distinct workspace/environment entries.
        if self.0.len() >= 128 {
            self.0.clear();
        }
        self.0.insert(
            key,
            Entry {
                roots: roots.clone(),
                dependencies: dependencies
                    .into_iter()
                    .map(|path| {
                        let state = stamp(&path);
                        (path, state)
                    })
                    .collect(),
            },
        );
        roots
    }
}

fn config_inputs(
    workspace: &Path,
    home: Option<&Path>,
    git: &Path,
    env: &BTreeMap<OsString, OsString>,
) -> Vec<PathBuf> {
    let var = |key: &str| env.get(std::ffi::OsStr::new(key)).map(PathBuf::from);
    let home = var("HOME").or_else(|| home.map(Path::to_path_buf));
    let mut paths = vec![git.to_path_buf(), PathBuf::from("/etc/gitconfig")];
    for key in ["GIT_CONFIG_GLOBAL", "GIT_CONFIG_SYSTEM"] {
        paths.extend(var(key).map(|path| workspace.join(path)));
    }
    if let Some(home) = &home {
        paths.push(home.join(".gitconfig"));
    }
    let xdg = var("XDG_CONFIG_HOME").or_else(|| home.map(|home| home.join(".config")));
    paths.extend(xdg.map(|xdg| xdg.join("git/config")));
    if let Some(prefix) = git.parent().and_then(Path::parent) {
        paths.push(prefix.join("etc/gitconfig"));
    }
    // Conditional includes can depend on the repository's branch or remotes.
    // Track worktree indirection and common config as well as ordinary .git.
    let dotgit = workspace.join(".git");
    paths.push(dotgit.clone());
    let gitdir = var("GIT_DIR")
        .map(|path| workspace.join(path))
        .unwrap_or_else(|| {
            std::fs::read_to_string(&dotgit)
                .ok()
                .and_then(|text| {
                    text.strip_prefix("gitdir: ")
                        .map(|path| workspace.join(path.trim()))
                })
                .unwrap_or(dotgit)
        });
    paths.extend([
        gitdir.join("HEAD"),
        gitdir.join("config"),
        gitdir.join("commondir"),
    ]);
    if let Ok(common) = std::fs::read_to_string(gitdir.join("commondir")) {
        paths.push(gitdir.join(common.trim()).join("config"));
    }
    paths
}

fn listing_inputs(listing: &[u8], workspace: &Path, home: Option<&Path>) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let mut fields = listing.split(|byte| *byte == 0);
    while let (Some(origin), Some(entry)) = (fields.next(), fields.next()) {
        let (Ok(origin), Ok(entry)) = (str::from_utf8(origin), str::from_utf8(entry)) else {
            continue;
        };
        let origin = origin
            .strip_prefix("file:")
            .and_then(|path| config_path(path, workspace, home));
        paths.extend(origin.clone());
        if let Some((key, value)) = entry.split_once('\n') {
            if key == "include.path" || (key.starts_with("includeif.") && key.ends_with(".path")) {
                let base = origin
                    .as_deref()
                    .and_then(Path::parent)
                    .unwrap_or(workspace);
                paths.extend(config_path(value, base, home));
            } else if matches!(
                key,
                "core.hookspath" | "core.excludesfile" | "core.attributesfile"
            ) {
                paths.extend(config_path(value, workspace, home));
            } else if key.starts_with("credential.") && key.ends_with(".helper") {
                if let Some(executable) = shell_words::split(value.trim_start_matches('!'))
                    .ok()
                    .and_then(|words| words.into_iter().next())
                    .filter(|word| Path::new(word).is_absolute() || word.starts_with("~/"))
                {
                    paths.extend(config_path(&executable, workspace, home));
                }
            }
        }
    }
    paths
}

fn config_path(value: &str, base: &Path, home: Option<&Path>) -> Option<PathBuf> {
    if value == "~" {
        home.map(Path::to_path_buf)
    } else if let Some(rest) = value.strip_prefix("~/") {
        home.map(|home| home.join(rest))
    } else if value.is_empty() {
        None
    } else {
        Some(base.join(value))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn git_queries_are_reused_and_includes_environment_and_branch_invalidate() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let home = temp.path().join("home");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        let config = home.join(".gitconfig");
        let included = home.join("included");
        let git = super::super::trusted_git_executable(&[]).expect("installed Git");
        let mut env = BTreeMap::from([
            ("HOME".into(), home.clone().into_os_string()),
            ("GIT_CONFIG_GLOBAL".into(), config.clone().into_os_string()),
            (
                "GIT_CONFIG_SYSTEM".into(),
                home.join("system").into_os_string(),
            ),
        ]);
        let mut init = crate::process_sandbox::session_std_command(&git).unwrap();
        assert!(init
            .env_clear()
            .envs(&env)
            .args(["init", "-q"])
            .arg(&workspace)
            .status()
            .unwrap()
            .success());
        std::fs::write(
            &config,
            "[include]\npath = included\n[includeIf \"onbranch:topic\"]\npath = topic\n",
        )
        .unwrap();
        std::fs::write(
            home.join("topic"),
            "[core]\nexcludesFile = ~/topic-ignore\n",
        )
        .unwrap();

        let calls = Cell::new(0);
        let mut cache = Cache::default();
        let query = |cache: &mut Cache, env: &BTreeMap<OsString, OsString>| {
            cache.roots(&workspace, Some(&home), &git, env, || {
                let mut listings = Vec::new();
                for scope in ["--global", "--system"] {
                    calls.set(calls.get() + 1);
                    let mut command = crate::process_sandbox::session_std_command(&git).unwrap();
                    let output = command
                        .env_clear()
                        .envs(env)
                        .arg("-C")
                        .arg(&workspace)
                        .args([
                            "config",
                            scope,
                            "--includes",
                            "--show-origin",
                            "--null",
                            "--get-regexp",
                            ".*",
                        ])
                        .output()
                        .unwrap();
                    assert!(
                        output.status.success() || output.status.code() == Some(1),
                        "{}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                    listings.push(output.stdout);
                }
                (listings, true)
            })
        };
        let first = query(&mut cache, &env);
        assert!(
            first.contains(&super::super::normalize_for_policy(&config)),
            "the real Git query must discover a config root"
        );
        assert_eq!(query(&mut cache, &env), first);
        assert_eq!(
            calls.get(),
            2,
            "an unchanged workspace must query each scope only once"
        );

        std::fs::write(&included, "[core]\nexcludesFile = ~/ignore\n").unwrap();
        assert!(query(&mut cache, &env)
            .contains(&super::super::normalize_for_policy(&home.join("ignore"))));
        assert_eq!(
            calls.get(),
            4,
            "creating a formerly absent include must invalidate"
        );
        std::fs::write(&included, "[core]\nexcludesFile = ~/replacement-ignore\n").unwrap();
        let updated = query(&mut cache, &env);
        assert!(updated.contains(&super::super::normalize_for_policy(
            &home.join("replacement-ignore")
        )));
        assert!(!updated.contains(&super::super::normalize_for_policy(&home.join("ignore"))));
        assert_eq!(calls.get(), 6);

        std::fs::write(workspace.join(".git/HEAD"), "ref: refs/heads/topic\n").unwrap();
        assert!(
            query(&mut cache, &env).contains(&super::super::normalize_for_policy(
                &home.join("topic-ignore")
            ))
        );
        assert_eq!(
            calls.get(),
            8,
            "branch-dependent includes must be re-evaluated"
        );
        env.insert(
            "GIT_CONFIG_GLOBAL".into(),
            home.join("other-config").into_os_string(),
        );
        assert!(query(&mut cache, &env).is_empty());
        assert!(query(&mut cache, &env).is_empty());
        assert_eq!(
            calls.get(),
            10,
            "a measured empty result must be cached too"
        );
    }

    #[test]
    fn failed_queries_are_never_cached_as_empty_success() {
        let mut cache = Cache::default();
        let env = BTreeMap::new();
        let calls = Cell::new(0);
        for _ in 0..2 {
            let roots = cache.roots(
                Path::new("/workspace"),
                None,
                Path::new("/git"),
                &env,
                || {
                    calls.set(calls.get() + 1);
                    (Vec::new(), false)
                },
            );
            assert!(roots.is_empty());
        }
        assert_eq!(calls.get(), 2);
    }
}
