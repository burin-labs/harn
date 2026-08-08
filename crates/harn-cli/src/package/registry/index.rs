//! The registry index document: its data model, parsing, validation, and the
//! semver parsing/normalization the version fields are held to.

use semver::{Version, VersionReq};

use crate::package::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PackageRegistryIndex {
    pub(super) version: u32,
    #[serde(default, rename = "package")]
    pub(super) packages: Vec<RegistryPackage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RegistryPackage {
    pub(super) name: String,
    #[serde(default)]
    pub(super) description: Option<String>,
    pub(super) repository: String,
    #[serde(default)]
    pub(super) license: Option<String>,
    #[serde(default)]
    pub(super) harn: Option<String>,
    #[serde(default)]
    pub(super) exports: Vec<String>,
    #[serde(default)]
    pub(super) rule_pack: Option<RegistryRulePackInfo>,
    #[serde(default)]
    pub(super) connector_contract: Option<String>,
    #[serde(default)]
    pub(super) docs_url: Option<String>,
    #[serde(default)]
    pub(super) checksum: Option<String>,
    #[serde(default)]
    pub(super) provenance: Option<String>,
    #[serde(default, rename = "version")]
    pub(super) versions: Vec<RegistryPackageVersion>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RegistryRulePackInfo {
    #[serde(default)]
    pub(crate) rule_count: usize,
    #[serde(default)]
    pub(crate) languages: Vec<String>,
    #[serde(default)]
    pub(crate) safety_summary: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RegistryPackageVersion {
    pub(super) version: String,
    #[serde(default)]
    pub(super) git: Option<String>,
    #[serde(default)]
    pub(super) archive: Option<String>,
    #[serde(default)]
    pub(super) tag: Option<String>,
    #[serde(default)]
    pub(super) rev: Option<String>,
    #[serde(default)]
    pub(super) package: Option<String>,
    #[serde(default)]
    pub(super) checksum: Option<String>,
    #[serde(default)]
    pub(super) provenance: Option<String>,
    #[serde(default)]
    pub(super) yanked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct RegistryPackageInfo {
    pub(super) package: RegistryPackage,
    pub(super) selected_version: Option<RegistryPackageVersion>,
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
    let mut content_identities = HashSet::new();
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
        let package_repository = normalize_git_url(&package.repository)?;
        let package_provenance = package.provenance.as_deref().ok_or_else(|| {
            format!(
                "package registry {source} entry '{}' must specify provenance",
                package.name
            )
        })?;
        validate_provenance_url(package_provenance, &package_repository).map_err(|error| {
            format!(
                "package registry {source} has invalid provenance for '{}': {error}",
                package.name
            )
        })?;
        if let Some(rule_pack) = package.rule_pack.as_mut() {
            normalize_rule_pack_info(rule_pack);
        }
        if package.versions.is_empty() {
            return Err(format!(
                "package registry {source} entry '{}' must publish at least one immutable version",
                package.name
            )
            .into());
        }
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
            parse_registry_semver(&version.version).map_err(|error| {
                format!(
                    "package registry {source} has invalid semver for '{}@{}': {error}",
                    package.name, version.version
                )
            })?;
            let provenance = version.provenance.as_deref().ok_or_else(|| {
                format!(
                    "package registry {source} entry '{}@{}' must specify provenance",
                    package.name, version.version
                )
            })?;
            validate_provenance_url(provenance, &package_repository).map_err(|error| {
                format!(
                    "package registry {source} has invalid provenance for '{}@{}': {error}",
                    package.name, version.version
                )
            })?;
            match (version.git.as_deref(), version.archive.as_deref()) {
                (Some(git), None) => {
                    let normalized_git = normalize_git_url(git).map_err(|error| {
                        format!(
                            "package registry {source} has invalid git source for '{}@{}': {error}",
                            package.name, version.version
                        )
                    })?;
                    if normalized_git != package_repository {
                        return Err(format!(
                            "package registry {source} entry '{}@{}' git source must match its package repository",
                            package.name, version.version
                        )
                        .into());
                    }
                    let tag = version.tag.as_deref().ok_or_else(|| {
                        format!(
                            "package registry {source} entry '{}@{}' must specify a published tag",
                            package.name, version.version
                        )
                    })?;
                    if tag.trim().is_empty() {
                        return Err(format!(
                            "package registry {source} entry '{}@{}' has an empty tag",
                            package.name, version.version
                        )
                        .into());
                    }
                    let rev = version.rev.as_deref().ok_or_else(|| {
                        format!(
                            "package registry {source} entry '{}@{}' must specify the tag's resolved commit SHA in rev",
                            package.name, version.version
                        )
                    })?;
                    if !is_full_git_commit_sha(rev) {
                        return Err(format!(
                            "package registry {source} entry '{}@{}' rev must be a full 40- or 64-character Git commit SHA",
                            package.name, version.version
                        )
                        .into());
                    }
                    let identity = format!(
                        "git:{normalized_git}:{rev}:{}",
                        version.package.as_deref().unwrap_or("")
                    );
                    if !content_identities.insert(identity) {
                        return Err(format!(
                            "package registry {source} reuses immutable content identity for '{}@{}'",
                            package.name, version.version
                        )
                        .into());
                    }
                }
                (None, Some(archive)) => {
                    if version.tag.is_some() || version.rev.is_some() {
                        return Err(format!(
                            "package registry {source} entry '{}@{}' must not combine archive with tag, rev, or branch",
                            package.name, version.version
                        )
                        .into());
                    }
                    normalize_archive_url(archive).map_err(|error| {
                        format!(
                            "package registry {source} has invalid archive source for '{}@{}': {error}",
                            package.name, version.version
                        )
                    })?;
                    let checksum = version.checksum.as_deref().ok_or_else(|| {
                        format!(
                            "package registry {source} entry '{}@{}' must specify checksum for archive source",
                            package.name, version.version
                        )
                    })?;
                    archive_cache_key(checksum).map_err(|error| {
                        format!(
                            "package registry {source} has invalid archive checksum for '{}@{}': {error}",
                            package.name, version.version
                        )
                    })?;
                    let identity = format!(
                        "archive:{archive}:{checksum}:{}",
                        version.package.as_deref().unwrap_or("")
                    );
                    if !content_identities.insert(identity) {
                        return Err(format!(
                            "package registry {source} reuses immutable content identity for '{}@{}'",
                            package.name, version.version
                        )
                        .into());
                    }
                }
                (Some(_), Some(_)) => {
                    return Err(format!(
                        "package registry {source} entry '{}@{}' must specify only one of git or archive",
                        package.name, version.version
                    )
                    .into());
                }
                (None, None) => {
                    return Err(format!(
                        "package registry {source} entry '{}@{}' must specify git or archive",
                        package.name, version.version
                    )
                    .into());
                }
            }
        }
    }
    index
        .packages
        .sort_by(|left, right| left.name.cmp(&right.name));
    Ok(())
}

fn normalize_rule_pack_info(rule_pack: &mut RegistryRulePackInfo) {
    rule_pack
        .languages
        .retain(|language| !language.trim().is_empty());
    rule_pack.languages.sort();
    rule_pack.languages.dedup();
    rule_pack
        .safety_summary
        .retain(|entry| !entry.trim().is_empty());
    rule_pack.safety_summary.sort();
    rule_pack.safety_summary.dedup();
}

pub(crate) fn is_full_git_commit_sha(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn validate_provenance_url(
    provenance: &str,
    normalized_repository: &str,
) -> Result<(), PackageError> {
    let provenance =
        Url::parse(provenance).map_err(|error| format!("must be an absolute URL: {error}"))?;
    let repository = Url::parse(normalized_repository)
        .map_err(|error| format!("repository URL is invalid: {error}"))?;
    let repository_path = repository
        .path()
        .trim_end_matches(".git")
        .trim_end_matches('/');
    let provenance_path = provenance.path().trim_end_matches('/');
    if provenance.scheme() != repository.scheme()
        || provenance.host_str() != repository.host_str()
        || !(provenance_path == repository_path
            || provenance_path.starts_with(&format!("{repository_path}/")))
    {
        return Err(
            "must identify the declared package repository or an artifact beneath it".into(),
        );
    }
    Ok(())
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
        || package.rule_pack.as_ref().is_some_and(|rule_pack| {
            rule_pack
                .languages
                .iter()
                .chain(rule_pack.safety_summary.iter())
                .any(|value| value.to_ascii_lowercase().contains(&query))
        })
}

pub(crate) fn latest_registry_version(
    package: &RegistryPackage,
) -> Option<&RegistryPackageVersion> {
    let parsed: Vec<(Version, &RegistryPackageVersion)> = package
        .versions
        .iter()
        .filter(|version| !version.yanked)
        .filter_map(|version| {
            parse_registry_semver(&version.version)
                .ok()
                .map(|semver| (semver, version))
        })
        .collect();
    // Match cargo/npm: a bare `harn add foo` (no constraint) resolves the
    // highest *stable* release. Prereleases are only considered when the
    // package has published nothing stable yet, so an `x.y.z-rc.1` tag never
    // shadows a real release.
    parsed
        .iter()
        .filter(|(semver, _)| semver.pre.is_empty())
        .max_by(|(left, _), (right, _)| left.cmp(right))
        .or_else(|| {
            parsed
                .iter()
                .max_by(|(left, _), (right, _)| left.cmp(right))
        })
        .map(|(_, version)| *version)
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
