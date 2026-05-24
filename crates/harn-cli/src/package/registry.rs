use super::errors::PackageError;
use super::*;
use semver::{Version, VersionReq};

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PackageCacheMetadata {
    version: u32,
    source: String,
    commit: String,
    content_hash: String,
    cached_at_unix_ms: u128,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PackageRegistryIndex {
    version: u32,
    #[serde(default, rename = "package")]
    packages: Vec<RegistryPackage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RegistryPackage {
    name: String,
    #[serde(default)]
    description: Option<String>,
    repository: String,
    #[serde(default)]
    license: Option<String>,
    #[serde(default, alias = "harn_version", alias = "harn_version_range")]
    harn: Option<String>,
    #[serde(default)]
    exports: Vec<String>,
    #[serde(default, alias = "connector-contract")]
    connector_contract: Option<String>,
    #[serde(default)]
    docs_url: Option<String>,
    #[serde(default)]
    checksum: Option<String>,
    #[serde(default)]
    provenance: Option<String>,
    #[serde(default, rename = "version")]
    versions: Vec<RegistryPackageVersion>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RegistryPackageVersion {
    version: String,
    git: String,
    #[serde(default)]
    tag: Option<String>,
    #[serde(default)]
    rev: Option<String>,
    #[serde(default)]
    sha: Option<String>,
    #[serde(default)]
    branch: Option<String>,
    #[serde(default)]
    package: Option<String>,
    #[serde(default)]
    checksum: Option<String>,
    #[serde(default)]
    provenance: Option<String>,
    #[serde(default)]
    yanked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct RegistryPackageInfo {
    package: RegistryPackage,
    selected_version: Option<RegistryPackageVersion>,
}

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

pub(crate) fn cache_root() -> Result<PathBuf, PackageError> {
    PackageWorkspace::from_current_dir()?.cache_root()
}

pub(crate) fn sha256_hex(bytes: impl AsRef<[u8]>) -> String {
    hex_bytes(Sha256::digest(bytes.as_ref()))
}

pub(crate) fn hex_bytes(bytes: impl AsRef<[u8]>) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let bytes = bytes.as_ref();
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

pub(crate) fn git_cache_dir_in(
    workspace: &PackageWorkspace,
    source: &str,
    commit: &str,
) -> Result<PathBuf, PackageError> {
    Ok(workspace
        .cache_root()?
        .join("git")
        .join(sha256_hex(source))
        .join(commit))
}

pub(crate) fn git_cache_lock_path_in(
    workspace: &PackageWorkspace,
    source: &str,
    commit: &str,
) -> Result<PathBuf, PackageError> {
    Ok(workspace
        .cache_root()?
        .join("locks")
        .join(format!("{}-{commit}.lock", sha256_hex(source))))
}

pub(crate) fn acquire_git_cache_lock_in(
    workspace: &PackageWorkspace,
    source: &str,
    commit: &str,
) -> Result<File, PackageError> {
    let path = git_cache_lock_path_in(workspace, source, commit)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    }
    let file = File::create(&path)
        .map_err(|error| format!("failed to open {}: {error}", path.display()))?;
    file.lock_exclusive()
        .map_err(|error| format!("failed to lock {}: {error}", path.display()))?;
    Ok(file)
}

pub(crate) fn read_cached_content_hash(dir: &Path) -> Result<Option<String>, PackageError> {
    let path = dir.join(CONTENT_HASH_FILE);
    match fs::read_to_string(&path) {
        Ok(value) => Ok(Some(value.trim().to_string())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("failed to read {}: {error}", path.display()).into()),
    }
}

pub(crate) fn write_cached_content_hash(dir: &Path, hash: &str) -> Result<(), PackageError> {
    let path = dir.join(CONTENT_HASH_FILE);
    harn_vm::atomic_io::atomic_write(&path, format!("{hash}\n").as_bytes()).map_err(|error| {
        PackageError::Registry(format!("failed to write {}: {error}", path.display()))
    })
}

pub(crate) fn read_cache_metadata(
    dir: &Path,
) -> Result<Option<PackageCacheMetadata>, PackageError> {
    let path = dir.join(CACHE_METADATA_FILE);
    let content = match fs::read_to_string(&path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("failed to read {}: {error}", path.display()).into()),
    };
    let metadata = toml::from_str::<PackageCacheMetadata>(&content)
        .map_err(|error| format!("failed to parse {}: {error}", path.display()))?;
    if metadata.version != CACHE_METADATA_VERSION {
        return Err(format!(
            "unsupported {} version {} (expected {})",
            path.display(),
            metadata.version,
            CACHE_METADATA_VERSION
        )
        .into());
    }
    Ok(Some(metadata))
}

pub(crate) fn write_cache_metadata(
    dir: &Path,
    source: &str,
    commit: &str,
    content_hash: &str,
) -> Result<(), PackageError> {
    let cached_at_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("system clock error: {error}"))?
        .as_millis();
    let metadata = PackageCacheMetadata {
        version: CACHE_METADATA_VERSION,
        source: source.to_string(),
        commit: commit.to_string(),
        content_hash: content_hash.to_string(),
        cached_at_unix_ms,
    };
    let body = toml::to_string_pretty(&metadata)
        .map_err(|error| format!("failed to encode cache metadata: {error}"))?;
    let path = dir.join(CACHE_METADATA_FILE);
    harn_vm::atomic_io::atomic_write(&path, body.as_bytes()).map_err(|error| {
        PackageError::Registry(format!("failed to write {}: {error}", path.display()))
    })
}

pub(crate) fn normalized_relative_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

pub(crate) fn collect_hashable_files(
    root: &Path,
    cursor: &Path,
    out: &mut Vec<PathBuf>,
) -> Result<(), PackageError> {
    for entry in fs::read_dir(cursor)
        .map_err(|error| format!("failed to read {}: {error}", cursor.display()))?
    {
        let entry =
            entry.map_err(|error| format!("failed to read {} entry: {error}", cursor.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| format!("failed to stat {}: {error}", path.display()))?;
        let name = entry.file_name();
        if name == OsStr::new(".git")
            || name == OsStr::new(".gitignore")
            || name == OsStr::new(CONTENT_HASH_FILE)
            || name == OsStr::new(CACHE_METADATA_FILE)
        {
            continue;
        }
        if file_type.is_dir() {
            collect_hashable_files(root, &path, out)?;
        } else if file_type.is_file() {
            let relative = path
                .strip_prefix(root)
                .map_err(|error| format!("failed to relativize {}: {error}", path.display()))?;
            out.push(relative.to_path_buf());
        }
    }
    Ok(())
}

pub(crate) fn compute_content_hash(dir: &Path) -> Result<String, PackageError> {
    let mut files = Vec::new();
    collect_hashable_files(dir, dir, &mut files)?;
    files.sort();
    let mut hasher = Sha256::new();
    for relative in files {
        let normalized = normalized_relative_path(&relative);
        let contents = fs::read(dir.join(&relative)).map_err(|error| {
            format!("failed to read {}: {error}", dir.join(&relative).display())
        })?;
        hasher.update(normalized.as_bytes());
        hasher.update([0]);
        hasher.update(sha256_hex(contents).as_bytes());
    }
    Ok(format!("sha256:{}", hex_bytes(hasher.finalize())))
}

pub(crate) fn verify_content_hash_or_compute(
    dir: &Path,
    expected: &str,
) -> Result<(), PackageError> {
    let actual = compute_content_hash(dir)?;
    if actual != expected {
        return Err(format!(
            "content hash mismatch for {}: expected {}, got {}",
            dir.display(),
            expected,
            actual
        )
        .into());
    }
    if read_cached_content_hash(dir)?.as_deref() != Some(expected) {
        write_cached_content_hash(dir, expected)?;
    }
    Ok(())
}

pub(crate) fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), PackageError> {
    fs::create_dir_all(dst)
        .map_err(|error| format!("failed to create {}: {error}", dst.display()))?;
    for entry in
        fs::read_dir(src).map_err(|error| format!("failed to read {}: {error}", src.display()))?
    {
        let entry =
            entry.map_err(|error| format!("failed to read {} entry: {error}", src.display()))?;
        let ty = entry
            .file_type()
            .map_err(|error| format!("failed to stat {}: {error}", entry.path().display()))?;
        let name = entry.file_name();
        if name == OsStr::new(".git")
            || name == OsStr::new(CONTENT_HASH_FILE)
            || name == OsStr::new(CACHE_METADATA_FILE)
        {
            continue;
        }
        let dest_path = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_recursive(&entry.path(), &dest_path)?;
        } else if ty.is_file() {
            if let Some(parent) = dest_path.parent() {
                fs::create_dir_all(parent)
                    .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
            }
            fs::copy(entry.path(), &dest_path).map_err(|error| {
                format!(
                    "failed to copy {} to {}: {error}",
                    entry.path().display(),
                    dest_path.display()
                )
            })?;
        }
    }
    Ok(())
}

pub(crate) fn remove_materialized_package(
    packages_dir: &Path,
    alias: &str,
) -> Result<(), PackageError> {
    remove_materialized_path(&packages_dir.join(alias))?;
    remove_materialized_path(&packages_dir.join(format!("{alias}.harn")))?;
    Ok(())
}

fn remove_materialized_path(path: &Path) -> Result<(), PackageError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if is_link_like(&metadata) => remove_link_like_path(path)
            .map_err(|error| format!("failed to remove {}: {error}", path.display()).into()),
        Ok(metadata) if metadata.is_file() => fs::remove_file(path)
            .map_err(|error| format!("failed to remove {}: {error}", path.display()).into()),
        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(path)
            .map_err(|error| format!("failed to remove {}: {error}", path.display()).into()),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("failed to stat {}: {error}", path.display()).into()),
    }
}

fn is_link_like(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink() || is_windows_reparse_point(metadata)
}

#[cfg(windows)]
fn is_windows_reparse_point(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_windows_reparse_point(_metadata: &fs::Metadata) -> bool {
    false
}

fn remove_link_like_path(path: &Path) -> std::io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(file_error) => match fs::remove_dir(path) {
            Ok(()) => Ok(()),
            Err(_) => Err(file_error),
        },
    }
}

#[cfg(unix)]
pub(crate) fn symlink_path_dependency(source: &Path, dest: &Path) -> Result<(), PackageError> {
    std::os::unix::fs::symlink(source, dest).map_err(|error| {
        PackageError::Registry(format!(
            "failed to symlink {} to {}: {error}",
            source.display(),
            dest.display()
        ))
    })
}

#[cfg(windows)]
pub(crate) fn symlink_path_dependency(source: &Path, dest: &Path) -> Result<(), PackageError> {
    if source.is_dir() {
        std::os::windows::fs::symlink_dir(source, dest)
    } else {
        std::os::windows::fs::symlink_file(source, dest)
    }
    .map_err(|error| {
        PackageError::Registry(format!(
            "failed to symlink {} to {}: {error}",
            source.display(),
            dest.display()
        ))
    })
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn symlink_path_dependency(_source: &Path, _dest: &Path) -> Result<(), PackageError> {
    Err("symlinks are not supported on this platform"
        .to_string()
        .into())
}

pub(crate) fn materialize_path_dependency(
    source: &Path,
    dest_root: &Path,
    alias: &str,
) -> Result<(), PackageError> {
    remove_materialized_package(dest_root, alias)?;
    if source.is_dir() {
        let dest = dest_root.join(alias);
        match symlink_path_dependency(source, &dest) {
            Ok(()) => Ok(()),
            Err(_) => copy_dir_recursive(source, &dest),
        }
    } else {
        let dest = dest_root.join(format!("{alias}.harn"));
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
        }
        match symlink_path_dependency(source, &dest) {
            Ok(()) => Ok(()),
            Err(_) => {
                fs::copy(source, &dest).map_err(|error| {
                    format!(
                        "failed to copy {} to {}: {error}",
                        source.display(),
                        dest.display()
                    )
                })?;
                Ok(())
            }
        }
    }
}

pub(crate) fn materialized_hash_matches(dir: &Path, expected: &str) -> bool {
    verify_content_hash_or_compute(dir, expected).is_ok()
}

pub(crate) fn resolve_path_dependency_source(
    manifest_dir: &Path,
    raw: &str,
) -> Result<PathBuf, PackageError> {
    let source = {
        let candidate = PathBuf::from(raw);
        if candidate.is_absolute() {
            candidate
        } else {
            manifest_dir.join(candidate)
        }
    };
    if source.exists() {
        return source.canonicalize().map_err(|error| {
            PackageError::Registry(format!(
                "failed to canonicalize {}: {error}",
                source.display()
            ))
        });
    }
    if source.extension().is_none() {
        let with_ext = source.with_extension("harn");
        if with_ext.exists() {
            return with_ext.canonicalize().map_err(|error| {
                PackageError::Registry(format!(
                    "failed to canonicalize {}: {error}",
                    with_ext.display()
                ))
            });
        }
    }
    Err(format!("package source not found: {}", source.display()).into())
}

pub(crate) fn path_source_uri(path: &Path) -> Result<String, PackageError> {
    let url = Url::from_file_path(path)
        .map_err(|_| format!("failed to convert {} to file:// URL", path.display()))?;
    Ok(format!("path+{}", url))
}

pub(crate) fn path_from_source_uri(source: &str) -> Result<PathBuf, PackageError> {
    let raw = source
        .strip_prefix("path+")
        .ok_or_else(|| format!("invalid path source: {source}"))?;
    if let Ok(url) = Url::parse(raw) {
        return url
            .to_file_path()
            .map_err(|_| PackageError::Registry(format!("invalid file:// path source: {source}")));
    }
    Ok(PathBuf::from(raw))
}

pub(crate) fn registry_file_url_or_path(raw: &str) -> Result<Option<PathBuf>, PackageError> {
    if let Ok(url) = Url::parse(raw) {
        if url.scheme() == "file" {
            return url.to_file_path().map(Some).map_err(|_| {
                PackageError::Registry(format!("invalid file:// registry URL: {raw}"))
            });
        }
        return Ok(None);
    }
    Ok(Some(PathBuf::from(raw)))
}

pub(crate) fn read_registry_source(source: &str) -> Result<String, PackageError> {
    if let Some(path) = registry_file_url_or_path(source)? {
        return fs::read_to_string(&path).map_err(|error| {
            PackageError::Registry(format!(
                "failed to read package registry {}: {error}",
                path.display()
            ))
        });
    }

    let url = Url::parse(source)
        .map_err(|error| format!("invalid package registry URL {source:?}: {error}"))?;
    match url.scheme() {
        "http" | "https" => {}
        other => return Err(format!("unsupported package registry URL scheme: {other}").into()),
    }
    // `reqwest::blocking` builds its own current-thread tokio runtime and
    // panics if dropped from inside an already-running tokio runtime — which
    // is exactly what `harn add` / `harn install` do today. Hop onto a fresh
    // OS thread so the blocking client's lifetime is fully outside any
    // ambient runtime.
    let source_owned = source.to_string();
    std::thread::scope(|scope| {
        scope
            .spawn(move || fetch_registry_blocking(url, &source_owned))
            .join()
            .map_err(|_| PackageError::Registry("registry fetch thread panicked".to_string()))?
    })
}

fn fetch_registry_blocking(url: Url, source: &str) -> Result<String, PackageError> {
    let response = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|error| format!("failed to build package registry client: {error}"))?
        .get(url)
        .send()
        .map_err(|error| format!("failed to fetch package registry {source}: {error}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("GET {source} returned HTTP {status}").into());
    }
    response.text().map_err(|error| {
        PackageError::Registry(format!("failed to read package registry response: {error}"))
    })
}

pub(crate) fn is_valid_registry_segment(segment: &str) -> bool {
    let mut chars = segment.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_alphanumeric()
        && chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
}

pub(crate) fn is_valid_registry_package_name(name: &str) -> bool {
    let trimmed = name.trim();
    if trimmed != name || trimmed.is_empty() || trimmed.contains("://") || trimmed.ends_with('/') {
        return false;
    }
    if let Some(scoped) = trimmed.strip_prefix('@') {
        let Some((scope, package)) = scoped.split_once('/') else {
            return false;
        };
        return !package.contains('/')
            && is_valid_registry_segment(scope)
            && is_valid_registry_segment(package);
    }
    !trimmed.contains('/') && is_valid_registry_segment(trimmed)
}

pub(crate) fn parse_registry_package_spec(spec: &str) -> Option<(&str, Option<&str>)> {
    let trimmed = spec.trim();
    if !trimmed.starts_with('@') {
        if let Some((name, version)) = trimmed.rsplit_once('@') {
            if is_valid_registry_package_name(name) && !version.trim().is_empty() {
                return Some((name, Some(version)));
            }
        }
        if is_valid_registry_package_name(trimmed) {
            return Some((trimmed, None));
        }
        return None;
    }

    if let Some((name, version)) = trimmed.rsplit_once('@') {
        if !name.is_empty()
            && name != trimmed
            && is_valid_registry_package_name(name)
            && !version.trim().is_empty()
        {
            return Some((name, Some(version)));
        }
    }
    if is_valid_registry_package_name(trimmed) {
        return Some((trimmed, None));
    }
    None
}

pub(crate) fn parse_package_registry_index(
    source: &str,
    content: &str,
) -> Result<PackageRegistryIndex, PackageError> {
    let mut index = toml::from_str::<PackageRegistryIndex>(content)
        .map_err(|error| format!("failed to parse package registry {source}: {error}"))?;
    if index.version != REGISTRY_INDEX_VERSION {
        return Err(format!(
            "unsupported package registry {source} version {} (expected {})",
            index.version, REGISTRY_INDEX_VERSION
        )
        .into());
    }
    validate_package_registry_index(source, &mut index)?;
    Ok(index)
}

pub(crate) fn validate_package_registry_index(
    source: &str,
    index: &mut PackageRegistryIndex,
) -> Result<(), PackageError> {
    let mut names = HashSet::new();
    for package in &mut index.packages {
        if !is_valid_registry_package_name(&package.name) {
            return Err(format!(
                "package registry {source} has invalid package name '{}'",
                package.name
            )
            .into());
        }
        if !names.insert(package.name.clone()) {
            return Err(format!(
                "package registry {source} declares '{}' more than once",
                package.name
            )
            .into());
        }
        normalize_git_url(&package.repository).map_err(|error| {
            format!(
                "package registry {source} has invalid repository for '{}': {error}",
                package.name
            )
        })?;
        let mut versions = HashSet::new();
        for version in &package.versions {
            if version.version.trim().is_empty() {
                return Err(format!(
                    "package registry {source} has empty version for '{}'",
                    package.name
                )
                .into());
            }
            if !versions.insert(version.version.clone()) {
                return Err(format!(
                    "package registry {source} declares '{}@{}' more than once",
                    package.name, version.version
                )
                .into());
            }
            if selected_git_ref_count(version) != 1 {
                return Err(format!(
                    "package registry {source} entry '{}@{}' must specify tag, rev, or branch; rev may accompany tag as a resolved commit pin",
                    package.name, version.version
                )
                .into());
            }
            parse_registry_semver(&version.version).map_err(|error| {
                format!(
                    "package registry {source} has invalid semver for '{}@{}': {error}",
                    package.name, version.version
                )
            })?;
            normalize_git_url(&version.git).map_err(|error| {
                format!(
                    "package registry {source} has invalid git source for '{}@{}': {error}",
                    package.name, version.version
                )
            })?;
        }
    }
    index
        .packages
        .sort_by(|left, right| left.name.cmp(&right.name));
    Ok(())
}

fn selected_git_ref_count(version: &RegistryPackageVersion) -> usize {
    usize::from(version.tag.is_some())
        + usize::from(version.tag.is_none() && version.rev.is_some())
        + usize::from(version.branch.is_some())
}

pub(crate) fn load_package_registry_in(
    workspace: &PackageWorkspace,
    explicit: Option<&str>,
) -> Result<(String, PackageRegistryIndex), PackageError> {
    let source = workspace.resolve_registry_source(explicit)?;
    let content = read_registry_source(&source)?;
    let index = parse_package_registry_index(&source, &content)?;
    Ok((source, index))
}

pub(crate) fn registry_package_matches(package: &RegistryPackage, query: &str) -> bool {
    if query.trim().is_empty() {
        return true;
    }
    let query = query.to_ascii_lowercase();
    package.name.to_ascii_lowercase().contains(&query)
        || package
            .description
            .as_deref()
            .is_some_and(|value| value.to_ascii_lowercase().contains(&query))
        || package.repository.to_ascii_lowercase().contains(&query)
        || package
            .exports
            .iter()
            .any(|export| export.to_ascii_lowercase().contains(&query))
}

pub(crate) fn latest_registry_version(
    package: &RegistryPackage,
) -> Option<&RegistryPackageVersion> {
    package
        .versions
        .iter()
        .filter(|version| !version.yanked)
        .filter_map(|version| {
            parse_registry_semver(&version.version)
                .ok()
                .map(|semver| (semver, version))
        })
        .max_by(|(left, _), (right, _)| left.cmp(right))
        .map(|(_, version)| version)
}

impl PackageRegistryIndex {
    pub(crate) fn latest_unyanked_version(&self, name: &str) -> Option<&str> {
        self.packages
            .iter()
            .find(|package| package.name == name)
            .and_then(latest_registry_version)
            .map(|version| version.version.as_str())
    }

    pub(crate) fn is_version_yanked(&self, name: &str, version: &str) -> bool {
        self.packages
            .iter()
            .find(|package| package.name == name)
            .into_iter()
            .flat_map(|package| package.versions.iter())
            .any(|entry| entry.version == version && entry.yanked)
    }
}

pub(crate) fn parse_registry_semver(raw: &str) -> Result<Version, PackageError> {
    Version::parse(raw.trim().trim_start_matches('v'))
        .map_err(|error| PackageError::Registry(error.to_string()))
}

pub(crate) fn parse_registry_version_req(raw: &str) -> Result<VersionReq, PackageError> {
    VersionReq::parse(&normalize_registry_version_req(raw)).map_err(|error| {
        PackageError::Registry(format!("invalid version requirement {raw:?}: {error}"))
    })
}

fn normalize_registry_version_req(raw: &str) -> String {
    raw.split(',')
        .map(|part| normalize_version_req_part(part.trim()))
        .collect::<Vec<_>>()
        .join(",")
}

fn normalize_version_req_part(part: &str) -> String {
    for op in ["<=", ">=", "!=", "=", "<", ">", "^", "~"] {
        if let Some(rest) = part.strip_prefix(op) {
            return format!("{op}{}", normalize_partial_version(rest.trim()));
        }
    }
    normalize_partial_version(part)
}

fn normalize_partial_version(raw: &str) -> String {
    let trimmed = raw.trim().trim_start_matches('v');
    if trimmed == "*" || trimmed.eq_ignore_ascii_case("x") {
        return trimmed.to_string();
    }
    let (core, suffix) = trimmed
        .find(['-', '+'])
        .map(|index| (&trimmed[..index], &trimmed[index..]))
        .unwrap_or((trimmed, ""));
    let mut parts = core.split('.').collect::<Vec<_>>();
    if (1..=2).contains(&parts.len())
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
    {
        while parts.len() < 3 {
            parts.push("0");
        }
        return format!("{}{}", parts.join("."), suffix);
    }
    trimmed.to_string()
}

/// Look up a registry package by either its scoped registry name
/// (`@burin/notion-sdk`) or any `[[package.version]].package` alias
/// (`notion-sdk-harn`). Bare-name lookup falls back to the alias so
/// `harn add notion-sdk-harn@0.1.0` works the same as the scoped form.
fn lookup_registry_package<'a>(
    index: &'a PackageRegistryIndex,
    name: &str,
) -> Result<&'a RegistryPackage, PackageError> {
    if let Some(package) = index.packages.iter().find(|package| package.name == name) {
        return Ok(package);
    }
    let matches: Vec<&RegistryPackage> = index
        .packages
        .iter()
        .filter(|package| {
            package
                .versions
                .iter()
                .any(|entry| entry.package.as_deref() == Some(name))
        })
        .collect();
    match matches.as_slice() {
        [package] => Ok(package),
        [] => Err(format!("package registry does not contain {name}").into()),
        many => Err(format!(
            "package alias {name} is ambiguous in the registry — found {} packages; use the scoped name (e.g. {})",
            many.len(),
            many[0].name,
        )
        .into()),
    }
}

pub(crate) fn find_registry_package_version(
    index: &PackageRegistryIndex,
    name: &str,
    version: Option<&str>,
) -> Result<RegistryPackageInfo, PackageError> {
    let package = lookup_registry_package(index, name)?;
    let selected_version = match version {
        Some(version) => Some(
            package
                .versions
                .iter()
                .find(|entry| entry.version == version)
                .ok_or_else(|| format!("package registry does not contain {name}@{version}"))?
                .clone(),
        ),
        None => latest_registry_version(package).cloned(),
    };
    Ok(RegistryPackageInfo {
        package: package.clone(),
        selected_version,
    })
}

pub(crate) fn find_registry_package_version_matching(
    index: &PackageRegistryIndex,
    name: &str,
    requirement: &str,
) -> Result<RegistryPackageInfo, PackageError> {
    let package = lookup_registry_package(index, name)?;
    let req = parse_registry_version_req(requirement)?;
    let selected_version = package
        .versions
        .iter()
        .filter(|entry| !entry.yanked)
        .filter_map(|entry| {
            parse_registry_semver(&entry.version)
                .ok()
                .filter(|version| req.matches(version))
                .map(|version| (version, entry.clone()))
        })
        .max_by(|(left, _), (right, _)| left.cmp(right))
        .map(|(_, entry)| entry)
        .ok_or_else(|| {
            format!("package registry does not contain {name} matching {requirement}")
        })?;
    Ok(RegistryPackageInfo {
        package: package.clone(),
        selected_version: Some(selected_version),
    })
}

pub(crate) fn search_package_registry_impl(
    query: Option<&str>,
    registry: Option<&str>,
) -> Result<Vec<RegistryPackage>, PackageError> {
    search_package_registry_in(&PackageWorkspace::from_current_dir()?, query, registry)
}

pub(crate) fn search_package_registry_in(
    workspace: &PackageWorkspace,
    query: Option<&str>,
    registry: Option<&str>,
) -> Result<Vec<RegistryPackage>, PackageError> {
    let (_, index) = load_package_registry_in(workspace, registry)?;
    Ok(index
        .packages
        .into_iter()
        .filter(|package| registry_package_matches(package, query.unwrap_or("")))
        .collect())
}

pub(crate) fn package_registry_info_impl(
    spec: &str,
    registry: Option<&str>,
) -> Result<RegistryPackageInfo, PackageError> {
    package_registry_info_in(&PackageWorkspace::from_current_dir()?, spec, registry)
}

pub(crate) fn package_registry_info_in(
    workspace: &PackageWorkspace,
    spec: &str,
    registry: Option<&str>,
) -> Result<RegistryPackageInfo, PackageError> {
    let Some((name, version)) = parse_registry_package_spec(spec) else {
        return Err(format!(
            "invalid registry package name '{spec}'; use names like @burin/notion-sdk or acme-lib"
        )
        .into());
    };
    let (_, index) = load_package_registry_in(workspace, registry)?;
    find_registry_package_version(&index, name, version)
}

pub(crate) fn registry_dependency_from_spec_in(
    workspace: &PackageWorkspace,
    spec: &str,
    alias: Option<&str>,
    registry: Option<&str>,
) -> Result<(String, Dependency), PackageError> {
    let Some((name, Some(version))) = parse_registry_package_spec(spec) else {
        return Err(format!(
            "registry dependency '{spec}' must include a version, for example {spec}@1.2.3"
        )
        .into());
    };
    let registry_source = workspace.resolve_registry_source(registry)?;
    let (_, index) = load_package_registry_in(workspace, registry)?;
    // Accept both exact versions (`@1.2.3`) and semver constraints
    // (`@^0.1`, `@~1.4`, `@>=1,<2`). The latter resolve to the highest
    // matching unyanked entry.
    let info = if is_exact_semver(version) {
        find_registry_package_version(&index, name, Some(version))?
    } else {
        find_registry_package_version_matching(&index, name, version)?
    };
    let selected = info
        .selected_version
        .ok_or_else(|| format!("package registry does not contain {name}@{version}"))?;
    if selected.yanked {
        return Err(format!("{name}@{version} is yanked in the package registry").into());
    }
    let git = normalize_git_url(&selected.git)?;
    let package_name = selected
        .package
        .clone()
        .map(Ok)
        .unwrap_or_else(|| derive_repo_name_from_source(&git))?;
    let alias = alias.unwrap_or(package_name.as_str()).to_string();
    let tag = selected.tag;
    let rev = if tag.is_some() { None } else { selected.rev };
    let resolved_version = selected.version.clone();
    Ok((
        alias.clone(),
        Dependency::Table(Box::new(DepTable {
            git: Some(git),
            tag,
            rev,
            branch: selected.branch,
            package: (alias != package_name).then_some(package_name),
            registry: Some(registry_source),
            // Store the canonical scoped registry name (e.g. `@burin/notion-sdk`)
            // even when the user typed the bare alias (`notion-sdk-harn`) so
            // re-resolves stay anchored to the same registry row.
            registry_name: Some(info.package.name.clone()),
            registry_version: Some(resolved_version),
            ..DepTable::default()
        })),
    ))
}

fn is_exact_semver(spec: &str) -> bool {
    parse_registry_semver(spec).is_ok()
}

pub(crate) fn registry_dependency_from_manifest_constraint_in(
    workspace: &PackageWorkspace,
    alias: &str,
    table: &DepTable,
) -> Result<Dependency, PackageError> {
    let requirement = table
        .version
        .as_deref()
        .ok_or_else(|| format!("dependency {alias} is missing `version`"))?;
    let registry_source = workspace.resolve_registry_source(table.registry.as_deref())?;
    let registry_name = table.registry_name.as_deref().unwrap_or(alias);
    let (_, index) = load_package_registry_in(workspace, Some(&registry_source))?;
    let info = find_registry_package_version_matching(&index, registry_name, requirement)?;
    let selected = info.selected_version.ok_or_else(|| {
        format!("package registry does not contain {registry_name} matching {requirement}")
    })?;
    let git = normalize_git_url(&selected.git)?;
    let tag = selected.tag;
    let rev = if tag.is_some() { None } else { selected.rev };
    Ok(Dependency::Table(Box::new(DepTable {
        git: Some(git),
        tag,
        rev,
        branch: selected.branch,
        package: selected.package.or_else(|| table.package.clone()),
        registry: Some(registry_source),
        registry_name: Some(registry_name.to_string()),
        registry_version: Some(selected.version),
        ..DepTable::default()
    })))
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
            .map_err(|error| format!("failed to canonicalize {}: {error}", trimmed))?;
        let url = Url::from_file_path(canonical)
            .map_err(|_| format!("failed to convert {} to file:// URL", trimmed))?;
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

struct HardenedGitEnv {
    _temp_dir: tempfile::TempDir,
    home: PathBuf,
    config_home: PathBuf,
    global_config: PathBuf,
    system_config: PathBuf,
}

impl HardenedGitEnv {
    fn new() -> Result<Self, PackageError> {
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

    fn apply_to(&self, command: &mut process::Command, cwd: Option<&Path>) {
        if let Some(dir) = cwd {
            command.current_dir(dir);
        }
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

pub(crate) fn git_output<I, S>(
    args: I,
    cwd: Option<&Path>,
) -> Result<std::process::Output, PackageError>
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
            .chain(refs.clone()),
        None,
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
fn pick_ls_remote_commit(stdout: &str) -> Option<&str> {
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

    let init = git_output(["init", "--quiet"], Some(dest))?;
    if !init.status.success() {
        return Err(format!(
            "failed to initialize git repo in {}: {}",
            dest.display(),
            String::from_utf8_lossy(&init.stderr).trim()
        )
        .into());
    }

    let remote = git_output(["remote", "add", "origin", url], Some(dest))?;
    if !remote.status.success() {
        return Err(format!(
            "failed to add git remote {url}: {}",
            String::from_utf8_lossy(&remote.stderr).trim()
        )
        .into());
    }

    let fetch = git_output(["fetch", "--depth", "1", "origin", commit], Some(dest))?;
    if !fetch.status.success() {
        let fallback_dir = dest.with_extension("full-clone");
        if fallback_dir.exists() {
            fs::remove_dir_all(&fallback_dir)
                .map_err(|error| format!("failed to remove {}: {error}", fallback_dir.display()))?;
        }
        let clone = git_output(
            ["clone", url, fallback_dir.to_string_lossy().as_ref()],
            None,
        )?;
        if !clone.status.success() {
            return Err(format!(
                "failed to fetch {commit} from {url}: {}",
                String::from_utf8_lossy(&fetch.stderr).trim()
            )
            .into());
        }
        let checkout = git_output(["checkout", commit], Some(&fallback_dir))?;
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
        let checkout = git_output(["checkout", "--detach", "FETCH_HEAD"], Some(dest))?;
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

pub(crate) fn ensure_git_cache_populated_in(
    workspace: &PackageWorkspace,
    url: &str,
    source: &str,
    commit: &str,
    expected_hash: Option<&str>,
    refetch: bool,
    offline: bool,
) -> Result<String, PackageError> {
    let cache_dir = git_cache_dir_in(workspace, source, commit)?;
    let _lock = acquire_git_cache_lock_in(workspace, source, commit)?;
    if refetch && cache_dir.exists() {
        fs::remove_dir_all(&cache_dir)
            .map_err(|error| format!("failed to remove {}: {error}", cache_dir.display()))?;
    }
    if cache_dir.exists() {
        if let Some(expected) = expected_hash {
            verify_content_hash_or_compute(&cache_dir, expected)?;
            write_cache_metadata(&cache_dir, source, commit, expected)?;
            return Ok(expected.to_string());
        }
        let hash = compute_content_hash(&cache_dir)?;
        write_cached_content_hash(&cache_dir, &hash)?;
        write_cache_metadata(&cache_dir, source, commit, &hash)?;
        return Ok(hash);
    }

    if offline {
        return Err(format!(
            "package cache entry for {source} at {commit} is missing; cannot fetch in offline mode"
        )
        .into());
    }

    let parent = cache_dir
        .parent()
        .ok_or_else(|| format!("invalid cache path {}", cache_dir.display()))?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    let temp_dir = unique_temp_dir(parent, "tmp")?;
    let populated = (|| -> Result<String, PackageError> {
        clone_git_commit_to(url, commit, &temp_dir)?;
        let hash = compute_content_hash(&temp_dir)?;
        if let Some(expected) = expected_hash {
            if hash != expected {
                return Err(format!(
                    "content hash mismatch for {} at {}: expected {}, got {}",
                    source, commit, expected, hash
                )
                .into());
            }
        }
        write_cached_content_hash(&temp_dir, &hash)?;
        write_cache_metadata(&temp_dir, source, commit, &hash)?;
        fs::rename(&temp_dir, &cache_dir).map_err(|error| {
            format!(
                "failed to move {} to {}: {error}",
                temp_dir.display(),
                cache_dir.display()
            )
        })?;
        Ok(hash)
    })();
    let hash = match populated {
        Ok(hash) => hash,
        Err(error) => {
            let _ = fs::remove_dir_all(&temp_dir);
            return Err(error);
        }
    };
    Ok(hash)
}

#[derive(Debug, Clone)]
pub(crate) struct PackageCacheEntry {
    path: PathBuf,
    source_hash: String,
    commit: String,
    metadata: Option<PackageCacheMetadata>,
}

pub(crate) fn git_cache_root_in(workspace: &PackageWorkspace) -> Result<PathBuf, PackageError> {
    Ok(workspace.cache_root()?.join("git"))
}

pub(crate) fn discover_git_cache_entries() -> Result<Vec<PackageCacheEntry>, PackageError> {
    discover_git_cache_entries_in(&PackageWorkspace::from_current_dir()?)
}

pub(crate) fn discover_git_cache_entries_in(
    workspace: &PackageWorkspace,
) -> Result<Vec<PackageCacheEntry>, PackageError> {
    let root = git_cache_root_in(workspace)?;
    let mut entries = Vec::new();
    let source_dirs = match fs::read_dir(&root) {
        Ok(source_dirs) => source_dirs,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(entries),
        Err(error) => return Err(format!("failed to read {}: {error}", root.display()).into()),
    };
    for source_dir in source_dirs {
        let source_dir = source_dir
            .map_err(|error| format!("failed to read {} entry: {error}", root.display()))?;
        let source_type = source_dir
            .file_type()
            .map_err(|error| format!("failed to stat {}: {error}", source_dir.path().display()))?;
        if !source_type.is_dir() {
            continue;
        }
        let source_hash = source_dir.file_name().to_string_lossy().to_string();
        let commit_dirs = fs::read_dir(source_dir.path())
            .map_err(|error| format!("failed to read {}: {error}", source_dir.path().display()))?;
        for commit_dir in commit_dirs {
            let commit_dir = commit_dir.map_err(|error| {
                format!(
                    "failed to read {} entry: {error}",
                    source_dir.path().display()
                )
            })?;
            let commit_type = commit_dir.file_type().map_err(|error| {
                format!("failed to stat {}: {error}", commit_dir.path().display())
            })?;
            if !commit_type.is_dir() {
                continue;
            }
            let commit = commit_dir.file_name().to_string_lossy().to_string();
            if commit.starts_with("tmp-") || commit.ends_with(".full-clone") {
                continue;
            }
            let metadata = read_cache_metadata(&commit_dir.path())?;
            entries.push(PackageCacheEntry {
                path: commit_dir.path(),
                source_hash: source_hash.clone(),
                commit,
                metadata,
            });
        }
    }
    entries.sort_by(|left, right| {
        left.source_hash
            .cmp(&right.source_hash)
            .then_with(|| left.commit.cmp(&right.commit))
    });
    Ok(entries)
}

pub(crate) fn locked_git_cache_paths_in(
    workspace: &PackageWorkspace,
    lock: &LockFile,
) -> Result<HashSet<PathBuf>, PackageError> {
    let mut keep = HashSet::new();
    for entry in &lock.packages {
        validate_package_alias(&entry.name)?;
        if !entry.source.starts_with("git+") {
            continue;
        }
        let commit = entry
            .commit
            .as_deref()
            .ok_or_else(|| format!("missing locked commit for {}", entry.name))?;
        keep.insert(git_cache_dir_in(workspace, &entry.source, commit)?);
    }
    Ok(keep)
}

pub(crate) fn verify_lock_entry_cache_in(
    workspace: &PackageWorkspace,
    entry: &LockEntry,
) -> Result<bool, PackageError> {
    validate_package_alias(&entry.name)?;
    if !entry.source.starts_with("git+") {
        if entry.source.starts_with("path+") {
            let path = path_from_source_uri(&entry.source)?;
            if !path.exists() {
                return Err(format!(
                    "path dependency {} source is missing: {}",
                    entry.name,
                    path.display()
                )
                .into());
            }
        }
        return Ok(false);
    }
    let commit = entry
        .commit
        .as_deref()
        .ok_or_else(|| format!("missing locked commit for {}", entry.name))?;
    let expected_hash = entry
        .content_hash
        .as_deref()
        .ok_or_else(|| format!("missing content hash for {}", entry.name))?;
    let cache_dir = git_cache_dir_in(workspace, &entry.source, commit)?;
    if !cache_dir.is_dir() {
        return Err(format!(
            "package cache entry for {} is missing: {}",
            entry.name,
            cache_dir.display()
        )
        .into());
    }
    verify_content_hash_or_compute(&cache_dir, expected_hash)?;
    match read_cache_metadata(&cache_dir)? {
        Some(metadata)
            if metadata.source == entry.source
                && metadata.commit == commit
                && metadata.content_hash == expected_hash => {}
        Some(metadata) => {
            return Err(format!(
                "package cache metadata mismatch for {}: expected {} {} {}, got {} {} {}",
                entry.name,
                entry.source,
                commit,
                expected_hash,
                metadata.source,
                metadata.commit,
                metadata.content_hash
            )
            .into());
        }
        None => write_cache_metadata(&cache_dir, &entry.source, commit, expected_hash)?,
    }
    Ok(true)
}

pub(crate) fn verify_materialized_lock_entry(
    ctx: &ManifestContext,
    entry: &LockEntry,
) -> Result<bool, PackageError> {
    validate_package_alias(&entry.name)?;
    let packages_dir = ctx.packages_dir();
    if entry.source.starts_with("path+") {
        let dir = packages_dir.join(&entry.name);
        let file = packages_dir.join(format!("{}.harn", entry.name));
        if !dir.exists() && !file.exists() {
            return Err(format!(
                "materialized path dependency {} is missing under {}",
                entry.name,
                packages_dir.display()
            )
            .into());
        }
        return Ok(true);
    }
    if !entry.source.starts_with("git+") {
        return Ok(false);
    }
    let expected_hash = entry
        .content_hash
        .as_deref()
        .ok_or_else(|| format!("missing content hash for {}", entry.name))?;
    let dest_dir = packages_dir.join(&entry.name);
    if !dest_dir.is_dir() {
        return Err(format!(
            "materialized package {} is missing: {}",
            entry.name,
            dest_dir.display()
        )
        .into());
    }
    verify_content_hash_or_compute(&dest_dir, expected_hash)?;
    Ok(true)
}

pub(crate) fn verify_package_cache_impl(materialized: bool) -> Result<usize, PackageError> {
    verify_package_cache_in(&PackageWorkspace::from_current_dir()?, materialized)
}

pub(crate) fn verify_package_cache_in(
    workspace: &PackageWorkspace,
    materialized: bool,
) -> Result<usize, PackageError> {
    let ctx = workspace.load_manifest_context()?;
    let lock = LockFile::load(&ctx.lock_path())?
        .ok_or_else(|| format!("{} is missing", ctx.lock_path().display()))?;
    validate_lock_matches_manifest(workspace, &ctx, &lock)?;
    let mut verified = 0usize;
    for entry in &lock.packages {
        if verify_lock_entry_cache_in(workspace, entry)? {
            verified += 1;
        }
        if materialized && verify_materialized_lock_entry(&ctx, entry)? {
            verified += 1;
        }
    }
    Ok(verified)
}

pub(crate) fn clean_package_cache_impl(all: bool) -> Result<usize, PackageError> {
    clean_package_cache_in(&PackageWorkspace::from_current_dir()?, all)
}

pub(crate) fn clean_package_cache_in(
    workspace: &PackageWorkspace,
    all: bool,
) -> Result<usize, PackageError> {
    let entries = discover_git_cache_entries_in(workspace)?;
    if entries.is_empty() {
        return Ok(0);
    }
    if all {
        let root = workspace.cache_root()?;
        for child in ["git", "locks"] {
            let path = root.join(child);
            if path.exists() {
                fs::remove_dir_all(&path)
                    .map_err(|error| format!("failed to remove {}: {error}", path.display()))?;
            }
        }
        return Ok(entries.len());
    }

    let ctx = workspace.load_manifest_context()?;
    let lock = LockFile::load(&ctx.lock_path())?.ok_or_else(|| {
        format!(
            "{} is missing; pass --all to clean every cache entry",
            LOCK_FILE
        )
    })?;
    validate_lock_matches_manifest(workspace, &ctx, &lock)?;
    let keep = locked_git_cache_paths_in(workspace, &lock)?;
    let mut removed = 0usize;
    for entry in entries {
        if keep.contains(&entry.path) {
            continue;
        }
        fs::remove_dir_all(&entry.path)
            .map_err(|error| format!("failed to remove {}: {error}", entry.path.display()))?;
        removed += 1;
        if let Some(parent) = entry.path.parent() {
            let is_empty = fs::read_dir(parent)
                .map(|mut children| children.next().is_none())
                .unwrap_or(false);
            if is_empty {
                fs::remove_dir(parent)
                    .map_err(|error| format!("failed to remove {}: {error}", parent.display()))?;
            }
        }
    }
    Ok(removed)
}

pub fn list_package_cache() {
    let result = (|| -> Result<(PathBuf, Vec<PackageCacheEntry>), PackageError> {
        Ok((cache_root()?, discover_git_cache_entries()?))
    })();

    match result {
        Ok((root, entries)) => {
            println!("Cache root: {}", root.display());
            if entries.is_empty() {
                println!("No cached git packages.");
                return;
            }
            println!("commit\tcontent_hash\tsource\tpath");
            for entry in entries {
                let (source, content_hash) = entry
                    .metadata
                    .as_ref()
                    .map(|metadata| (metadata.source.as_str(), metadata.content_hash.as_str()))
                    .unwrap_or(("(unknown)", "(unknown)"));
                println!(
                    "{}\t{}\t{}\t{}",
                    entry.commit,
                    content_hash,
                    source,
                    entry.path.display()
                );
            }
        }
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    }
}

pub fn clean_package_cache(all: bool) {
    match clean_package_cache_impl(all) {
        Ok(removed) => println!("Removed {removed} cached package entries."),
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    }
}

pub fn verify_package_cache(materialized: bool) {
    match verify_package_cache_impl(materialized) {
        Ok(verified) => println!("Verified {verified} package cache entries."),
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    }
}

pub fn search_package_registry(query: Option<&str>, registry: Option<&str>, json: bool) {
    match search_package_registry_impl(query, registry) {
        Ok(packages) if json => {
            println!(
                "{}",
                serde_json::to_string_pretty(&packages)
                    .unwrap_or_else(|error| format!(r#"{{"error":"{error}"}}"#))
            );
        }
        Ok(packages) => {
            if packages.is_empty() {
                println!("No packages found.");
                return;
            }
            println!("name\tlatest\tharn\tcontract\tdescription");
            for package in packages {
                let latest = latest_registry_version(&package)
                    .map(|version| version.version.as_str())
                    .unwrap_or("-");
                println!(
                    "{}\t{}\t{}\t{}\t{}",
                    package.name,
                    latest,
                    package.harn.as_deref().unwrap_or("-"),
                    package.connector_contract.as_deref().unwrap_or("-"),
                    package.description.as_deref().unwrap_or("")
                );
            }
        }
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    }
}

pub fn show_package_registry_info(spec: &str, registry: Option<&str>, json: bool) {
    match package_registry_info_impl(spec, registry) {
        Ok(info) if json => {
            println!(
                "{}",
                serde_json::to_string_pretty(&info)
                    .unwrap_or_else(|error| format!(r#"{{"error":"{error}"}}"#))
            );
        }
        Ok(info) => {
            let package = info.package;
            println!("{}", package.name);
            if let Some(description) = package.description.as_deref() {
                println!("description: {description}");
            }
            println!("repository: {}", package.repository);
            if let Some(license) = package.license.as_deref() {
                println!("license: {license}");
            }
            if let Some(harn) = package.harn.as_deref() {
                println!("harn: {harn}");
            }
            if let Some(contract) = package.connector_contract.as_deref() {
                println!("connector_contract: {contract}");
            }
            if let Some(docs) = package.docs_url.as_deref() {
                println!("docs: {docs}");
            }
            if let Some(checksum) = package.checksum.as_deref() {
                println!("checksum: {checksum}");
            }
            if let Some(provenance) = package.provenance.as_deref() {
                println!("provenance: {provenance}");
            }
            if !package.exports.is_empty() {
                println!("exports: {}", package.exports.join(", "));
            }
            if let Some(version) = info.selected_version {
                println!("selected: {}", version.version);
                println!("git: {}", version.git);
                if let Some(rev) = version.rev.as_deref() {
                    println!("rev: {rev}");
                }
                if let Some(branch) = version.branch.as_deref() {
                    println!("branch: {branch}");
                }
                if let Some(package_name) = version.package.as_deref() {
                    println!("package: {package_name}");
                }
            }
            if !package.versions.is_empty() {
                let versions = package
                    .versions
                    .iter()
                    .map(|version| {
                        if version.yanked {
                            format!("{} (yanked)", version.version)
                        } else {
                            version.version.clone()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                println!("versions: {versions}");
            }
        }
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package::test_support::*;

    #[test]
    fn pick_ls_remote_commit_prefers_peeled_tag_over_tag_object() {
        // Real-world example from notion-sdk-harn v0.1.0: the tag is
        // annotated, so ls-remote returns both the tag-object SHA and the
        // commit it points at.
        let output = "\
963b6e8acfdf030a9b922bc5a73e010758ff47da\trefs/tags/v0.1.0\n\
bad580c5fbe8ede612b2748ad98606642ce2fc02\trefs/tags/v0.1.0^{}\n";
        assert_eq!(
            pick_ls_remote_commit(output),
            Some("bad580c5fbe8ede612b2748ad98606642ce2fc02"),
        );
    }

    #[test]
    fn pick_ls_remote_commit_falls_back_to_first_match_for_lightweight_tags() {
        let output = "\
abc123abc123abc123abc123abc123abc1234567\trefs/tags/v0.0.1\n";
        assert_eq!(
            pick_ls_remote_commit(output),
            Some("abc123abc123abc123abc123abc123abc1234567"),
        );
    }

    #[test]
    fn pick_ls_remote_commit_returns_none_on_empty_output() {
        assert_eq!(pick_ls_remote_commit(""), None);
    }

    #[cfg(unix)]
    #[test]
    fn hardened_git_env_scrubs_ambient_git_credentials_and_config() {
        let git_env = HardenedGitEnv::new().unwrap();
        let mut command = process::Command::new("/usr/bin/env");
        command
            .env("HOME", "/sensitive/home")
            .env("XDG_CONFIG_HOME", "/sensitive/config")
            .env("GIT_ASKPASS", "/sensitive/askpass")
            .env("GIT_SSH_COMMAND", "ssh -i /sensitive/key")
            .env("SSH_AUTH_SOCK", "/sensitive/agent.sock")
            .env("GIT_CONFIG_COUNT", "1")
            .env(
                "GIT_CONFIG_KEY_0",
                "http.https://attacker.example/.extraheader",
            )
            .env("GIT_CONFIG_VALUE_0", "Authorization: bearer secret");
        git_env.apply_to(&mut command, None);

        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "env probe failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).unwrap();
        let vars: std::collections::BTreeMap<_, _> = stdout
            .lines()
            .filter_map(|line| line.split_once('='))
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect();

        assert_eq!(Path::new(&vars["HOME"]), git_env.home);
        assert_eq!(Path::new(&vars["XDG_CONFIG_HOME"]), git_env.config_home);
        assert_eq!(Path::new(&vars["GIT_CONFIG_GLOBAL"]), git_env.global_config);
        assert_eq!(Path::new(&vars["GIT_CONFIG_SYSTEM"]), git_env.system_config);
        assert_eq!(vars["GIT_CONFIG_NOSYSTEM"], "1");
        assert_eq!(vars["GIT_TERMINAL_PROMPT"], "0");
        assert!(!vars.contains_key("GIT_ASKPASS"));
        assert!(!vars.contains_key("GIT_SSH_COMMAND"));
        assert!(!vars.contains_key("SSH_AUTH_SOCK"));
        assert!(!vars.contains_key("GIT_CONFIG_COUNT"));
        assert!(!vars.contains_key("GIT_CONFIG_KEY_0"));
        assert!(!vars.contains_key("GIT_CONFIG_VALUE_0"));
    }

    #[test]
    fn compute_content_hash_ignores_git_and_hash_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::write(root.join(".gitignore"), "ignored\n").unwrap();
        fs::write(root.join(CONTENT_HASH_FILE), "stale\n").unwrap();
        fs::write(
            root.join("lib.harn"),
            "pub fn value() -> number { return 1 }\n",
        )
        .unwrap();
        let first = compute_content_hash(root).unwrap();
        fs::write(root.join(".git/HEAD"), "changed\n").unwrap();
        fs::write(root.join(".gitignore"), "changed\n").unwrap();
        fs::write(root.join(CONTENT_HASH_FILE), "changed\n").unwrap();
        let second = compute_content_hash(root).unwrap();
        assert_eq!(first, second);
    }

    #[cfg(unix)]
    #[test]
    fn remove_materialized_package_unlinks_directory_symlink_without_touching_source() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source");
        let packages = tmp.path().join(".harn/packages");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&packages).unwrap();
        fs::write(
            source.join("lib.harn"),
            "pub fn value() -> number { return 1 }\n",
        )
        .unwrap();

        let materialized = packages.join("acme");
        std::os::unix::fs::symlink(&source, &materialized).unwrap();

        remove_materialized_package(&packages, "acme").unwrap();

        assert!(!materialized.exists());
        assert!(source.join("lib.harn").is_file());
    }

    #[test]
    fn package_cache_verify_detects_tampering_even_with_stale_marker() {
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

        let error = verify_package_cache_in(workspace.env(), false).unwrap_err();
        assert!(error.to_string().contains("content hash mismatch"));
    }

    #[test]
    fn package_cache_clean_all_removes_cached_git_entries() {
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
        assert_eq!(
            discover_git_cache_entries_in(workspace.env())
                .unwrap()
                .len(),
            1
        );

        let removed = clean_package_cache_in(workspace.env(), true).unwrap();
        assert_eq!(removed, 1);
        assert!(discover_git_cache_entries_in(workspace.env())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn registry_index_search_and_info_use_local_file_without_network() {
        let (_repo_tmp, repo, _branch) = create_git_package_repo();
        let project_tmp = tempfile::tempdir().unwrap();
        let root = project_tmp.path();
        let workspace = TestWorkspace::new(root);
        let registry_path = root.join("index.toml");
        let git = normalize_git_url(repo.to_string_lossy().as_ref()).unwrap();
        write_package_registry_index(&registry_path, "@burin/acme-lib", &git, "acme-lib");
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

        let matches = search_package_registry_in(
            workspace.env(),
            Some("acme"),
            Some(registry_path.to_string_lossy().as_ref()),
        )
        .unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].name, "@burin/acme-lib");
        assert_eq!(
            matches[0].harn.as_deref(),
            Some(crate::package::current_harn_range_example().as_str())
        );
        assert_eq!(matches[0].connector_contract.as_deref(), Some("v1"));
        assert_eq!(matches[0].exports, vec!["lib"]);

        let info = package_registry_info_in(
            workspace.env(),
            "@burin/acme-lib@1.0.0",
            Some(registry_path.to_string_lossy().as_ref()),
        )
        .unwrap();
        assert_eq!(info.package.license.as_deref(), Some("MIT OR Apache-2.0"));
        assert_eq!(
            info.selected_version
                .as_ref()
                .map(|version| version.git.as_str()),
            Some(git.as_str())
        );
    }

    #[test]
    fn add_registry_dependency_preserves_provenance_in_manifest_and_lock() {
        let (_repo_tmp, repo, _branch) = create_git_package_repo();
        let project_tmp = tempfile::tempdir().unwrap();
        let root = project_tmp.path();
        let registry_path = root.join("index.toml");
        let workspace =
            TestWorkspace::new(root).with_registry_source(registry_path.display().to_string());
        let git = normalize_git_url(repo.to_string_lossy().as_ref()).unwrap();
        write_package_registry_index(&registry_path, "@burin/acme-lib", &git, "acme-lib");
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

        let manifest = fs::read_to_string(root.join(MANIFEST)).unwrap();
        assert!(
            manifest.contains(&format!("git = \"{git}\"")),
            "registry install must record the resolved git URL: {manifest}"
        );
        assert!(
            manifest.contains("tag = \"v1.0.0\""),
            "registry install must pin the resolved tag: {manifest}"
        );
        assert!(
            manifest.contains("registry_name = \"@burin/acme-lib\""),
            "registry install must preserve the registry-side package name: {manifest}"
        );
        assert!(
            manifest.contains("registry_version = \"1.0.0\""),
            "registry install must preserve the requested registry version: {manifest}"
        );
        let lock = LockFile::load(&root.join(LOCK_FILE)).unwrap().unwrap();
        let entry = lock.find("acme-lib").unwrap();
        assert_eq!(entry.source, format!("git+{git}"));
        let registry = entry
            .registry
            .as_ref()
            .expect("registry-added entry should carry registry provenance");
        assert_eq!(registry.name, "@burin/acme-lib");
        assert_eq!(registry.version, "1.0.0");
        assert!(root
            .join(PKG_DIR)
            .join("acme-lib")
            .join("lib.harn")
            .is_file());
    }

    #[test]
    fn add_registry_dependency_accepts_bare_alias_and_semver_range() {
        // Covers the literal acceptance from the free-tier package-manager
        // epic (harn#2157): `harn add notion-sdk-harn@^0.1` should resolve
        // even though the registry-side name is `@burin/notion-sdk`.
        let (_repo_tmp, repo, _branch) = create_git_package_repo();
        let project_tmp = tempfile::tempdir().unwrap();
        let root = project_tmp.path();
        let registry_path = root.join("index.toml");
        let workspace =
            TestWorkspace::new(root).with_registry_source(registry_path.display().to_string());
        let git = normalize_git_url(repo.to_string_lossy().as_ref()).unwrap();
        write_package_registry_index(&registry_path, "@burin/acme-lib", &git, "acme-lib");
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

        // Bare alias + semver range. Highest matching unyanked version wins.
        add_package_to(
            workspace.env(),
            "acme-lib@^1",
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();

        let manifest = fs::read_to_string(root.join(MANIFEST)).unwrap();
        assert!(
            manifest.contains("registry_name = \"@burin/acme-lib\""),
            "bare-alias add must record the canonical scoped registry name: {manifest}"
        );
        assert!(
            manifest.contains("registry_version = \"1.0.0\""),
            "semver range must resolve to the highest matching exact version: {manifest}"
        );
    }

    #[test]
    fn registry_index_rejects_invalid_names_and_duplicate_versions() {
        let content = r#"
    version = 1

    [[package]]
    name = "@bad/"
    repository = "https://github.com/acme/acme-lib"

    [[package.version]]
    version = "1.0.0"
    git = "https://github.com/acme/acme-lib"
    rev = "v1.0.0"
    "#;
        let error = parse_package_registry_index("fixture", content).unwrap_err();
        assert!(error.to_string().contains("invalid package name"));

        let content = r#"
    version = 1

    [[package]]
    name = "@burin/acme-lib"
    repository = "https://github.com/acme/acme-lib"

    [[package.version]]
    version = "1.0.0"
    git = "https://github.com/acme/acme-lib"
    rev = "v1.0.0"

    [[package.version]]
    version = "1.0.0"
    git = "https://github.com/acme/acme-lib"
    rev = "v1.0.0"
    "#;
        let error = parse_package_registry_index("fixture", content).unwrap_err();
        assert!(error.to_string().contains("more than once"));
    }
}
