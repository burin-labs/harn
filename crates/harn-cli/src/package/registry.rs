use super::*;

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
    rev: Option<String>,
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
    manifest
        .dependencies
        .values()
        .any(|dependency| dependency.git_url().is_some())
}

pub(crate) fn ensure_git_available() -> Result<(), String> {
    process::Command::new("git")
        .arg("--version")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .map(|_| ())
        .map_err(|_| "git is required for git dependencies but was not found in PATH".to_string())
}

pub(crate) fn cache_root() -> Result<PathBuf, String> {
    if let Ok(value) = std::env::var(HARN_CACHE_DIR_ENV) {
        if !value.trim().is_empty() {
            return Ok(PathBuf::from(value));
        }
    }

    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "HOME is not set and HARN_CACHE_DIR was not provided".to_string())?;
    if cfg!(target_os = "macos") {
        return Ok(home.join("Library/Caches/harn"));
    }
    if let Some(xdg) = std::env::var_os("XDG_CACHE_HOME") {
        return Ok(PathBuf::from(xdg).join("harn"));
    }
    Ok(home.join(".cache/harn"))
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

pub(crate) fn git_cache_dir(source: &str, commit: &str) -> Result<PathBuf, String> {
    Ok(cache_root()?
        .join("git")
        .join(sha256_hex(source))
        .join(commit))
}

pub(crate) fn git_cache_lock_path(source: &str, commit: &str) -> Result<PathBuf, String> {
    Ok(cache_root()?
        .join("locks")
        .join(format!("{}-{commit}.lock", sha256_hex(source))))
}

pub(crate) fn acquire_git_cache_lock(source: &str, commit: &str) -> Result<File, String> {
    let path = git_cache_lock_path(source, commit)?;
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

pub(crate) fn read_cached_content_hash(dir: &Path) -> Result<Option<String>, String> {
    let path = dir.join(CONTENT_HASH_FILE);
    match fs::read_to_string(&path) {
        Ok(value) => Ok(Some(value.trim().to_string())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("failed to read {}: {error}", path.display())),
    }
}

pub(crate) fn write_cached_content_hash(dir: &Path, hash: &str) -> Result<(), String> {
    fs::write(dir.join(CONTENT_HASH_FILE), format!("{hash}\n")).map_err(|error| {
        format!(
            "failed to write {}: {error}",
            dir.join(CONTENT_HASH_FILE).display()
        )
    })
}

pub(crate) fn read_cache_metadata(dir: &Path) -> Result<Option<PackageCacheMetadata>, String> {
    let path = dir.join(CACHE_METADATA_FILE);
    let content = match fs::read_to_string(&path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("failed to read {}: {error}", path.display())),
    };
    let metadata = toml::from_str::<PackageCacheMetadata>(&content)
        .map_err(|error| format!("failed to parse {}: {error}", path.display()))?;
    if metadata.version != CACHE_METADATA_VERSION {
        return Err(format!(
            "unsupported {} version {} (expected {})",
            path.display(),
            metadata.version,
            CACHE_METADATA_VERSION
        ));
    }
    Ok(Some(metadata))
}

pub(crate) fn write_cache_metadata(
    dir: &Path,
    source: &str,
    commit: &str,
    content_hash: &str,
) -> Result<(), String> {
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
    fs::write(dir.join(CACHE_METADATA_FILE), body).map_err(|error| {
        format!(
            "failed to write {}: {error}",
            dir.join(CACHE_METADATA_FILE).display()
        )
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
) -> Result<(), String> {
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

pub(crate) fn compute_content_hash(dir: &Path) -> Result<String, String> {
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

pub(crate) fn verify_content_hash_or_compute(dir: &Path, expected: &str) -> Result<(), String> {
    let actual = compute_content_hash(dir)?;
    if actual != expected {
        return Err(format!(
            "content hash mismatch for {}: expected {}, got {}",
            dir.display(),
            expected,
            actual
        ));
    }
    if read_cached_content_hash(dir)?.as_deref() != Some(expected) {
        write_cached_content_hash(dir, expected)?;
    }
    Ok(())
}

pub(crate) fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), String> {
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

pub(crate) fn remove_materialized_package(packages_dir: &Path, alias: &str) -> Result<(), String> {
    let dir = packages_dir.join(alias);
    match fs::symlink_metadata(&dir) {
        Ok(metadata) if metadata.file_type().is_symlink() || metadata.is_file() => {
            fs::remove_file(&dir)
                .map_err(|error| format!("failed to remove {}: {error}", dir.display()))?;
        }
        Ok(metadata) if metadata.is_dir() => {
            fs::remove_dir_all(&dir)
                .map_err(|error| format!("failed to remove {}: {error}", dir.display()))?;
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("failed to stat {}: {error}", dir.display())),
    }
    let file = packages_dir.join(format!("{alias}.harn"));
    match fs::symlink_metadata(&file) {
        Ok(metadata) if metadata.file_type().is_symlink() || metadata.is_file() => {
            fs::remove_file(&file)
                .map_err(|error| format!("failed to remove {}: {error}", file.display()))?;
        }
        Ok(metadata) if metadata.is_dir() => {
            fs::remove_dir_all(&file)
                .map_err(|error| format!("failed to remove {}: {error}", file.display()))?;
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("failed to stat {}: {error}", file.display())),
    }
    Ok(())
}

#[cfg(unix)]
pub(crate) fn symlink_path_dependency(source: &Path, dest: &Path) -> Result<(), String> {
    std::os::unix::fs::symlink(source, dest).map_err(|error| {
        format!(
            "failed to symlink {} to {}: {error}",
            source.display(),
            dest.display()
        )
    })
}

#[cfg(windows)]
pub(crate) fn symlink_path_dependency(source: &Path, dest: &Path) -> Result<(), String> {
    if source.is_dir() {
        std::os::windows::fs::symlink_dir(source, dest)
    } else {
        std::os::windows::fs::symlink_file(source, dest)
    }
    .map_err(|error| {
        format!(
            "failed to symlink {} to {}: {error}",
            source.display(),
            dest.display()
        )
    })
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn symlink_path_dependency(_source: &Path, _dest: &Path) -> Result<(), String> {
    Err("symlinks are not supported on this platform".to_string())
}

pub(crate) fn materialize_path_dependency(
    source: &Path,
    dest_root: &Path,
    alias: &str,
) -> Result<(), String> {
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
) -> Result<PathBuf, String> {
    let source = {
        let candidate = PathBuf::from(raw);
        if candidate.is_absolute() {
            candidate
        } else {
            manifest_dir.join(candidate)
        }
    };
    if source.exists() {
        return source
            .canonicalize()
            .map_err(|error| format!("failed to canonicalize {}: {error}", source.display()));
    }
    if source.extension().is_none() {
        let with_ext = source.with_extension("harn");
        if with_ext.exists() {
            return with_ext.canonicalize().map_err(|error| {
                format!("failed to canonicalize {}: {error}", with_ext.display())
            });
        }
    }
    Err(format!("package source not found: {}", source.display()))
}

pub(crate) fn path_source_uri(path: &Path) -> Result<String, String> {
    let url = Url::from_file_path(path)
        .map_err(|_| format!("failed to convert {} to file:// URL", path.display()))?;
    Ok(format!("path+{}", url))
}

pub(crate) fn path_from_source_uri(source: &str) -> Result<PathBuf, String> {
    let raw = source
        .strip_prefix("path+")
        .ok_or_else(|| format!("invalid path source: {source}"))?;
    if let Ok(url) = Url::parse(raw) {
        return url
            .to_file_path()
            .map_err(|_| format!("invalid file:// path source: {source}"));
    }
    Ok(PathBuf::from(raw))
}

pub(crate) fn registry_file_url_or_path(raw: &str) -> Result<Option<PathBuf>, String> {
    if let Ok(url) = Url::parse(raw) {
        if url.scheme() == "file" {
            return url
                .to_file_path()
                .map(Some)
                .map_err(|_| format!("invalid file:// registry URL: {raw}"));
        }
        return Ok(None);
    }
    Ok(Some(PathBuf::from(raw)))
}

pub(crate) fn read_registry_source(source: &str) -> Result<String, String> {
    if let Some(path) = registry_file_url_or_path(source)? {
        return fs::read_to_string(&path).map_err(|error| {
            format!(
                "failed to read package registry {}: {error}",
                path.display()
            )
        });
    }

    let url = Url::parse(source)
        .map_err(|error| format!("invalid package registry URL {source:?}: {error}"))?;
    match url.scheme() {
        "http" | "https" => {}
        other => return Err(format!("unsupported package registry URL scheme: {other}")),
    }
    let response = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|error| format!("failed to build package registry client: {error}"))?
        .get(url)
        .send()
        .map_err(|error| format!("failed to fetch package registry {source}: {error}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("GET {source} returned HTTP {status}"));
    }
    response
        .text()
        .map_err(|error| format!("failed to read package registry response: {error}"))
}

pub(crate) fn resolve_configured_registry_source(explicit: Option<&str>) -> Result<String, String> {
    if let Some(explicit) = explicit.map(str::trim).filter(|value| !value.is_empty()) {
        return Ok(explicit.to_string());
    }
    if let Ok(value) = std::env::var(HARN_PACKAGE_REGISTRY_ENV) {
        let value = value.trim();
        if !value.is_empty() {
            return Ok(value.to_string());
        }
    }

    let cwd = std::env::current_dir().map_err(|error| format!("failed to read cwd: {error}"))?;
    if let Some((manifest, manifest_dir)) = find_nearest_manifest(&cwd) {
        if let Some(raw) = manifest
            .registry
            .url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            if Url::parse(raw).is_ok() || PathBuf::from(raw).is_absolute() {
                return Ok(raw.to_string());
            }
            return Ok(manifest_dir.join(raw).display().to_string());
        }
    }

    Ok(DEFAULT_PACKAGE_REGISTRY_URL.to_string())
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
) -> Result<PackageRegistryIndex, String> {
    let mut index = toml::from_str::<PackageRegistryIndex>(content)
        .map_err(|error| format!("failed to parse package registry {source}: {error}"))?;
    if index.version != REGISTRY_INDEX_VERSION {
        return Err(format!(
            "unsupported package registry {source} version {} (expected {})",
            index.version, REGISTRY_INDEX_VERSION
        ));
    }
    validate_package_registry_index(source, &mut index)?;
    Ok(index)
}

pub(crate) fn validate_package_registry_index(
    source: &str,
    index: &mut PackageRegistryIndex,
) -> Result<(), String> {
    let mut names = HashSet::new();
    for package in &mut index.packages {
        if !is_valid_registry_package_name(&package.name) {
            return Err(format!(
                "package registry {source} has invalid package name '{}'",
                package.name
            ));
        }
        if !names.insert(package.name.clone()) {
            return Err(format!(
                "package registry {source} declares '{}' more than once",
                package.name
            ));
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
                ));
            }
            if !versions.insert(version.version.clone()) {
                return Err(format!(
                    "package registry {source} declares '{}@{}' more than once",
                    package.name, version.version
                ));
            }
            if version.rev.is_none() && version.branch.is_none() {
                return Err(format!(
                    "package registry {source} entry '{}@{}' must specify rev or branch",
                    package.name, version.version
                ));
            }
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

pub(crate) fn load_package_registry(
    explicit: Option<&str>,
) -> Result<(String, PackageRegistryIndex), String> {
    let source = resolve_configured_registry_source(explicit)?;
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
        .rev()
        .find(|version| !version.yanked)
}

pub(crate) fn find_registry_package_version(
    index: &PackageRegistryIndex,
    name: &str,
    version: Option<&str>,
) -> Result<RegistryPackageInfo, String> {
    let package = index
        .packages
        .iter()
        .find(|package| package.name == name)
        .ok_or_else(|| format!("package registry does not contain {name}"))?;
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

pub(crate) fn search_package_registry_impl(
    query: Option<&str>,
    registry: Option<&str>,
) -> Result<Vec<RegistryPackage>, String> {
    let (_, index) = load_package_registry(registry)?;
    Ok(index
        .packages
        .into_iter()
        .filter(|package| registry_package_matches(package, query.unwrap_or("")))
        .collect())
}

pub(crate) fn package_registry_info_impl(
    spec: &str,
    registry: Option<&str>,
) -> Result<RegistryPackageInfo, String> {
    let Some((name, version)) = parse_registry_package_spec(spec) else {
        return Err(format!(
            "invalid registry package name '{spec}'; use names like @burin/notion-sdk or acme-lib"
        ));
    };
    let (_, index) = load_package_registry(registry)?;
    find_registry_package_version(&index, name, version)
}

pub(crate) fn registry_dependency_from_spec(
    spec: &str,
    alias: Option<&str>,
    registry: Option<&str>,
) -> Result<(String, Dependency), String> {
    let Some((name, Some(version))) = parse_registry_package_spec(spec) else {
        return Err(format!(
            "registry dependency '{spec}' must include a version, for example {spec}@1.2.3"
        ));
    };
    let info = package_registry_info_impl(&format!("{name}@{version}"), registry)?;
    let selected = info
        .selected_version
        .ok_or_else(|| format!("package registry does not contain {name}@{version}"))?;
    if selected.yanked {
        return Err(format!(
            "{name}@{version} is yanked in the package registry"
        ));
    }
    let git = normalize_git_url(&selected.git)?;
    let package_name = selected
        .package
        .clone()
        .map(Ok)
        .unwrap_or_else(|| derive_repo_name_from_source(&git))?;
    let alias = alias.unwrap_or(package_name.as_str()).to_string();
    Ok((
        alias.clone(),
        Dependency::Table(DepTable {
            git: Some(git),
            tag: None,
            rev: selected.rev,
            branch: selected.branch,
            path: None,
            package: (alias != package_name).then_some(package_name),
        }),
    ))
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

pub(crate) fn normalize_git_url(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("git URL cannot be empty".to_string());
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

pub(crate) fn derive_repo_name_from_source(source: &str) -> Result<String, String> {
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

pub(crate) fn derive_package_alias_from_path(path: &Path) -> Result<String, String> {
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
        .ok_or_else(|| format!("failed to derive package alias from {}", path.display()))
}

pub(crate) fn is_full_git_sha(value: &str) -> bool {
    value.len() == 40 && value.as_bytes().iter().all(|byte| byte.is_ascii_hexdigit())
}

pub(crate) fn git_output<I, S>(args: I, cwd: Option<&Path>) -> Result<std::process::Output, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = process::Command::new("git");
    command.args(args);
    if let Some(dir) = cwd {
        command.current_dir(dir);
    }
    command
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .map_err(|error| format!("failed to run git: {error}"))
}

pub(crate) fn resolve_git_commit(
    url: &str,
    rev: Option<&str>,
    branch: Option<&str>,
) -> Result<String, String> {
    let requested = branch.or(rev).unwrap_or("HEAD");
    if branch.is_none() && is_full_git_sha(requested) {
        return Ok(requested.to_string());
    }

    let refs = if let Some(branch) = branch {
        vec![format!("refs/heads/{branch}")]
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
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let commit = stdout
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .find(|value| is_full_git_sha(value))
        .ok_or_else(|| format!("could not resolve {requested} from {url}"))?;
    Ok(commit.to_string())
}

pub(crate) fn clone_git_commit_to(url: &str, commit: &str, dest: &Path) -> Result<(), String> {
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
        ));
    }

    let remote = git_output(["remote", "add", "origin", url], Some(dest))?;
    if !remote.status.success() {
        return Err(format!(
            "failed to add git remote {url}: {}",
            String::from_utf8_lossy(&remote.stderr).trim()
        ));
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
            ));
        }
        let checkout = git_output(["checkout", commit], Some(&fallback_dir))?;
        if !checkout.status.success() {
            return Err(format!(
                "failed to checkout {commit} in {}: {}",
                fallback_dir.display(),
                String::from_utf8_lossy(&checkout.stderr).trim()
            ));
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
            ));
        }
    }

    let git_dir = dest.join(".git");
    if git_dir.exists() {
        fs::remove_dir_all(&git_dir)
            .map_err(|error| format!("failed to remove {}: {error}", git_dir.display()))?;
    }
    Ok(())
}

pub(crate) fn unique_temp_dir(base: &Path, label: &str) -> Result<PathBuf, String> {
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
    ))
}

pub(crate) fn ensure_git_cache_populated(
    url: &str,
    source: &str,
    commit: &str,
    expected_hash: Option<&str>,
    refetch: bool,
    offline: bool,
) -> Result<String, String> {
    let cache_dir = git_cache_dir(source, commit)?;
    let _lock = acquire_git_cache_lock(source, commit)?;
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
        ));
    }

    let parent = cache_dir
        .parent()
        .ok_or_else(|| format!("invalid cache path {}", cache_dir.display()))?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    let temp_dir = unique_temp_dir(parent, "tmp")?;
    let populated = (|| -> Result<String, String> {
        clone_git_commit_to(url, commit, &temp_dir)?;
        let hash = compute_content_hash(&temp_dir)?;
        if let Some(expected) = expected_hash {
            if hash != expected {
                return Err(format!(
                    "content hash mismatch for {} at {}: expected {}, got {}",
                    source, commit, expected, hash
                ));
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

pub(crate) fn git_cache_root() -> Result<PathBuf, String> {
    Ok(cache_root()?.join("git"))
}

pub(crate) fn discover_git_cache_entries() -> Result<Vec<PackageCacheEntry>, String> {
    let root = git_cache_root()?;
    let mut entries = Vec::new();
    let source_dirs = match fs::read_dir(&root) {
        Ok(source_dirs) => source_dirs,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(entries),
        Err(error) => return Err(format!("failed to read {}: {error}", root.display())),
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

pub(crate) fn locked_git_cache_paths(lock: &LockFile) -> Result<HashSet<PathBuf>, String> {
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
        keep.insert(git_cache_dir(&entry.source, commit)?);
    }
    Ok(keep)
}

pub(crate) fn verify_lock_entry_cache(entry: &LockEntry) -> Result<bool, String> {
    validate_package_alias(&entry.name)?;
    if !entry.source.starts_with("git+") {
        if entry.source.starts_with("path+") {
            let path = path_from_source_uri(&entry.source)?;
            if !path.exists() {
                return Err(format!(
                    "path dependency {} source is missing: {}",
                    entry.name,
                    path.display()
                ));
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
    let cache_dir = git_cache_dir(&entry.source, commit)?;
    if !cache_dir.is_dir() {
        return Err(format!(
            "package cache entry for {} is missing: {}",
            entry.name,
            cache_dir.display()
        ));
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
            ));
        }
        None => write_cache_metadata(&cache_dir, &entry.source, commit, expected_hash)?,
    }
    Ok(true)
}

pub(crate) fn verify_materialized_lock_entry(
    ctx: &ManifestContext,
    entry: &LockEntry,
) -> Result<bool, String> {
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
            ));
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
        ));
    }
    verify_content_hash_or_compute(&dest_dir, expected_hash)?;
    Ok(true)
}

pub(crate) fn verify_package_cache_impl(materialized: bool) -> Result<usize, String> {
    let ctx = load_current_manifest_context()?;
    let lock = LockFile::load(&ctx.lock_path())?
        .ok_or_else(|| format!("{} is missing", ctx.lock_path().display()))?;
    validate_lock_matches_manifest(&ctx, &lock)?;
    let mut verified = 0usize;
    for entry in &lock.packages {
        if verify_lock_entry_cache(entry)? {
            verified += 1;
        }
        if materialized && verify_materialized_lock_entry(&ctx, entry)? {
            verified += 1;
        }
    }
    Ok(verified)
}

pub(crate) fn clean_package_cache_impl(all: bool) -> Result<usize, String> {
    let entries = discover_git_cache_entries()?;
    if entries.is_empty() {
        return Ok(0);
    }
    if all {
        let root = cache_root()?;
        for child in ["git", "locks"] {
            let path = root.join(child);
            if path.exists() {
                fs::remove_dir_all(&path)
                    .map_err(|error| format!("failed to remove {}: {error}", path.display()))?;
            }
        }
        return Ok(entries.len());
    }

    let ctx = load_current_manifest_context()?;
    let lock = LockFile::load(&ctx.lock_path())?.ok_or_else(|| {
        format!(
            "{} is missing; pass --all to clean every cache entry",
            LOCK_FILE
        )
    })?;
    validate_lock_matches_manifest(&ctx, &lock)?;
    let keep = locked_git_cache_paths(&lock)?;
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
    let result = (|| -> Result<(PathBuf, Vec<PackageCacheEntry>), String> {
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

    #[test]
    fn package_cache_verify_detects_tampering_even_with_stale_marker() {
        let (_repo_tmp, repo, _branch) = create_git_package_repo();
        let project_tmp = tempfile::tempdir().unwrap();
        let root = project_tmp.path();
        let cache_dir = root.join(".cache");
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

        with_test_env(root, &cache_dir, || {
            install_packages_impl(false, None, false).unwrap();
            let lock = LockFile::load(&root.join(LOCK_FILE)).unwrap().unwrap();
            let entry = lock.find("acme-lib").unwrap();
            let cache_dir = git_cache_dir(&entry.source, entry.commit.as_deref().unwrap()).unwrap();
            fs::write(
                cache_dir.join("lib.harn"),
                "pub fn value() { return \"pwned\" }\n",
            )
            .unwrap();

            let error = verify_package_cache_impl(false).unwrap_err();
            assert!(error.contains("content hash mismatch"));
        });
    }

    #[test]
    fn package_cache_clean_all_removes_cached_git_entries() {
        let (_repo_tmp, repo, _branch) = create_git_package_repo();
        let project_tmp = tempfile::tempdir().unwrap();
        let root = project_tmp.path();
        let cache_dir = root.join(".cache");
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

        with_test_env(root, &cache_dir, || {
            install_packages_impl(false, None, false).unwrap();
            assert_eq!(discover_git_cache_entries().unwrap().len(), 1);

            let removed = clean_package_cache_impl(true).unwrap();
            assert_eq!(removed, 1);
            assert!(discover_git_cache_entries().unwrap().is_empty());
        });
    }

    #[test]
    fn registry_index_search_and_info_use_local_file_without_network() {
        let (_repo_tmp, repo, _branch) = create_git_package_repo();
        let project_tmp = tempfile::tempdir().unwrap();
        let root = project_tmp.path();
        let cache_dir = root.join(".cache");
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

        with_test_env(root, &cache_dir, || {
            let matches = search_package_registry_impl(Some("acme"), Some("index.toml")).unwrap();
            assert_eq!(matches.len(), 1);
            assert_eq!(matches[0].name, "@burin/acme-lib");
            assert_eq!(matches[0].harn.as_deref(), Some(">=0.7,<0.8"));
            assert_eq!(matches[0].connector_contract.as_deref(), Some("v1"));
            assert_eq!(matches[0].exports, vec!["lib"]);

            let info =
                package_registry_info_impl("@burin/acme-lib@1.0.0", Some("index.toml")).unwrap();
            assert_eq!(info.package.license.as_deref(), Some("MIT OR Apache-2.0"));
            assert_eq!(
                info.selected_version
                    .as_ref()
                    .map(|version| version.git.as_str()),
                Some(git.as_str())
            );
        });
    }

    #[test]
    fn add_registry_dependency_writes_existing_git_dependency_shape() {
        let (_repo_tmp, repo, _branch) = create_git_package_repo();
        let project_tmp = tempfile::tempdir().unwrap();
        let root = project_tmp.path();
        let cache_dir = root.join(".cache");
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

        with_test_env(root, &cache_dir, || {
            std::env::set_var(HARN_PACKAGE_REGISTRY_ENV, "index.toml");
            add_package("@burin/acme-lib@1.0.0", None, None, None, None, None, None);

            let manifest = fs::read_to_string(root.join(MANIFEST)).unwrap();
            assert!(
                manifest.contains(&format!(
                    "acme-lib = {{ git = \"{git}\", rev = \"v1.0.0\" }}"
                )),
                "registry install should write the same dependency line as a direct git add: {manifest}"
            );
            let lock = LockFile::load(&root.join(LOCK_FILE)).unwrap().unwrap();
            let entry = lock.find("acme-lib").unwrap();
            assert_eq!(entry.source, format!("git+{git}"));
            assert!(root
                .join(PKG_DIR)
                .join("acme-lib")
                .join("lib.harn")
                .is_file());
        });
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
        assert!(error.contains("invalid package name"));

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
        assert!(error.contains("more than once"));
    }
}
