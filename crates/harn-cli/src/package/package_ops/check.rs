use super::*;

pub(crate) fn check_package_impl(
    anchor: Option<&Path>,
) -> Result<PackageCheckReport, PackageError> {
    let ctx = load_manifest_context_for_anchor(anchor)?;
    let manifest_path = ctx.manifest_path();
    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    let package = ctx.manifest.package.as_ref();
    let name = package.and_then(|package| package.name.clone());
    let version = package.and_then(|package| package.version.clone());
    let package_name = required_package_string(
        package.and_then(|package| package.name.as_deref()),
        "[package].name",
        &mut errors,
    );
    if let Some(name) = package_name {
        if let Err(message) = validate_package_alias(name) {
            push_error(&mut errors, "[package].name", message);
        }
    }
    required_package_string(
        package.and_then(|package| package.version.as_deref()),
        "[package].version",
        &mut errors,
    );
    required_package_string(
        package.and_then(|package| package.description.as_deref()),
        "[package].description",
        &mut errors,
    );
    required_package_string(
        package.and_then(|package| package.license.as_deref()),
        "[package].license",
        &mut errors,
    );
    if !ctx.dir.join("README.md").is_file() {
        push_error(&mut errors, "README.md", "package README.md is required");
    }
    if !ctx.dir.join("LICENSE").is_file() && package.and_then(|p| p.license.as_deref()).is_none() {
        push_error(
            &mut errors,
            "[package].license",
            "publishable packages require a license field or LICENSE file",
        );
    }

    validate_optional_url(
        package.and_then(|package| package.repository.as_deref()),
        "[package].repository",
        &mut errors,
    );
    validate_docs_url(
        &ctx.dir,
        package.and_then(|package| package.docs_url.as_deref()),
        &mut errors,
        &mut warnings,
    );
    match package.and_then(|package| package.harn.as_deref()) {
        Some(range) if supports_current_harn(range) => {}
        Some(range) => push_error(
            &mut errors,
            "[package].harn",
            format!(
                "unsupported Harn version range '{range}'; include the current {} line, for example {}",
                current_harn_line_label(),
                current_harn_range_example()
            ),
        ),
        None => push_error(
            &mut errors,
            "[package].harn",
            format!(
                "missing Harn compatibility metadata; add harn = \"{}\"",
                current_harn_range_example()
            ),
        ),
    }

    validate_dependencies_for_publish(&ctx, &mut errors, &mut warnings);
    if let Err(error) = validate_handoff_routes(&ctx.manifest.handoff_routes, &ctx.manifest) {
        push_error(&mut errors, "handoff_routes", error.to_string());
    }
    validate_rule_pack_for_publish(&ctx, &mut errors);
    let exports = validate_exports_for_publish(&ctx, &mut errors, &mut warnings);
    let (tools, skills) = validate_package_interface_exports(&ctx, &mut errors, &mut warnings);

    Ok(PackageCheckReport {
        package_dir: ctx.dir.display().to_string(),
        manifest_path: manifest_path.display().to_string(),
        name,
        version,
        errors,
        warnings,
        exports,
        tools,
        skills,
    })
}

pub(super) fn validate_rule_pack_for_publish(
    ctx: &ManifestContext,
    errors: &mut Vec<PackageCheckDiagnostic>,
) {
    if ctx.manifest.rules.rule_dirs.is_empty() {
        return;
    }
    if let Err(error) = collect_rule_pack_metadata(ctx) {
        push_error(errors, "[rules] ruleDirs", error.to_string());
    }
}

pub(crate) fn collect_rule_pack_metadata(
    ctx: &ManifestContext,
) -> Result<Option<RegistryRulePackInfo>, PackageError> {
    if ctx.manifest.rules.rule_dirs.is_empty() {
        return Ok(None);
    }

    let mut rule_count = 0usize;
    let mut languages = BTreeSet::new();
    let mut safety = BTreeMap::<String, usize>::new();
    for rel in &ctx.manifest.rules.rule_dirs {
        let dir = safe_package_relative_path(&ctx.dir, rel)?;
        if !dir.is_dir() {
            return Err(PackageError::Validation(format!(
                "`[rules] ruleDirs` entry `{rel}` is not a directory ({})",
                dir.display()
            )));
        }
        let mut paths: Vec<_> = fs::read_dir(&dir)
            .map_err(|error| format!("failed to read rule dir {}: {error}", dir.display()))?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.is_file() && path.extension().and_then(|ext| ext.to_str()) == Some("toml")
            })
            .collect();
        paths.sort();
        for path in paths {
            let rule = read_rule_file_metadata(&path)?;
            rule_count += 1;
            languages.insert(rule.language);
            *safety.entry(rule.safety).or_default() += 1;
        }
    }

    if rule_count == 0 {
        return Err(PackageError::Validation(
            "rule packs must contain at least one `*.toml` rule under `[rules] ruleDirs`"
                .to_string(),
        ));
    }

    Ok(Some(RegistryRulePackInfo {
        rule_count,
        languages: languages.into_iter().collect(),
        safety_summary: safety
            .into_iter()
            .map(|(name, count)| format!("{name}:{count}"))
            .collect(),
    }))
}

pub(super) struct RuleFileMetadata {
    pub(super) language: String,
    pub(super) safety: String,
}

#[cfg(feature = "hostlib")]
pub(super) fn read_rule_file_metadata(path: &Path) -> Result<RuleFileMetadata, PackageError> {
    let source = fs::read_to_string(path)
        .map_err(|error| format!("failed to read rule {}: {error}", path.display()))?;
    let rule = harn_rules::Rule::from_toml_str(&source)
        .map_err(|error| format!("failed to parse rule {}: {error}", path.display()))?;
    let safety = if rule.fix.is_some() {
        rule.safety.as_str()
    } else {
        "no-fix"
    };
    Ok(RuleFileMetadata {
        language: rule.language,
        safety: safety.to_string(),
    })
}

#[cfg(not(feature = "hostlib"))]
pub(super) fn read_rule_file_metadata(path: &Path) -> Result<RuleFileMetadata, PackageError> {
    #[derive(Deserialize)]
    struct MinimalRule {
        id: String,
        language: String,
        #[serde(default)]
        safety: Option<String>,
        #[serde(default)]
        fix: Option<String>,
        rule: toml::Value,
    }

    let source = fs::read_to_string(path)
        .map_err(|error| format!("failed to read rule {}: {error}", path.display()))?;
    let rule: MinimalRule = toml::from_str(&source)
        .map_err(|error| format!("failed to parse rule {}: {error}", path.display()))?;
    if rule.id.trim().is_empty() || rule.language.trim().is_empty() {
        return Err(PackageError::Validation(format!(
            "rule {} must declare non-empty `id` and `language`",
            path.display()
        )));
    }
    let safety = if rule.fix.is_some() {
        rule.safety.unwrap_or_else(|| "scope-local".to_string())
    } else {
        "no-fix".to_string()
    };
    let _ = rule.rule;
    Ok(RuleFileMetadata {
        language: rule.language,
        safety,
    })
}
