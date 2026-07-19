use super::*;

use crate::format::shell_quote_path;
use base64::Engine as _;

#[derive(Debug, Clone)]
pub(crate) struct PackagePublishOptions<'a> {
    pub(crate) dry_run: bool,
    pub(crate) remote: &'a str,
    pub(crate) index_repo: &'a str,
    pub(crate) index_path: &'a Path,
    pub(crate) registry_name: Option<&'a str>,
    pub(crate) skip_index_pr: bool,
    pub(crate) registry: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub(super) struct PackagePublishPlan {
    pub(super) repo_root: PathBuf,
    pub(super) package_name: String,
    pub(super) registry_name: String,
    pub(super) version: String,
    pub(super) tag: String,
    pub(super) sha: String,
    pub(super) git: String,
    pub(super) remote: String,
    pub(super) index_repo: String,
    pub(super) index_path: PathBuf,
    pub(super) updated_index_content: String,
    pub(super) index_diff: String,
    pub(super) tag_command: String,
    pub(super) pack: PackagePackReport,
}

pub(crate) fn publish_package_impl(
    anchor: Option<&Path>,
    options: &PackagePublishOptions<'_>,
) -> Result<PackagePublishReport, PackageError> {
    publish_package_impl_with_rule_pack(anchor, options, None)
}

pub(crate) fn publish_rule_package_impl(
    anchor: Option<&Path>,
    options: &PackagePublishOptions<'_>,
) -> Result<PackagePublishReport, PackageError> {
    let ctx = load_manifest_context_for_anchor(anchor)?;
    let rule_pack = collect_rule_pack_metadata(&ctx)?.ok_or_else(|| {
        PackageError::Validation(
            "rule packs must declare `[rules] ruleDirs` in harn.toml".to_string(),
        )
    })?;
    publish_package_impl_with_rule_pack(anchor, options, Some(rule_pack))
}

pub(super) fn publish_package_impl_with_rule_pack(
    anchor: Option<&Path>,
    options: &PackagePublishOptions<'_>,
    rule_pack: Option<RegistryRulePackInfo>,
) -> Result<PackagePublishReport, PackageError> {
    let registry = options
        .registry
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| {
            format!(
                "{}/{}",
                options.index_repo.trim(),
                normalized_relative_path(options.index_path)
            )
        });
    let index_content = if options.skip_index_pr {
        String::new()
    } else {
        fetch_package_index_from_github(options.index_repo, options.index_path)?
    };
    let mut plan = prepare_publish_plan(anchor, options, index_content, &registry, rule_pack)?;
    if !options.dry_run && !options.skip_index_pr {
        ensure_github_repo_writeable(options.index_repo)?;
    }
    let index_pr_url = if options.dry_run {
        None
    } else {
        execute_publish_plan(&mut plan, options.skip_index_pr)?
    };

    Ok(PackagePublishReport {
        dry_run: options.dry_run,
        registry,
        artifact_dir: plan.pack.artifact_dir,
        files: plan.pack.files,
        tag: plan.tag,
        sha: plan.sha,
        remote: plan.remote,
        index_repo: plan.index_repo,
        index_path: normalized_relative_path(&plan.index_path),
        index_pr_url,
        tag_command: Some(plan.tag_command),
        index_diff: if options.skip_index_pr {
            None
        } else {
            Some(plan.index_diff)
        },
        check: plan.pack.check,
    })
}

pub(super) fn prepare_publish_plan(
    anchor: Option<&Path>,
    options: &PackagePublishOptions<'_>,
    index_content: String,
    registry: &str,
    rule_pack: Option<RegistryRulePackInfo>,
) -> Result<PackagePublishPlan, PackageError> {
    let pack = pack_package_impl(anchor, None, true)?;
    let ctx = load_manifest_context_for_anchor(anchor)?;
    let package_info = ctx
        .manifest
        .package
        .as_ref()
        .ok_or_else(|| PackageError::Ops("[package] metadata is required".to_string()))?;
    let package_name = pack
        .check
        .name
        .clone()
        .ok_or_else(|| PackageError::Ops("[package].name is required".to_string()))?;
    let version = pack
        .check
        .version
        .clone()
        .ok_or_else(|| PackageError::Ops("[package].version is required".to_string()))?;
    let registry_name = options
        .registry_name
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or(&package_name)
        .to_string();
    if !is_valid_registry_package_name(&registry_name) {
        return Err(PackageError::Validation(format!(
            "invalid registry package name '{registry_name}'; use names like @burin/notion-sdk or acme-lib"
        )));
    }

    let repo_root = git_output(&ctx.dir, ["rev-parse", "--show-toplevel"])?;
    let repo_root = PathBuf::from(repo_root.trim());
    ensure_git_worktree_clean(&repo_root)?;
    let sha = git_output(&repo_root, ["rev-parse", "HEAD"])?
        .trim()
        .to_string();
    let remote = options.remote.trim();
    if remote.is_empty() {
        return Err(PackageError::Ops("--remote cannot be empty".to_string()));
    }
    let remote_url = git_output(&repo_root, ["remote", "get-url", remote])?
        .trim()
        .to_string();
    let git = normalize_git_url(&remote_url)?;
    let tag = format!("v{version}");
    ensure_tag_available(&repo_root, remote, &tag)?;
    ensure_changelog_entry(&ctx.dir.join("CHANGELOG.md"), &version)?;

    let (updated_index_content, index_diff) = if options.skip_index_pr {
        (index_content, String::new())
    } else {
        let entry = render_registry_version_entry(&version, &git, &tag, &sha, &package_name)?;
        let updated = add_registry_version_entry(
            &index_content,
            package_info,
            &pack.check,
            &registry_name,
            &entry,
            &version,
            &git,
            rule_pack.as_ref(),
        )?;
        parse_package_registry_index(registry, &updated)?;
        let diff = render_unified_diff(
            &index_content,
            &updated,
            &normalized_relative_path(options.index_path),
        )?;
        (updated, diff)
    };

    Ok(PackagePublishPlan {
        repo_root: repo_root.clone(),
        package_name,
        registry_name,
        version,
        tag: tag.clone(),
        sha,
        git,
        remote: remote.to_string(),
        index_repo: options.index_repo.trim().to_string(),
        index_path: options.index_path.to_path_buf(),
        updated_index_content,
        index_diff,
        tag_command: format!(
            "git -C {} tag {tag} && git -C {} push {remote} refs/tags/{tag}",
            shell_quote_path(&repo_root),
            shell_quote_path(&repo_root)
        ),
        pack,
    })
}

pub(super) fn execute_publish_plan(
    plan: &mut PackagePublishPlan,
    skip_index_pr: bool,
) -> Result<Option<String>, PackageError> {
    run_git_checked(&plan.repo_root, ["tag", plan.tag.as_str()])?;
    run_git_checked(
        &plan.repo_root,
        [
            "push",
            plan.remote.as_str(),
            &format!("refs/tags/{}", plan.tag),
        ],
    )?;
    if skip_index_pr {
        return Ok(None);
    }
    create_index_pull_request(plan).map(Some)
}

pub(super) fn create_index_pull_request(plan: &PackagePublishPlan) -> Result<String, PackageError> {
    let temp = tempfile::tempdir()
        .map_err(|error| PackageError::Ops(format!("failed to create temp dir: {error}")))?;
    let checkout = temp.path().join("index");
    let base_branch = github_default_branch(&plan.index_repo)?;
    run_command_checked(
        Path::new("."),
        "gh",
        [
            "repo",
            "clone",
            plan.index_repo.as_str(),
            checkout.to_string_lossy().as_ref(),
            "--",
            "--depth",
            "1",
            "--branch",
            base_branch.as_str(),
        ],
    )?;
    let branch = format!(
        "harn-publish/{}-{}",
        sanitize_branch_segment(&plan.package_name),
        sanitize_branch_segment(&plan.version)
    );
    run_git_checked(&checkout, ["switch", "-c", branch.as_str()])?;
    let index_path = checkout.join(&plan.index_path);
    if let Some(parent) = index_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    }
    fs::write(&index_path, &plan.updated_index_content)
        .map_err(|error| format!("failed to write {}: {error}", index_path.display()))?;
    run_git_checked(
        &checkout,
        ["add", normalized_relative_path(&plan.index_path).as_str()],
    )?;
    run_git_checked(
        &checkout,
        [
            "commit",
            "-m",
            &format!(
                "Add {} {} to package index",
                plan.registry_name, plan.version
            ),
        ],
    )?;
    run_git_checked(&checkout, ["push", "-u", "origin", branch.as_str()])?;
    let body = format!(
        "Adds `{}` version `{}` to the Harn package index.\n\nSource tag: `{}`\nSource SHA: `{}`\nSource git: `{}`\n",
        plan.registry_name, plan.version, plan.tag, plan.sha, plan.git
    );
    let body_path = temp.path().join("pr-body.md");
    fs::write(&body_path, body)
        .map_err(|error| format!("failed to write {}: {error}", body_path.display()))?;
    let output = run_command_output(
        Path::new("."),
        "gh",
        [
            "pr",
            "create",
            "--repo",
            plan.index_repo.as_str(),
            "--base",
            base_branch.as_str(),
            "--head",
            branch.as_str(),
            "--title",
            &format!(
                "Add {} {} to package index",
                plan.registry_name, plan.version
            ),
            "--body-file",
            body_path.to_string_lossy().as_ref(),
        ],
    )?;
    Ok(output.trim().to_string())
}

pub(super) fn github_default_branch(index_repo: &str) -> Result<String, PackageError> {
    let branch = run_command_output(
        Path::new("."),
        "gh",
        [
            "repo",
            "view",
            index_repo.trim(),
            "--json",
            "defaultBranchRef",
            "--jq",
            ".defaultBranchRef.name",
        ],
    )?;
    let branch = branch.trim();
    if branch.is_empty() {
        Err(PackageError::Registry(format!(
            "failed to resolve default branch for {index_repo}"
        )))
    } else {
        Ok(branch.to_string())
    }
}

pub(super) fn fetch_package_index_from_github(
    index_repo: &str,
    index_path: &Path,
) -> Result<String, PackageError> {
    ensure_gh_available()?;
    let api_path = format!(
        "repos/{}/contents/{}",
        index_repo.trim(),
        normalized_relative_path(index_path)
    );
    let content = run_command_output(
        Path::new("."),
        "gh",
        ["api", api_path.as_str(), "--jq", ".content"],
    )?;
    let encoded = content.replace(['\n', '\r'], "");
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded.as_bytes())
        .map_err(|error| {
            PackageError::Registry(format!(
                "failed to decode package index from {index_repo}: {}: {error}",
                index_path.display()
            ))
        })?;
    String::from_utf8(bytes).map_err(|error| {
        PackageError::Registry(format!(
            "package index {} in {index_repo} is not UTF-8: {error}",
            index_path.display()
        ))
    })
}

pub(super) fn ensure_gh_available() -> Result<(), PackageError> {
    which::which("gh").map(|_| ()).map_err(|_| {
        PackageError::Registry(
            "gh is required to read or update the package index but was not found in PATH"
                .to_string(),
        )
    })
}

pub(super) fn ensure_github_repo_writeable(index_repo: &str) -> Result<(), PackageError> {
    let permission = run_command_output(
        Path::new("."),
        "gh",
        [
            "repo",
            "view",
            index_repo.trim(),
            "--json",
            "viewerPermission",
            "--jq",
            ".viewerPermission",
        ],
    )?;
    let permission = permission.trim();
    if matches!(permission, "ADMIN" | "MAINTAIN" | "WRITE") {
        Ok(())
    } else {
        Err(PackageError::Registry(format!(
            "current gh auth has {permission} permission on {index_repo}; WRITE, MAINTAIN, or ADMIN is required to open the package-index PR"
        )))
    }
}

pub(super) fn ensure_git_worktree_clean(repo: &Path) -> Result<(), PackageError> {
    let status = git_output(repo, ["status", "--porcelain"])?;
    if status.trim().is_empty() {
        Ok(())
    } else {
        Err(PackageError::Ops(format!(
            "working tree must be clean before publishing:\n{}",
            status.trim_end()
        )))
    }
}

pub(super) fn ensure_tag_available(
    repo: &Path,
    remote: &str,
    tag: &str,
) -> Result<(), PackageError> {
    if git_status(
        repo,
        [
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/tags/{tag}"),
        ],
    )?
    .success()
    {
        return Err(PackageError::Ops(format!(
            "git tag {tag} already exists locally"
        )));
    }
    let status = git_status(
        repo,
        [
            "ls-remote",
            "--exit-code",
            "--tags",
            remote,
            &format!("refs/tags/{tag}"),
        ],
    )?;
    if status.success() {
        return Err(PackageError::Ops(format!(
            "git tag {tag} already exists on remote {remote}"
        )));
    }
    if status.code() == Some(2) {
        return Ok(());
    }
    Err(PackageError::Ops(format!(
        "failed to check whether tag {tag} exists on remote {remote}"
    )))
}

pub(super) fn ensure_changelog_entry(path: &Path, version: &str) -> Result<(), PackageError> {
    let content = fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    if changelog_has_nonempty_entry(&content, version) {
        Ok(())
    } else {
        Err(PackageError::Validation(format!(
            "{} must contain a non-empty entry for version {version}",
            path.display()
        )))
    }
}

pub(super) fn changelog_has_nonempty_entry(content: &str, version: &str) -> bool {
    let escaped = regex::escape(version);
    let heading = Regex::new(&format!(
        r"(?m)^#{{1,6}}\s+(?:\[?v?{escaped}\]?)(?:\s|$|[-(])"
    ))
    .expect("valid changelog heading regex");
    let Some(found) = heading.find(content) else {
        return false;
    };
    let rest = &content[found.end()..];
    let entry = rest
        .lines()
        .take_while(|line| !line.trim_start().starts_with('#'))
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("<!--"))
        .collect::<Vec<_>>();
    !entry.is_empty()
}

pub(super) fn add_registry_version_entry(
    content: &str,
    package_info: &PackageInfo,
    report: &PackageCheckReport,
    registry_name: &str,
    version_entry: &str,
    version: &str,
    git: &str,
    rule_pack: Option<&RegistryRulePackInfo>,
) -> Result<String, PackageError> {
    let snapshot = parse_publish_index_snapshot(content)?;
    if let Some(package) = snapshot
        .packages
        .iter()
        .find(|package| package.name == registry_name)
    {
        if package
            .versions
            .iter()
            .any(|entry| entry.version == version)
        {
            return Err(PackageError::Registry(format!(
                "package index already contains {registry_name}@{version}"
            )));
        }
        let updated = insert_version_entry(content, registry_name, version_entry)?;
        return if let Some(rule_pack) = rule_pack {
            upsert_rule_pack_metadata(&updated, registry_name, rule_pack)
        } else {
            Ok(updated)
        };
    }

    let mut updated = content.trim_end().to_string();
    updated.push_str("\n\n");
    updated.push_str(&render_registry_package_block(
        package_info,
        report,
        registry_name,
        git,
        version_entry,
        rule_pack,
    )?);
    Ok(updated)
}

pub(super) fn insert_version_entry(
    content: &str,
    registry_name: &str,
    version_entry: &str,
) -> Result<String, PackageError> {
    let starts = package_block_offsets(content);
    for (idx, start) in starts.iter().enumerate() {
        let end = starts.get(idx + 1).copied().unwrap_or(content.len());
        let block = &content[*start..end];
        if block_has_registry_name(block, registry_name) {
            let mut updated = String::with_capacity(content.len() + version_entry.len() + 2);
            updated.push_str(content[..end].trim_end());
            updated.push_str("\n\n");
            updated.push_str(version_entry.trim_end());
            updated.push('\n');
            updated.push_str(&content[end..]);
            return Ok(updated);
        }
    }
    Err(PackageError::Registry(format!(
        "failed to locate package index block for {registry_name}"
    )))
}

pub(super) fn upsert_rule_pack_metadata(
    content: &str,
    registry_name: &str,
    rule_pack: &RegistryRulePackInfo,
) -> Result<String, PackageError> {
    let starts = package_block_offsets(content);
    for (idx, start) in starts.iter().enumerate() {
        let end = starts.get(idx + 1).copied().unwrap_or(content.len());
        let block = &content[*start..end];
        if block_has_registry_name(block, registry_name) {
            let updated_block = upsert_rule_pack_metadata_in_block(block, rule_pack)?;
            let mut updated = String::with_capacity(content.len() + updated_block.len());
            updated.push_str(&content[..*start]);
            updated.push_str(&updated_block);
            updated.push_str(&content[end..]);
            return Ok(updated);
        }
    }
    Err(PackageError::Registry(format!(
        "failed to locate package index block for {registry_name}"
    )))
}

pub(super) fn upsert_rule_pack_metadata_in_block(
    block: &str,
    rule_pack: &RegistryRulePackInfo,
) -> Result<String, PackageError> {
    let metadata = render_rule_pack_metadata(rule_pack)?;
    let line_ranges = block_line_ranges(block);
    let metadata_start = line_ranges
        .iter()
        .find(|(_, _, line)| line.trim() == "[package.rule_pack]")
        .map(|(start, _, _)| *start);

    if let Some(start) = metadata_start {
        let end = line_ranges
            .iter()
            .find(|(line_start, _, line)| *line_start > start && line.trim_start().starts_with('['))
            .map(|(line_start, _, _)| *line_start)
            .unwrap_or(block.len());
        let mut out = String::with_capacity(block.len() + metadata.len());
        out.push_str(block[..start].trim_end());
        out.push_str("\n\n");
        out.push_str(&metadata);
        out.push('\n');
        out.push_str(block[end..].trim_start_matches('\n'));
        return Ok(out);
    }

    let insert_at = line_ranges
        .iter()
        .find(|(_, _, line)| line.trim() == "[[package.version]]")
        .map(|(start, _, _)| *start)
        .unwrap_or(block.len());
    let mut out = String::with_capacity(block.len() + metadata.len() + 2);
    out.push_str(block[..insert_at].trim_end());
    out.push_str("\n\n");
    out.push_str(&metadata);
    out.push_str("\n\n");
    out.push_str(block[insert_at..].trim_start_matches('\n'));
    Ok(out)
}

pub(super) fn block_line_ranges(block: &str) -> Vec<(usize, usize, &str)> {
    let mut ranges = Vec::new();
    let mut cursor = 0;
    for line in block.split_inclusive('\n') {
        let end = cursor + line.len();
        ranges.push((cursor, end, line.trim_end_matches('\n')));
        cursor = end;
    }
    if cursor < block.len() {
        ranges.push((cursor, block.len(), &block[cursor..]));
    }
    ranges
}

pub(super) fn package_block_offsets(content: &str) -> Vec<usize> {
    let mut offsets = Vec::new();
    let mut cursor = 0;
    for line in content.split_inclusive('\n') {
        if line.trim() == "[[package]]" {
            offsets.push(cursor);
        }
        cursor += line.len();
    }
    if cursor < content.len() && content[cursor..].trim() == "[[package]]" {
        offsets.push(cursor);
    }
    offsets
}

pub(super) fn block_has_registry_name(block: &str, registry_name: &str) -> bool {
    let literal = match toml_string_literal(registry_name) {
        Ok(literal) => literal,
        Err(_) => return false,
    };
    block.lines().any(|line| {
        let line = line.trim();
        line.strip_prefix("name")
            .and_then(|rest| rest.trim_start().strip_prefix('='))
            .is_some_and(|value| value.trim() == literal)
    })
}

#[derive(Debug, Deserialize)]
pub(super) struct PublishIndexSnapshot {
    #[serde(default, rename = "package")]
    pub(super) packages: Vec<PublishIndexPackageSnapshot>,
}

#[derive(Debug, Deserialize)]
pub(super) struct PublishIndexPackageSnapshot {
    pub(super) name: String,
    #[serde(default, rename = "version")]
    pub(super) versions: Vec<PublishIndexVersionSnapshot>,
}

#[derive(Debug, Deserialize)]
pub(super) struct PublishIndexVersionSnapshot {
    pub(super) version: String,
}

pub(super) fn parse_publish_index_snapshot(
    content: &str,
) -> Result<PublishIndexSnapshot, PackageError> {
    toml::from_str(content)
        .map_err(|error| PackageError::Registry(format!("failed to parse package index: {error}")))
}

pub(super) fn render_registry_package_block(
    package_info: &PackageInfo,
    report: &PackageCheckReport,
    registry_name: &str,
    git: &str,
    version_entry: &str,
    rule_pack: Option<&RegistryRulePackInfo>,
) -> Result<String, PackageError> {
    let mut out = String::new();
    out.push_str("[[package]]\n");
    push_toml_string_field(&mut out, "name", registry_name)?;
    if let Some(description) = package_info.description.as_deref() {
        push_toml_string_field(&mut out, "description", description)?;
    }
    push_toml_string_field(
        &mut out,
        "repository",
        package_info.repository.as_deref().unwrap_or(git),
    )?;
    if let Some(license) = package_info.license.as_deref() {
        push_toml_string_field(&mut out, "license", license)?;
    }
    if let Some(harn) = package_info.harn.as_deref() {
        push_toml_string_field(&mut out, "harn", harn)?;
    }
    if !report.exports.is_empty() {
        let exports = report
            .exports
            .iter()
            .map(|export| toml_string_literal(&export.name))
            .collect::<Result<Vec<_>, _>>()?
            .join(", ");
        out.push_str(&format!("exports = [{exports}]\n"));
    }
    if let Some(docs_url) = package_info.docs_url.as_deref() {
        push_toml_string_field(&mut out, "docs_url", docs_url)?;
    }
    push_toml_string_field(&mut out, "provenance", git)?;
    out.push('\n');
    if let Some(rule_pack) = rule_pack {
        out.push_str(&render_rule_pack_metadata(rule_pack)?);
        out.push('\n');
    }
    out.push_str(version_entry.trim_end());
    out.push('\n');
    Ok(out)
}

pub(super) fn render_rule_pack_metadata(
    rule_pack: &RegistryRulePackInfo,
) -> Result<String, PackageError> {
    let languages = rule_pack
        .languages
        .iter()
        .map(|language| toml_string_literal(language))
        .collect::<Result<Vec<_>, _>>()?
        .join(", ");
    let safety = rule_pack
        .safety_summary
        .iter()
        .map(|entry| toml_string_literal(entry))
        .collect::<Result<Vec<_>, _>>()?
        .join(", ");
    Ok(format!(
        "[package.rule_pack]\nrule_count = {}\nlanguages = [{}]\nsafety_summary = [{}]\n",
        rule_pack.rule_count, languages, safety
    ))
}

pub(super) fn render_registry_version_entry(
    version: &str,
    git: &str,
    tag: &str,
    sha: &str,
    package_name: &str,
) -> Result<String, PackageError> {
    let provenance =
        github_tag_url(git, tag).unwrap_or_else(|| format!("{git}/releases/tag/{tag}"));
    let mut out = String::new();
    out.push_str("[[package.version]]\n");
    push_toml_string_field(&mut out, "version", version)?;
    push_toml_string_field(&mut out, "git", git)?;
    push_toml_string_field(&mut out, "rev", sha)?;
    push_toml_string_field(&mut out, "tag", tag)?;
    push_toml_string_field(&mut out, "sha", sha)?;
    push_toml_string_field(&mut out, "package", package_name)?;
    push_toml_string_field(&mut out, "provenance", &provenance)?;
    Ok(out)
}

pub(super) fn github_tag_url(git: &str, tag: &str) -> Option<String> {
    let url = Url::parse(git).ok()?;
    let host = url.host_str()?;
    if host != "github.com" {
        return None;
    }
    let path = url.path().trim_matches('/');
    let mut segments = path.split('/');
    let owner = segments.next()?;
    let repo = segments.next()?;
    Some(format!(
        "https://github.com/{owner}/{repo}/releases/tag/{tag}"
    ))
}

pub(super) fn push_toml_string_field(
    out: &mut String,
    key: &str,
    value: &str,
) -> Result<(), PackageError> {
    out.push_str(key);
    out.push_str(" = ");
    out.push_str(&toml_string_literal(value)?);
    out.push('\n');
    Ok(())
}

pub(super) fn render_unified_diff(
    old: &str,
    new: &str,
    label: &str,
) -> Result<String, PackageError> {
    let temp = tempfile::tempdir()
        .map_err(|error| PackageError::Ops(format!("failed to create temp dir: {error}")))?;
    let old_path = temp.path().join("old");
    let new_path = temp.path().join("new");
    fs::write(&old_path, old).map_err(|error| format!("failed to write diff input: {error}"))?;
    fs::write(&new_path, new).map_err(|error| format!("failed to write diff input: {error}"))?;
    let output = process::Command::new("git")
        .args(["diff", "--no-index", "--"])
        .arg(&old_path)
        .arg(&new_path)
        .output()
        .map_err(|error| {
            PackageError::Ops(format!("failed to render package-index diff: {error}"))
        })?;
    if !output.status.success() && output.status.code() != Some(1) {
        return Err(PackageError::Ops(format!(
            "failed to render package-index diff: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    let mut diff = String::from_utf8_lossy(&output.stdout).into_owned();
    let old_display = old_path.display().to_string();
    let new_display = new_path.display().to_string();
    diff = diff.replace(&format!("--- {old_display}"), &format!("--- a/{label}"));
    diff = diff.replace(&format!("+++ {new_display}"), &format!("+++ b/{label}"));
    Ok(diff)
}

pub(super) fn git_output<const N: usize>(
    repo: &Path,
    args: [&str; N],
) -> Result<String, PackageError> {
    run_command_output(repo, "git", args)
}

pub(super) fn git_status<const N: usize>(
    repo: &Path,
    args: [&str; N],
) -> Result<process::ExitStatus, PackageError> {
    process::Command::new("git")
        .current_dir(repo)
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .map(|output| output.status)
        .map_err(|error| PackageError::Ops(format!("failed to run git: {error}")))
}

pub(super) fn run_git_checked<const N: usize>(
    repo: &Path,
    args: [&str; N],
) -> Result<(), PackageError> {
    run_command_checked(repo, "git", args)
}

pub(super) fn run_command_checked<const N: usize>(
    cwd: &Path,
    program: &str,
    args: [&str; N],
) -> Result<(), PackageError> {
    run_command_output(cwd, program, args).map(|_| ())
}

pub(super) fn run_command_output<const N: usize>(
    cwd: &Path,
    program: &str,
    args: [&str; N],
) -> Result<String, PackageError> {
    let output = process::Command::new(program)
        .current_dir(cwd)
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .map_err(|error| PackageError::Ops(format!("failed to run {program}: {error}")))?;
    if !output.status.success() {
        return Err(PackageError::Ops(format!(
            "{} failed: {}",
            program,
            String::from_utf8_lossy(&output.stderr).trim_end()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

pub(super) fn sanitize_branch_segment(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '-'
            }
        })
        .collect()
}
