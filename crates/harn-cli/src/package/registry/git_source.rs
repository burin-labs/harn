//! Git-backed package sources: recognizing and normalizing the spec shapes a
//! user can type, and invoking `git` itself through a hardened environment
//! that refuses to inherit ambient credentials or config.

use crate::package::*;

const PRESERVED_GIT_ENV: &[&str] = &[
    "PATH",
    "TMPDIR",
    "TEMP",
    "TMP",
    "SystemRoot",
    "WINDIR",
    "COMSPEC",
    "PATHEXT",
];
const CANONICAL_CHECKOUT_CONFIG: &[&str] = &[
    "-c",
    "core.autocrlf=false",
    "-c",
    "core.eol=lf",
    "-c",
    "core.symlinks=false",
];

pub(crate) fn manifest_has_git_dependencies(manifest: &Manifest) -> bool {
    manifest.dependencies.values().any(Dependency::requires_git)
}

pub(crate) fn ensure_git_available() -> Result<(), PackageError> {
    process::Command::new("git")
        .arg("--version")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .map(|_| ())
        .map_err(|_| {
            PackageError::Registry(
                "git is required for git dependencies but was not found in PATH".to_string(),
            )
        })
}

pub(crate) fn is_probable_shorthand_git_url(raw: &str) -> bool {
    !raw.contains("://")
        && !raw.starts_with("git@")
        && raw.contains('/')
        && raw
            .split('/')
            .next()
            .is_some_and(|segment| segment.contains('.'))
}

pub(crate) fn normalize_git_url(raw: &str) -> Result<String, PackageError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("git URL cannot be empty".to_string().into());
    }

    let candidate_path = PathBuf::from(trimmed);
    if candidate_path.exists() {
        let canonical = candidate_path
            .canonicalize()
            .map_err(|error| format!("failed to canonicalize {trimmed}: {error}"))?;
        let url = Url::from_file_path(canonical)
            .map_err(|_| format!("failed to convert {trimmed} to file:// URL"))?;
        return Ok(url.to_string().trim_end_matches('/').to_string());
    }

    if let Some(rest) = trimmed.strip_prefix("git@") {
        if let Some((host, path)) = rest.split_once(':') {
            return Ok(format!(
                "ssh://git@{}/{}",
                host,
                path.trim_start_matches('/').trim_end_matches('/')
            ));
        }
    }

    let with_scheme = if is_probable_shorthand_git_url(trimmed) {
        format!("https://{trimmed}")
    } else {
        trimmed.to_string()
    };
    let parsed =
        Url::parse(&with_scheme).map_err(|error| format!("invalid git URL {trimmed}: {error}"))?;
    let mut normalized = parsed.to_string();
    while normalized.ends_with('/') {
        normalized.pop();
    }
    if parsed.scheme() != "file" && normalized.ends_with(".git") {
        normalized.truncate(normalized.len() - 4);
    }
    Ok(normalized)
}

pub(crate) fn derive_repo_name_from_source(source: &str) -> Result<String, PackageError> {
    let url = Url::parse(source).map_err(|error| format!("invalid git URL {source}: {error}"))?;
    let segment = url
        .path_segments()
        .and_then(|mut segments| segments.rfind(|segment| !segment.is_empty()))
        .ok_or_else(|| format!("failed to derive package name from {source}"))?;
    Ok(segment.trim_end_matches(".git").to_string())
}

pub(crate) fn parse_positional_git_spec(spec: &str) -> (&str, Option<&str>) {
    if let Some((source, candidate_ref)) = spec.rsplit_once('@') {
        if !candidate_ref.is_empty()
            && !candidate_ref.contains('/')
            && !candidate_ref.contains(':')
            && !source.ends_with("://")
        {
            return (source, Some(candidate_ref));
        }
    }
    (spec, None)
}

pub(crate) fn existing_local_path_spec(spec: &str) -> Option<PathBuf> {
    if spec.trim().is_empty() || spec.contains("://") || spec.starts_with("git@") {
        return None;
    }
    let candidate = PathBuf::from(spec);
    if candidate.exists() {
        return Some(candidate);
    }
    if candidate.extension().is_none() {
        let with_ext = candidate.with_extension("harn");
        if with_ext.exists() {
            return Some(with_ext);
        }
    }
    if is_probable_shorthand_git_url(spec) {
        return None;
    }
    None
}

pub(crate) fn package_manifest_name(path: &Path) -> Option<String> {
    let manifest_path = if path.is_dir() {
        path.join(MANIFEST)
    } else {
        path.parent()?.join(MANIFEST)
    };
    let manifest = read_manifest_from_path(&manifest_path).ok()?;
    manifest
        .package
        .and_then(|pkg| pkg.name)
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
}

pub(crate) fn derive_package_alias_from_path(path: &Path) -> Result<String, PackageError> {
    if let Some(name) = package_manifest_name(path) {
        return Ok(name);
    }
    let fallback = if path.is_dir() {
        path.file_name()
    } else {
        path.file_stem()
    };
    fallback
        .and_then(|name| name.to_str())
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            PackageError::Registry(format!(
                "failed to derive package alias from {}",
                path.display()
            ))
        })
}

pub(crate) fn is_full_git_sha(value: &str) -> bool {
    value.len() == 40 && value.as_bytes().iter().all(|byte| byte.is_ascii_hexdigit())
}

pub(super) struct HardenedGitEnv {
    pub(super) _temp_dir: tempfile::TempDir,
    pub(super) home: PathBuf,
    pub(super) config_home: PathBuf,
    pub(super) global_config: PathBuf,
    pub(super) system_config: PathBuf,
}

impl HardenedGitEnv {
    pub(super) fn new() -> Result<Self, PackageError> {
        let temp_dir = tempfile::Builder::new()
            .prefix("harn-git-env-")
            .tempdir()
            .map_err(|error| {
                PackageError::Registry(format!("failed to create isolated git env: {error}"))
            })?;
        let home = temp_dir.path().join("home");
        let config_home = temp_dir.path().join("xdg-config");
        fs::create_dir_all(&home)
            .map_err(|error| format!("failed to create {}: {error}", home.display()))?;
        fs::create_dir_all(&config_home)
            .map_err(|error| format!("failed to create {}: {error}", config_home.display()))?;
        let global_config = home.join(".gitconfig");
        let system_config = temp_dir.path().join("gitconfig-system");
        Ok(Self {
            _temp_dir: temp_dir,
            home,
            config_home,
            global_config,
            system_config,
        })
    }

    pub(super) fn apply_to(&self, command: &mut process::Command, cwd: Cwd<'_>) {
        // Always set an explicit working directory: `Detached` resolves to the
        // env's own `HOME` tempdir, so a remote-only git call never inherits —
        // and never dies on — a deleted process CWD.
        command.current_dir(cwd.resolve(self.home.as_path()));
        // Registry git URLs are untrusted input, so fetches must not inherit
        // user Git config, credential helpers, SSH agents, or askpass hooks.
        command.env_clear();
        for name in PRESERVED_GIT_ENV {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        command
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", &self.config_home)
            .env("GIT_CONFIG_GLOBAL", &self.global_config)
            .env("GIT_CONFIG_SYSTEM", &self.system_config)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0");
    }
}

pub(crate) fn git_output<I, S>(args: I, cwd: Cwd<'_>) -> Result<std::process::Output, PackageError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let git_env = HardenedGitEnv::new()?;
    let mut command = process::Command::new("git");
    git_env.apply_to(&mut command, cwd);
    command.args(args);
    command
        .output()
        .map_err(|error| PackageError::Registry(format!("failed to run git: {error}")))
}

pub(crate) fn resolve_git_commit(
    url: &str,
    rev: Option<&str>,
    tag: Option<&str>,
    branch: Option<&str>,
) -> Result<String, PackageError> {
    let requested = branch.or(rev).or(tag).unwrap_or("HEAD");
    if branch.is_none() && tag.is_none() && is_full_git_sha(requested) {
        return Ok(requested.to_string());
    }

    let refs = if let Some(branch) = branch {
        vec![format!("refs/heads/{branch}")]
    } else if let Some(tag) = tag {
        vec![format!("refs/tags/{tag}^{{}}"), format!("refs/tags/{tag}")]
    } else if requested == "HEAD" {
        vec!["HEAD".to_string()]
    } else {
        vec![
            requested.to_string(),
            format!("refs/tags/{requested}^{{}}"),
            format!("refs/tags/{requested}"),
            format!("refs/heads/{requested}"),
        ]
    };

    let output = git_output(
        std::iter::once("ls-remote".to_string())
            .chain(std::iter::once(url.to_string()))
            .chain(refs),
        // Remote query — no working tree involved.
        Cwd::Detached,
    )?;
    if !output.status.success() {
        return Err(format!(
            "failed to resolve git ref from {url}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    pick_ls_remote_commit(&stdout)
        .map(str::to_string)
        .ok_or_else(|| format!("could not resolve {requested} from {url}").into())
}

/// Pick the commit SHA from `git ls-remote` output.
///
/// Annotated tags surface as two refs: `refs/tags/X` (the tag object) and
/// `refs/tags/X^{}` (the commit the tag points at). Prefer the peeled form so
/// the lockfile records the commit SHA, not the tag-object SHA — checking out
/// the tag object still recovers the commit, but the SHA recorded in the lock
/// is less surprising and round-trips through normal git commands.
pub(super) fn pick_ls_remote_commit(stdout: &str) -> Option<&str> {
    let parsed: Vec<(&str, &str)> = stdout
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let sha = parts.next()?;
            let refname = parts.next().unwrap_or("");
            is_full_git_sha(sha).then_some((sha, refname))
        })
        .collect();
    parsed
        .iter()
        .find_map(|(sha, refname)| refname.ends_with("^{}").then_some(*sha))
        .or_else(|| parsed.first().map(|(sha, _)| *sha))
}

pub(crate) fn clone_git_commit_to(
    url: &str,
    commit: &str,
    dest: &Path,
) -> Result<(), PackageError> {
    if dest.exists() {
        fs::remove_dir_all(dest)
            .map_err(|error| format!("failed to reset {}: {error}", dest.display()))?;
    }
    fs::create_dir_all(dest)
        .map_err(|error| format!("failed to create {}: {error}", dest.display()))?;

    let init = git_output(["init", "--quiet"], Cwd::In(dest))?;
    if !init.status.success() {
        return Err(format!(
            "failed to initialize git repo in {}: {}",
            dest.display(),
            String::from_utf8_lossy(&init.stderr).trim()
        )
        .into());
    }

    let remote = git_output(["remote", "add", "origin", url], Cwd::In(dest))?;
    if !remote.status.success() {
        return Err(format!(
            "failed to add git remote {url}: {}",
            String::from_utf8_lossy(&remote.stderr).trim()
        )
        .into());
    }

    let fetch = git_output(["fetch", "--depth", "1", "origin", commit], Cwd::In(dest))?;
    if !fetch.status.success() {
        let fallback_dir = dest.with_extension("full-clone");
        if fallback_dir.exists() {
            fs::remove_dir_all(&fallback_dir)
                .map_err(|error| format!("failed to remove {}: {error}", fallback_dir.display()))?;
        }
        let fallback_dir_arg = fallback_dir.to_string_lossy();
        let clone = git_output(
            CANONICAL_CHECKOUT_CONFIG.iter().copied().chain([
                "clone",
                url,
                fallback_dir_arg.as_ref(),
            ]),
            // `fallback_dir` is an absolute destination — no working tree needed.
            Cwd::Detached,
        )?;
        if !clone.status.success() {
            return Err(format!(
                "failed to fetch {commit} from {url}: {}",
                String::from_utf8_lossy(&fetch.stderr).trim()
            )
            .into());
        }
        let checkout = git_output(
            CANONICAL_CHECKOUT_CONFIG
                .iter()
                .copied()
                .chain(["checkout", commit]),
            Cwd::In(&fallback_dir),
        )?;
        if !checkout.status.success() {
            return Err(format!(
                "failed to checkout {commit} in {}: {}",
                fallback_dir.display(),
                String::from_utf8_lossy(&checkout.stderr).trim()
            )
            .into());
        }
        fs::remove_dir_all(dest)
            .map_err(|error| format!("failed to remove {}: {error}", dest.display()))?;
        fs::rename(&fallback_dir, dest).map_err(|error| {
            format!(
                "failed to move {} to {}: {error}",
                fallback_dir.display(),
                dest.display()
            )
        })?;
    } else {
        let checkout = git_output(
            CANONICAL_CHECKOUT_CONFIG
                .iter()
                .copied()
                .chain(["checkout", "--detach", "FETCH_HEAD"]),
            Cwd::In(dest),
        )?;
        if !checkout.status.success() {
            return Err(format!(
                "failed to checkout FETCH_HEAD in {}: {}",
                dest.display(),
                String::from_utf8_lossy(&checkout.stderr).trim()
            )
            .into());
        }
    }

    let git_dir = dest.join(".git");
    if git_dir.exists() {
        fs::remove_dir_all(&git_dir)
            .map_err(|error| format!("failed to remove {}: {error}", git_dir.display()))?;
    }
    Ok(())
}

pub(crate) fn unique_temp_dir(base: &Path, label: &str) -> Result<PathBuf, PackageError> {
    for _ in 0..16 {
        let suffix = uuid::Uuid::now_v7();
        let candidate = base.join(format!("{label}-{suffix}"));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(format!(
        "failed to allocate a unique temporary directory under {}",
        base.display()
    )
    .into())
}
