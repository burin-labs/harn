use super::*;

pub(crate) fn push_error(
    diagnostics: &mut Vec<PackageCheckDiagnostic>,
    field: impl Into<String>,
    message: impl Into<String>,
) {
    diagnostics.push(PackageCheckDiagnostic {
        field: field.into(),
        message: message.into(),
    });
}

pub(crate) fn push_warning(
    diagnostics: &mut Vec<PackageCheckDiagnostic>,
    field: impl Into<String>,
    message: impl Into<String>,
) {
    push_error(diagnostics, field, message);
}

pub(crate) fn validate_optional_url(
    value: Option<&str>,
    field: &str,
    errors: &mut Vec<PackageCheckDiagnostic>,
) {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        push_error(errors, field, format!("missing required {field}"));
        return;
    };
    if Url::parse(value).is_err() {
        push_error(errors, field, format!("{field} must be an absolute URL"));
    }
}

pub(crate) fn validate_docs_url(
    root: &Path,
    value: Option<&str>,
    errors: &mut Vec<PackageCheckDiagnostic>,
    warnings: &mut Vec<PackageCheckDiagnostic>,
) {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        push_warning(
            warnings,
            "[package].docs_url",
            "missing docs_url; `harn package docs` defaults to docs/api.md",
        );
        return;
    };
    if Url::parse(value).is_ok() {
        return;
    }
    let path = PathBuf::from(value);
    let path = if path.is_absolute() {
        path
    } else {
        root.join(path)
    };
    if !path.exists() {
        push_error(
            errors,
            "[package].docs_url",
            format!("docs_url path {} does not exist", path.display()),
        );
    }
}

pub(crate) fn validate_dependencies_for_publish(
    ctx: &ManifestContext,
    errors: &mut Vec<PackageCheckDiagnostic>,
    warnings: &mut Vec<PackageCheckDiagnostic>,
) {
    let mut aliases = BTreeSet::new();
    for (alias, dependency) in &ctx.manifest.dependencies {
        let field = format!("[dependencies].{alias}");
        if let Err(message) = validate_package_alias(alias) {
            push_error(errors, &field, message);
        }
        if !aliases.insert(alias) {
            push_error(errors, &field, "duplicate dependency alias");
        }
        match dependency {
            Dependency::Path(path) => push_error(
                errors,
                &field,
                format!("path-only dependency '{path}' is not publishable; pin a git tag, git rev, or registry version"),
            ),
            Dependency::Table(table) => {
                if table.version.is_some()
                    && (table.git.is_some()
                        || table.archive.is_some()
                        || table.path.is_some()
                        || table.rev.is_some()
                        || table.tag.is_some()
                        || table.branch.is_some())
                {
                    push_error(
                        errors,
                        &field,
                        "version dependencies resolve through the registry; do not combine version with git, archive, path, tag, rev, or branch",
                    );
                }
                if table.path.is_some() {
                    push_error(
                        errors,
                        &field,
                        "path dependencies are not publishable; pin a git tag, git rev, or registry version",
                    );
                }
                let source_count = usize::from(table.git.is_some())
                    + usize::from(table.archive.is_some())
                    + usize::from(table.path.is_some());
                if source_count > 1 {
                    push_error(errors, &field, "dependency must specify only one of git, archive, or path");
                }
                if table.git.is_none()
                    && table.archive.is_none()
                    && table.path.is_none()
                    && table.version.is_none()
                {
                    push_error(
                        errors,
                        &field,
                        "dependency must specify git, archive, registry version, or path",
                    );
                }
                let git_ref_count = usize::from(table.rev.is_some())
                    + usize::from(table.tag.is_some())
                    + usize::from(table.branch.is_some());
                if table.git.is_some() && git_ref_count > 1 {
                    push_error(errors, &field, "dependency cannot specify more than one of tag, rev, or branch");
                }
                if table.git.is_some() && git_ref_count == 0 {
                    push_error(errors, &field, "git dependency must specify tag, rev, or branch");
                }
                if table.archive.is_some() && git_ref_count > 0 {
                    push_error(errors, &field, "archive dependency cannot specify tag, rev, or branch");
                }
                if table.branch.is_some() {
                    push_warning(
                        warnings,
                        &field,
                        "branch dependencies are non-reproducible for publishing; prefer tag, rev, or registry version",
                    );
                }
                if let Some(version) = table.version.as_deref() {
                    if let Err(error) = parse_registry_version_req(version) {
                        push_error(errors, &field, error.to_string());
                    }
                }
                if let Some(git) = table.git.as_deref() {
                    if normalize_git_url(git).is_err() {
                        push_error(errors, &field, format!("invalid git source '{git}'"));
                    }
                }
                if let Some(archive) = table.archive.as_deref() {
                    if normalize_archive_url(archive).is_err() {
                        push_error(errors, &field, format!("invalid archive source '{archive}'"));
                    }
                    match table.checksum.as_deref() {
                        Some(checksum) if archive_cache_key(checksum).is_ok() => {}
                        Some(checksum) => {
                            push_error(errors, &field, format!("invalid archive checksum '{checksum}'"));
                        }
                        None => push_error(errors, &field, "archive dependency must specify checksum"),
                    }
                }
            }
        }
    }
}

pub(crate) fn validate_exports_for_publish(
    ctx: &ManifestContext,
    errors: &mut Vec<PackageCheckDiagnostic>,
    warnings: &mut Vec<PackageCheckDiagnostic>,
) -> Vec<PackageExportReport> {
    if ctx.manifest.exports.is_empty() {
        if ctx.manifest.rules.rule_dirs.is_empty() {
            push_error(
                errors,
                "[exports]",
                "publishable packages require at least one stable export or `[rules] ruleDirs`",
            );
        }
        return Vec::new();
    }

    let mut exports = Vec::new();
    for (name, rel_path) in &ctx.manifest.exports {
        let field = format!("[exports].{name}");
        if let Err(message) = validate_package_alias(name) {
            push_error(errors, &field, message);
        }
        let Ok(path) = safe_package_relative_path(&ctx.dir, rel_path) else {
            push_error(
                errors,
                &field,
                "export path must stay inside the package directory",
            );
            continue;
        };
        if path.extension() != Some(OsStr::new("harn")) {
            push_error(errors, &field, "export path must point at a .harn file");
            continue;
        }
        let content = match fs::read_to_string(&path) {
            Ok(content) => content,
            Err(error) => {
                push_error(
                    errors,
                    &field,
                    format!("failed to read export {}: {error}", path.display()),
                );
                continue;
            }
        };
        if let Err(error) = parse_harn_source(&content) {
            push_error(errors, &field, format!("failed to parse export: {error}"));
        }
        let symbols = extract_api_symbols(&content);
        if symbols.is_empty() {
            push_warning(
                warnings,
                &field,
                "exported module has no public symbols to document",
            );
        }
        for symbol in &symbols {
            if symbol.docs.is_none() {
                push_warning(
                    warnings,
                    &field,
                    format!(
                        "public {} '{}' has no doc comment",
                        symbol.kind, symbol.name
                    ),
                );
            }
        }
        exports.push(PackageExportReport {
            name: name.clone(),
            path: rel_path.clone(),
            symbols,
        });
    }
    exports.sort_by(|left, right| left.name.cmp(&right.name));
    exports
}

pub(crate) fn validate_package_interface_exports(
    ctx: &ManifestContext,
    errors: &mut Vec<PackageCheckDiagnostic>,
    warnings: &mut Vec<PackageCheckDiagnostic>,
) -> (Vec<PackageToolExportReport>, Vec<PackageSkillExportReport>) {
    let Some(package) = ctx.manifest.package.as_ref() else {
        return (Vec::new(), Vec::new());
    };

    validate_permission_tokens(
        &package.permissions,
        "[package].permissions",
        errors,
        warnings,
    );
    validate_host_requirements(
        &package.host_requirements,
        "[package].host_requirements",
        errors,
    );

    let mut tools = Vec::new();
    for (index, tool) in package.tools.iter().enumerate() {
        let field = format!("[[package.tools]] #{}", index + 1);
        if let Err(message) = validate_package_alias(&tool.name) {
            push_error(errors, format!("{field}.name"), message.to_string());
        }
        validate_required_manifest_string(&tool.module, &format!("{field}.module"), errors);
        validate_required_manifest_string(&tool.symbol, &format!("{field}.symbol"), errors);
        validate_package_module_path(ctx, &tool.module, &format!("{field}.module"), errors);
        validate_permission_tokens(
            &tool.permissions,
            &format!("{field}.permissions"),
            errors,
            warnings,
        );
        validate_host_requirements(
            &tool.host_requirements,
            &format!("{field}.host_requirements"),
            errors,
        );
        validate_schema_value(
            tool.input_schema.as_ref(),
            &format!("{field}.input_schema"),
            errors,
        );
        validate_schema_value(
            tool.output_schema.as_ref(),
            &format!("{field}.output_schema"),
            errors,
        );
        validate_tool_annotations(&tool.annotations, &format!("{field}.annotations"), errors);
        if tool.annotations.is_empty() {
            push_warning(
                warnings,
                format!("{field}.annotations"),
                "tool export has no annotations; policy evaluation will treat it conservatively",
            );
        }
        tools.push(PackageToolExportReport {
            name: tool.name.clone(),
            module: tool.module.clone(),
            symbol: tool.symbol.clone(),
            permissions: merge_package_requirements(&package.permissions, &tool.permissions),
            host_requirements: merge_package_requirements(
                &package.host_requirements,
                &tool.host_requirements,
            ),
        });
    }
    tools.sort_by(|left, right| left.name.cmp(&right.name));

    let mut skills = Vec::new();
    for (index, skill) in package.skills.iter().enumerate() {
        let field = format!("[[package.skills]] #{}", index + 1);
        if let Err(message) = validate_package_alias(&skill.name) {
            push_error(errors, format!("{field}.name"), message.to_string());
        }
        validate_required_manifest_string(&skill.path, &format!("{field}.path"), errors);
        validate_package_skill_path(ctx, &skill.path, &format!("{field}.path"), errors);
        validate_permission_tokens(
            &skill.permissions,
            &format!("{field}.permissions"),
            errors,
            warnings,
        );
        validate_host_requirements(
            &skill.host_requirements,
            &format!("{field}.host_requirements"),
            errors,
        );
        skills.push(PackageSkillExportReport {
            name: skill.name.clone(),
            path: skill.path.clone(),
            permissions: merge_package_requirements(&package.permissions, &skill.permissions),
            host_requirements: merge_package_requirements(
                &package.host_requirements,
                &skill.host_requirements,
            ),
        });
    }
    skills.sort_by(|left, right| left.name.cmp(&right.name));

    (tools, skills)
}

pub(crate) fn merge_package_requirements(base: &[String], item: &[String]) -> Vec<String> {
    let mut merged = BTreeSet::new();
    merged.extend(
        base.iter()
            .filter_map(|value| normalized_requirement(value)),
    );
    merged.extend(
        item.iter()
            .filter_map(|value| normalized_requirement(value)),
    );
    merged.into_iter().collect()
}

pub(super) fn normalized_requirement(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

pub(super) fn validate_required_manifest_string(
    value: &str,
    field: &str,
    errors: &mut Vec<PackageCheckDiagnostic>,
) {
    if value.trim().is_empty() {
        push_error(errors, field, format!("missing required {field}"));
    }
}

pub(super) fn validate_permission_tokens(
    permissions: &[String],
    field: &str,
    errors: &mut Vec<PackageCheckDiagnostic>,
    warnings: &mut Vec<PackageCheckDiagnostic>,
) {
    let mut seen = BTreeSet::new();
    for permission in permissions {
        let trimmed = permission.trim();
        if trimmed.is_empty() {
            push_error(errors, field, "permission entries cannot be empty");
            continue;
        }
        if trimmed.chars().any(char::is_whitespace) {
            push_error(
                errors,
                field,
                format!("permission {permission:?} cannot contain whitespace"),
            );
        }
        if !trimmed.contains(':') && !trimmed.contains('.') {
            push_warning(
                warnings,
                field,
                format!("permission {permission:?} should use a namespaced token"),
            );
        }
        if !seen.insert(trimmed.to_string()) {
            push_warning(
                warnings,
                field,
                format!("duplicate permission {permission:?}"),
            );
        }
    }
}

pub(crate) fn validate_host_requirements(
    requirements: &[String],
    field: &str,
    errors: &mut Vec<PackageCheckDiagnostic>,
) {
    let mut seen = BTreeSet::new();
    for requirement in requirements {
        let trimmed = requirement.trim();
        if trimmed.is_empty() {
            push_error(errors, field, "host requirement entries cannot be empty");
            continue;
        }
        let Some((capability, operation)) = trimmed.split_once('.') else {
            push_error(
                errors,
                field,
                format!("host requirement {requirement:?} must use capability.operation"),
            );
            continue;
        };
        if !valid_identifier(capability)
            || !(valid_identifier(operation) || operation == "*")
            || trimmed.matches('.').count() != 1
        {
            push_error(
                errors,
                field,
                format!("host requirement {requirement:?} must use valid capability.operation identifiers"),
            );
        }
        if !seen.insert(trimmed.to_string()) {
            push_error(
                errors,
                field,
                format!("duplicate host requirement {requirement:?}"),
            );
        }
    }
}

pub(super) fn validate_package_module_path(
    ctx: &ManifestContext,
    rel_path: &str,
    field: &str,
    errors: &mut Vec<PackageCheckDiagnostic>,
) {
    let Ok(path) = safe_package_relative_path(&ctx.dir, rel_path) else {
        push_error(errors, field, "module path must stay inside the package");
        return;
    };
    if path.extension() != Some(OsStr::new("harn")) {
        push_error(errors, field, "module path must point at a .harn file");
        return;
    }
    match fs::read_to_string(&path) {
        Ok(content) => {
            if let Err(error) = parse_harn_source(&content) {
                push_error(errors, field, format!("failed to parse module: {error}"));
            }
        }
        Err(error) => push_error(
            errors,
            field,
            format!("failed to read module {}: {error}", path.display()),
        ),
    }
}

pub(super) fn validate_package_skill_path(
    ctx: &ManifestContext,
    rel_path: &str,
    field: &str,
    errors: &mut Vec<PackageCheckDiagnostic>,
) {
    let Ok(path) = safe_package_relative_path(&ctx.dir, rel_path) else {
        push_error(errors, field, "skill path must stay inside the package");
        return;
    };
    let skill_file = if path.is_dir() {
        path.join("SKILL.md")
    } else {
        path
    };
    if skill_file.file_name() != Some(OsStr::new("SKILL.md")) {
        push_error(
            errors,
            field,
            "skill path must be a SKILL.md file or skill directory",
        );
        return;
    }
    match fs::read_to_string(&skill_file) {
        Ok(content) => {
            let (frontmatter, _) = harn_vm::skills::split_frontmatter(&content);
            if let Err(error) = harn_vm::skills::parse_frontmatter(frontmatter) {
                push_error(
                    errors,
                    field,
                    format!("invalid SKILL.md frontmatter: {error}"),
                );
            }
        }
        Err(error) => push_error(
            errors,
            field,
            format!("failed to read skill {}: {error}", skill_file.display()),
        ),
    }
}

pub(super) fn validate_schema_value(
    value: Option<&toml::Value>,
    field: &str,
    errors: &mut Vec<PackageCheckDiagnostic>,
) {
    let Some(value) = value else {
        return;
    };
    let json = match toml_value_to_json(value) {
        Ok(json) => json,
        Err(error) => {
            push_error(errors, field, error);
            return;
        }
    };
    let Some(object) = json.as_object() else {
        push_error(errors, field, "schema must be a table/object");
        return;
    };
    if let Some(schema_type) = object.get("type") {
        if !schema_type.is_string() {
            push_error(errors, field, "schema `type` must be a string when present");
        }
    }
    if let Some(required) = object.get("required") {
        let valid = required
            .as_array()
            .is_some_and(|items| items.iter().all(|item| item.as_str().is_some()));
        if !valid {
            push_error(errors, field, "schema `required` must be a list of strings");
        }
    }
}

pub(super) fn validate_tool_annotations(
    annotations: &BTreeMap<String, toml::Value>,
    field: &str,
    errors: &mut Vec<PackageCheckDiagnostic>,
) {
    if annotations.is_empty() {
        return;
    }
    let json = match toml_value_to_json(&toml::Value::Table(
        annotations
            .clone()
            .into_iter()
            .collect::<toml::map::Map<String, toml::Value>>(),
    )) {
        Ok(json) => json,
        Err(error) => {
            push_error(errors, field, error);
            return;
        }
    };
    if let Err(error) = serde_json::from_value::<harn_vm::tool_annotations::ToolAnnotations>(json) {
        push_error(
            errors,
            field,
            format!("annotations do not match ToolAnnotations: {error}"),
        );
    }
}

pub(super) fn toml_value_to_json(value: &toml::Value) -> Result<serde_json::Value, String> {
    serde_json::to_value(value).map_err(|error| format!("failed to normalize TOML value: {error}"))
}

pub(crate) fn parse_harn_source(source: &str) -> Result<(), PackageError> {
    let mut lexer = harn_lexer::Lexer::new(source);
    let tokens = lexer.tokenize().map_err(|error| error.to_string())?;
    let mut parser = harn_parser::Parser::new(tokens);
    parser
        .parse()
        .map(|_| ())
        .map_err(|error| PackageError::Ops(error.to_string()))
}

pub(crate) fn safe_package_relative_path(
    root: &Path,
    rel_path: &str,
) -> Result<PathBuf, PackageError> {
    let rel = PathBuf::from(rel_path);
    if rel.is_absolute()
        || has_windows_rooted_or_drive_relative_prefix(rel_path)
        || has_windows_separator_escape(rel_path)
        || rel.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::Prefix(_)
                    | std::path::Component::RootDir
            )
        })
    {
        return Err(format!("path {rel_path:?} escapes package root").into());
    }
    Ok(root.join(rel))
}

pub(super) fn has_windows_rooted_or_drive_relative_prefix(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    let bytes = normalized.as_bytes();
    normalized.starts_with('/')
        || (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
}

pub(super) fn has_windows_separator_escape(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    Path::new(&normalized).components().any(|component| {
        matches!(
            component,
            std::path::Component::ParentDir
                | std::path::Component::Prefix(_)
                | std::path::Component::RootDir
        )
    })
}

pub(crate) fn extract_api_symbols(source: &str) -> Vec<PackageApiSymbol> {
    static DECL_RE: OnceLock<Regex> = OnceLock::new();
    let decl_re = DECL_RE.get_or_init(|| {
        Regex::new(r"^\s*pub\s+(fn|pipeline|tool|skill|struct|enum|type|interface)\s+([A-Za-z_][A-Za-z0-9_]*)\b(.*)$")
            .expect("valid declaration regex")
    });
    let mut docs: Vec<String> = Vec::new();
    let mut symbols = Vec::new();
    let mut in_block_doc = false;
    for line in source.lines() {
        let trimmed = line.trim();
        if in_block_doc {
            // Collect content between /** and */, stripping the conventional
            // ` * ` continuation marker so docs render the same regardless of
            // which form (`///` or `/** */`) authors picked.
            let (content, closes) = match trimmed.split_once("*/") {
                Some((before, _)) => (before, true),
                None => (trimmed, false),
            };
            let stripped = content
                .strip_prefix("* ")
                .or_else(|| content.strip_prefix('*'))
                .unwrap_or(content)
                .trim();
            if !stripped.is_empty() {
                docs.push(stripped.to_string());
            }
            if closes {
                in_block_doc = false;
            }
            continue;
        }
        if let Some(doc) = trimmed.strip_prefix("///") {
            docs.push(doc.trim().to_string());
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("/**") {
            // `/** … */` on a single line collapses to one doc line; the
            // multi-line opener `/**` (with no `*/` on the same line) flips
            // the block-doc flag so subsequent lines are absorbed above.
            if let Some((inner, _)) = rest.split_once("*/") {
                let stripped = inner.trim();
                if !stripped.is_empty() {
                    docs.push(stripped.to_string());
                }
            } else {
                let stripped = rest.trim();
                if !stripped.is_empty() {
                    docs.push(stripped.to_string());
                }
                in_block_doc = true;
            }
            continue;
        }
        if trimmed.is_empty() {
            continue;
        }
        if let Some(captures) = decl_re.captures(line) {
            let kind = captures.get(1).expect("kind").as_str().to_string();
            let name = captures.get(2).expect("name").as_str().to_string();
            let signature = trim_signature(line);
            let doc_text = (!docs.is_empty()).then(|| docs.join("\n"));
            symbols.push(PackageApiSymbol {
                kind,
                name,
                signature,
                docs: doc_text,
            });
        }
        docs.clear();
    }
    symbols
}

pub(crate) fn trim_signature(line: &str) -> String {
    let mut signature = line.trim().to_string();
    if let Some((before, _)) = signature.split_once('{') {
        signature = before.trim_end().to_string();
    }
    signature
}
