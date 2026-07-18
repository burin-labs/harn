use std::fs;
use std::path::{Path, PathBuf};

use include_dir::{include_dir, Dir};

use crate::cli::{PersonaMaterializeArgs, PersonaNewArgs, PersonaTemplateKind};

use super::persona_prompt::{self, PromptCompiledPersona, PromptCompiledPersonaLowering};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonaScaffoldResult {
    pub root: PathBuf,
    pub files: Vec<PathBuf>,
}

static PERSONA_TEMPLATE_ASSETS: Dir<'_> =
    include_dir!("$CARGO_MANIFEST_DIR/assets/persona-templates");

async fn scaffold_persona(args: &PersonaNewArgs) -> Result<PersonaScaffoldResult, String> {
    let raw_name = args
        .name
        .as_deref()
        .ok_or_else(|| "manual persona scaffolding requires a name".to_string())?;
    let template = args
        .template
        .ok_or_else(|| "manual persona scaffolding requires --template".to_string())?;
    let name = normalize_name(raw_name)?;
    let target_root = args.output_root.join(&name);
    if target_root.exists() && !args.force {
        return Err(format!(
            "{} already exists; pass --force to replace it after validation",
            target_root.display()
        ));
    }

    let prepared = prepare_persona_package(&args.output_root, &name, template)?;
    validate_prepared_persona(&prepared, &name).await?;
    publish_prepared_persona(prepared, target_root, args.force)
}

pub async fn scaffold_persona_package(
    name: &str,
    template: &str,
    output_root: &Path,
    force: bool,
) -> Result<PersonaScaffoldResult, String> {
    let args = PersonaNewArgs {
        name: Some(name.to_string()),
        template: Some(parse_template_kind(template)?),
        from_prompt: None,
        prompt_name: None,
        provider: None,
        model: None,
        max_tokens: None,
        output_root: output_root.to_path_buf(),
        force,
    };
    scaffold_persona(&args).await
}

async fn materialize_persona(
    args: &PersonaMaterializeArgs,
) -> Result<PersonaScaffoldResult, String> {
    materialize_persona_package(&args.blueprint, &args.output_root, args.force).await
}

pub async fn materialize_persona_package(
    blueprint_path: &Path,
    output_root: &Path,
    force: bool,
) -> Result<PersonaScaffoldResult, String> {
    let lowering = persona_prompt::compile_blueprint_path(blueprint_path).await?;
    materialize_prompt_compiled_lowering(lowering, output_root, force).await
}

pub(super) async fn materialize_prompt_compiled_lowering(
    lowering: PromptCompiledPersonaLowering,
    output_root: &Path,
    force: bool,
) -> Result<PersonaScaffoldResult, String> {
    let name = normalize_name(&lowering.persona.name)?;
    let template = parse_template_kind(&lowering.template)?;
    let target_root = output_root.join(&name);
    if target_root.exists() && !force {
        return Err(format!(
            "{} already exists; pass --force to replace it after validation",
            target_root.display()
        ));
    }

    let mut prepared = prepare_persona_package(output_root, &name, template)?;
    apply_prompt_compiled_overlay(&mut prepared, &lowering)?;
    validate_prepared_persona(&prepared, &name).await?;
    publish_prepared_persona(prepared, target_root, force)
}

pub(crate) async fn run_new(args: &PersonaNewArgs) -> Result<(), String> {
    let (result, name) = if let Some(prompt) = args.from_prompt.as_deref() {
        let compiled = persona_prompt::compile_prompt(
            prompt,
            args.prompt_name.as_deref(),
            args.provider.as_deref(),
            args.model.as_deref(),
            args.max_tokens,
        )
        .await?;
        let lowering = compiled.receipt.into_lowering()?;
        let name = lowering.persona.name.clone();
        let result =
            materialize_prompt_compiled_lowering(lowering, &args.output_root, args.force).await?;
        (result, name)
    } else {
        let name = args
            .name
            .as_deref()
            .ok_or_else(|| "manual persona scaffolding requires a name".to_string())?;
        (scaffold_persona(args).await?, normalize_name(name)?)
    };
    println!("created persona package {}", result.root.display());
    for file in &result.files {
        println!("  create {}", file.display());
    }
    println!();
    println!("  harn persona doctor {name}");
    Ok(())
}

pub(crate) async fn run_materialize(args: &PersonaMaterializeArgs) -> Result<(), String> {
    let result = materialize_persona(args).await?;
    println!("materialized persona package {}", result.root.display());
    for file in &result.files {
        println!("  create {}", file.display());
    }
    Ok(())
}

fn normalize_name(raw: &str) -> Result<String, String> {
    let name = raw.trim().replace('-', "_");
    if !valid_identifier(&name) {
        return Err(format!(
            "persona name must be an identifier-like token, got {raw:?}"
        ));
    }
    Ok(name)
}

fn valid_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    match chars.next() {
        Some(ch) if ch == '_' || ch.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn parse_template_kind(value: &str) -> Result<PersonaTemplateKind, String> {
    match value {
        "deterministic-sweeper" => Ok(PersonaTemplateKind::DeterministicSweeper),
        "hybrid-classify-then-act" => Ok(PersonaTemplateKind::HybridClassifyThenAct),
        "frontier-judgment-loop" => Ok(PersonaTemplateKind::FrontierJudgmentLoop),
        _ => Err(format!("unknown persona template {value:?}")),
    }
}

fn to_title(name: &str) -> String {
    name.split('_')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => format!("{}{}", first.to_ascii_uppercase(), chars.as_str()),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn to_package_slug(name: &str) -> String {
    name.replace('_', "-")
}

struct TemplateIdentity {
    persona_name: String,
    persona_title: String,
    package_slug: String,
    template_kind: &'static str,
}

struct PreparedPersonaPackage {
    staging: tempfile::TempDir,
    relative_files: Vec<PathBuf>,
}

impl TemplateIdentity {
    fn new(name: &str, kind: PersonaTemplateKind) -> Self {
        Self {
            persona_name: name.to_string(),
            persona_title: to_title(name),
            package_slug: to_package_slug(name),
            template_kind: kind.as_str(),
        }
    }
}

fn prepare_persona_package(
    output_root: &Path,
    name: &str,
    kind: PersonaTemplateKind,
) -> Result<PreparedPersonaPackage, String> {
    fs::create_dir_all(output_root)
        .map_err(|error| format!("failed to create {}: {error}", output_root.display()))?;
    let staging = tempfile::Builder::new()
        .prefix(".harn-persona-stage-")
        .tempdir_in(output_root)
        .map_err(|error| {
            format!(
                "failed to create persona staging directory in {}: {error}",
                output_root.display()
            )
        })?;
    let identity = TemplateIdentity::new(name, kind);
    let template_dir = PERSONA_TEMPLATE_ASSETS
        .get_dir(kind.as_str())
        .ok_or_else(|| format!("canonical persona template {} is missing", kind.as_str()))?;
    let mut relative_files = Vec::new();

    write_embedded_template_dir(
        template_dir,
        Path::new(""),
        staging.path(),
        &identity,
        &mut relative_files,
    )?;
    let readme = PERSONA_TEMPLATE_ASSETS
        .get_file("package-README.md")
        .ok_or_else(|| "canonical persona package README is missing".to_string())?;
    write_embedded_template_file(
        Path::new("README.md"),
        readme
            .contents_utf8()
            .ok_or_else(|| "canonical persona package README is not UTF-8".to_string())?,
        staging.path(),
        &identity,
        &mut relative_files,
    )?;
    relative_files.sort();

    Ok(PreparedPersonaPackage {
        staging,
        relative_files,
    })
}

async fn validate_prepared_persona(
    prepared: &PreparedPersonaPackage,
    name: &str,
) -> Result<(), String> {
    let manifest = prepared.staging.path().join("harn.toml");
    let report = match crate::commands::persona_doctor::doctor_report_for_persona(
        Some(&manifest),
        name,
        10_000,
    )
    .await
    {
        Ok(report) | Err(report) => report,
    };
    let failures = report
        .checks
        .iter()
        .filter(|check| check.status != crate::commands::persona_doctor::DoctorStatus::Green)
        .map(|check| format!("{}: {}", check.name, check.message))
        .collect::<Vec<_>>();
    if !failures.is_empty() {
        return Err(format!(
            "generated persona failed strict validation: {}",
            failures.join("; ")
        ));
    }
    remove_validation_artifacts(prepared.staging.path())?;
    Ok(())
}

fn remove_validation_artifacts(root: &Path) -> Result<(), String> {
    for name in [".harn", ".harn-runs"] {
        let path = root.join(name);
        if !path.exists() {
            continue;
        }
        fs::remove_dir_all(&path).map_err(|error| {
            format!(
                "failed to remove validation artifact {}: {error}",
                path.display()
            )
        })?;
    }
    Ok(())
}

fn publish_prepared_persona(
    prepared: PreparedPersonaPackage,
    target_root: PathBuf,
    force: bool,
) -> Result<PersonaScaffoldResult, String> {
    let PreparedPersonaPackage {
        staging,
        relative_files,
    } = prepared;
    let staged_root = staging.keep();
    if let Err(error) = publish_staged_persona(&staged_root, &target_root, force) {
        let _ = fs::remove_dir_all(&staged_root);
        return Err(error);
    }
    let files = relative_files
        .into_iter()
        .map(|relative| target_root.join(relative))
        .collect();
    Ok(PersonaScaffoldResult {
        root: target_root,
        files,
    })
}

fn publish_staged_persona(staged: &Path, target: &Path, force: bool) -> Result<(), String> {
    publish_staged_persona_with(staged, target, force, |from, to| fs::rename(from, to))
}

fn publish_staged_persona_with(
    staged: &Path,
    target: &Path,
    force: bool,
    mut rename: impl FnMut(&Path, &Path) -> std::io::Result<()>,
) -> Result<(), String> {
    if !target.exists() {
        return rename(staged, target).map_err(|error| {
            format!(
                "failed to publish persona package {}: {error}",
                target.display()
            )
        });
    }
    if !force {
        return Err(format!(
            "{} already exists; pass --force to replace it after validation",
            target.display()
        ));
    }

    let backup = unique_backup_path(target)?;
    rename(target, &backup).map_err(|error| {
        format!(
            "failed to stage existing persona package {} for replacement: {error}",
            target.display()
        )
    })?;
    if let Err(publish_error) = rename(staged, target) {
        return match rename(&backup, target) {
            Ok(()) => Err(format!(
                "failed to publish persona package {}; restored the previous package: {publish_error}",
                target.display()
            )),
            Err(restore_error) => Err(format!(
                "failed to publish persona package {} ({publish_error}) and failed to restore its backup {} ({restore_error})",
                target.display(),
                backup.display()
            )),
        };
    }

    remove_path(&backup).map_err(|error| {
        format!(
            "published persona package {}, but failed to remove backup {}: {error}",
            target.display(),
            backup.display()
        )
    })
}

fn unique_backup_path(target: &Path) -> Result<PathBuf, String> {
    let parent = target
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", target.display()))?;
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("{} has no UTF-8 file name", target.display()))?;
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    loop {
        let counter = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let candidate = parent.join(format!(
            ".{name}.harn-persona-backup-{}-{counter}",
            std::process::id()
        ));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
}

fn remove_path(path: &Path) -> std::io::Result<()> {
    if path.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

fn render_template_path(template: &Path, identity: &TemplateIdentity) -> Result<PathBuf, String> {
    let template = template.to_str().ok_or_else(|| {
        format!(
            "canonical persona template path is not UTF-8: {}",
            template.display()
        )
    })?;
    Ok(PathBuf::from(
        template.replace("template_persona", &identity.persona_name),
    ))
}

fn write_embedded_template_dir(
    template_dir: &Dir<'_>,
    relative_root: &Path,
    staging_root: &Path,
    identity: &TemplateIdentity,
    relative_files: &mut Vec<PathBuf>,
) -> Result<(), String> {
    for file in template_dir.files() {
        let name = file.path().file_name().ok_or_else(|| {
            format!(
                "canonical persona template file has no name: {}",
                file.path().display()
            )
        })?;
        let relative = relative_root.join(name);
        let content = file.contents_utf8().ok_or_else(|| {
            format!(
                "canonical persona template file is not UTF-8: {}",
                file.path().display()
            )
        })?;
        write_embedded_template_file(&relative, content, staging_root, identity, relative_files)?;
    }
    for directory in template_dir.dirs() {
        let name = directory.path().file_name().ok_or_else(|| {
            format!(
                "canonical persona template directory has no name: {}",
                directory.path().display()
            )
        })?;
        write_embedded_template_dir(
            directory,
            &relative_root.join(name),
            staging_root,
            identity,
            relative_files,
        )?;
    }
    Ok(())
}

fn write_embedded_template_file(
    template_path: &Path,
    template_content: &str,
    staging_root: &Path,
    identity: &TemplateIdentity,
    relative_files: &mut Vec<PathBuf>,
) -> Result<(), String> {
    let relative = render_template_path(template_path, identity)?;
    let path = staging_root.join(&relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    }
    let content = render_template_file(template_path, template_content, identity)?;
    fs::write(&path, content)
        .map_err(|error| format!("failed to write {}: {error}", path.display()))?;
    relative_files.push(relative);
    Ok(())
}

fn render_template_file(
    template_path: &Path,
    template_content: &str,
    identity: &TemplateIdentity,
) -> Result<String, String> {
    if template_path == Path::new("harn.toml") {
        return render_manifest(template_content, identity);
    }
    let mut rendered = template_content
        .replace("template_persona", &identity.persona_name)
        .replace("template-persona", &identity.package_slug);
    if template_path == Path::new("README.md") {
        rendered = rendered
            .replace("{{persona_name}}", &identity.persona_name)
            .replace("{{persona_title}}", &identity.persona_title)
            .replace("{{template_kind}}", identity.template_kind);
    }
    Ok(rendered)
}

fn render_manifest(template: &str, identity: &TemplateIdentity) -> Result<String, String> {
    let mut manifest = toml::from_str::<toml::Value>(template)
        .map_err(|error| format!("canonical persona template manifest is invalid: {error}"))?;
    {
        let package = manifest
            .get_mut("package")
            .and_then(toml::Value::as_table_mut)
            .ok_or_else(|| "canonical persona template is missing [package]".to_string())?;
        package.insert(
            "name".to_string(),
            toml::Value::String(format!("harn-{}-persona", identity.package_slug)),
        );
        package.insert(
            "description".to_string(),
            toml::Value::String(format!(
                "{} persona package generated from the {} template.",
                identity.persona_title, identity.template_kind
            )),
        );
    }
    {
        let persona = manifest
            .get_mut("personas")
            .and_then(toml::Value::as_array_mut)
            .and_then(|personas| personas.first_mut())
            .and_then(toml::Value::as_table_mut)
            .ok_or_else(|| "canonical persona template is missing [[personas]]".to_string())?;
        persona.insert(
            "name".to_string(),
            toml::Value::String(identity.persona_name.clone()),
        );
        persona.insert(
            "description".to_string(),
            toml::Value::String(format!(
                "{} persona generated from the {} template.",
                identity.persona_title, identity.template_kind
            )),
        );
        persona.insert(
            "entry_workflow".to_string(),
            toml::Value::String(format!("src/{}.harn#run", identity.persona_name)),
        );
    }
    toml::to_string_pretty(&manifest)
        .map_err(|error| format!("failed to render persona manifest: {error}"))
}

fn apply_prompt_compiled_overlay(
    prepared: &mut PreparedPersonaPackage,
    lowering: &PromptCompiledPersonaLowering,
) -> Result<(), String> {
    let root = prepared.staging.path();
    let manifest_path = root.join("harn.toml");
    let manifest = fs::read_to_string(&manifest_path)
        .map_err(|error| format!("failed to read {}: {error}", manifest_path.display()))?;
    fs::write(
        &manifest_path,
        render_prompt_compiled_manifest(&manifest, lowering)?,
    )
    .map_err(|error| format!("failed to write {}: {error}", manifest_path.display()))?;

    let source_path = root
        .join("src")
        .join(format!("{}.harn", lowering.persona.name));
    let source = fs::read_to_string(&source_path)
        .map_err(|error| format!("failed to read {}: {error}", source_path.display()))?;
    fs::write(
        &source_path,
        render_prompt_compiled_source(&source, lowering)?,
    )
    .map_err(|error| format!("failed to write {}: {error}", source_path.display()))?;

    let prompt_path = root.join("prompts/system.harn.prompt");
    let prompt = fs::read_to_string(&prompt_path)
        .map_err(|error| format!("failed to read {}: {error}", prompt_path.display()))?;
    if prompt.contains("{{persona_goal}}") {
        return Err("canonical persona prompt already defines persona_goal".to_string());
    }
    fs::write(
        &prompt_path,
        format!("{}\n\nGoal: {{{{persona_goal}}}}\n", prompt.trim_end()),
    )
    .map_err(|error| format!("failed to write {}: {error}", prompt_path.display()))?;

    Ok(())
}

fn render_prompt_compiled_manifest(
    rendered_template: &str,
    lowering: &PromptCompiledPersonaLowering,
) -> Result<String, String> {
    let mut manifest = toml::from_str::<toml::Value>(rendered_template)
        .map_err(|error| format!("generated persona manifest is invalid: {error}"))?;
    {
        let package = manifest
            .get_mut("package")
            .and_then(toml::Value::as_table_mut)
            .ok_or_else(|| "generated persona manifest is missing [package]".to_string())?;
        package.insert(
            "name".to_string(),
            toml::Value::String(format!(
                "harn-{}-persona",
                to_package_slug(&lowering.persona.name)
            )),
        );
        package.insert(
            "description".to_string(),
            toml::Value::String(lowering.persona.description.clone()),
        );
    }
    {
        let persona = manifest
            .get_mut("personas")
            .and_then(toml::Value::as_array_mut)
            .and_then(|personas| personas.first_mut())
            .and_then(toml::Value::as_table_mut)
            .ok_or_else(|| "generated persona manifest is missing [[personas]]".to_string())?;
        persona.insert(
            "name".to_string(),
            toml::Value::String(lowering.persona.name.clone()),
        );
        persona.insert(
            "description".to_string(),
            toml::Value::String(lowering.persona.description.clone()),
        );
        persona.insert(
            "entry_workflow".to_string(),
            toml::Value::String(format!("src/{}.harn#run", lowering.persona.name)),
        );
        persona.insert(
            "autonomy_tier".to_string(),
            toml::Value::String("suggest".to_string()),
        );
        persona.insert(
            "receipt_policy".to_string(),
            toml::Value::String("required".to_string()),
        );
        persona.remove("triggers");
        persona.remove("schedules");
    }

    let trigger = lowering
        .triggers
        .first()
        .ok_or_else(|| "prompt_compiled_v1 lowering has no trigger".to_string())?;
    let mut root_trigger = toml::map::Map::new();
    root_trigger.insert("id".to_string(), toml::Value::String(trigger.id.clone()));
    root_trigger.insert(
        "kind".to_string(),
        toml::Value::String(trigger.kind.clone()),
    );
    root_trigger.insert(
        "provider".to_string(),
        toml::Value::String(trigger.provider.clone()),
    );
    root_trigger.insert(
        "match".to_string(),
        toml::Value::Table(toml::map::Map::from_iter([(
            "events".to_string(),
            toml::Value::Array(
                trigger
                    .events
                    .iter()
                    .cloned()
                    .map(toml::Value::String)
                    .collect(),
            ),
        )])),
    );
    root_trigger.insert(
        "handler".to_string(),
        toml::Value::String(trigger.handler.clone()),
    );
    if !trigger.secrets.is_empty() {
        root_trigger.insert(
            "secrets".to_string(),
            toml::Value::Table(
                trigger
                    .secrets
                    .iter()
                    .map(|(name, reference)| (name.clone(), toml::Value::String(reference.clone())))
                    .collect(),
            ),
        );
    }
    if let Some(schedule) = &trigger.schedule {
        root_trigger.insert(
            "schedule".to_string(),
            toml::Value::String(schedule.clone()),
        );
    }
    if let Some(timezone) = &trigger.timezone {
        root_trigger.insert(
            "timezone".to_string(),
            toml::Value::String(timezone.clone()),
        );
    }
    let root = manifest
        .as_table_mut()
        .ok_or_else(|| "generated persona manifest root is not a table".to_string())?;
    root.remove("triggers");
    root.insert(
        "triggers".to_string(),
        toml::Value::Array(vec![toml::Value::Table(root_trigger)]),
    );

    toml::to_string_pretty(&manifest)
        .map_err(|error| format!("failed to render prompt-compiled manifest: {error}"))
}

fn render_prompt_compiled_source(
    rendered_template: &str,
    lowering: &PromptCompiledPersonaLowering,
) -> Result<String, String> {
    let needle = "template_kind:";
    if rendered_template.matches(needle).count() != 1 {
        return Err(
            "canonical persona template must render exactly one prompt binding".to_string(),
        );
    }
    // Liquid renders variable values as data, so a goal such as `{{name}}`
    // remains literal text rather than becoming executable template syntax.
    let goal = serde_json::to_string(&lowering.persona.goal)
        .map_err(|error| format!("failed to encode persona goal: {error}"))?;
    let source = rendered_template.replacen(
        needle,
        &format!("persona_goal: {goal},\n      template_kind:"),
        1,
    );
    let source = rewrite_persona_annotation(&source, &lowering.persona)?;
    Ok(source)
}

fn rewrite_persona_annotation(
    source: &str,
    persona: &PromptCompiledPersona,
) -> Result<String, String> {
    let name = serde_json::to_string(&persona.name)
        .map_err(|error| format!("failed to encode persona name: {error}"))?;
    let description = serde_json::to_string(&persona.description)
        .map_err(|error| format!("failed to encode persona description: {error}"))?;
    let mut rewritten = String::with_capacity(source.len());
    let mut found = false;
    for line in source.split_inclusive('\n') {
        let (body, newline) = line
            .strip_suffix('\n')
            .map_or((line, ""), |body| (body, "\n"));
        let trimmed = body.trim_start();
        if !trimmed.starts_with("@persona(") {
            rewritten.push_str(line);
            continue;
        }
        if found {
            return Err(
                "canonical persona source declares more than one @persona annotation".to_string(),
            );
        }
        found = true;
        let indentation = &body[..body.len() - trimmed.len()];
        let (_, after_description) = trimmed
            .split_once(", description: ")
            .ok_or_else(|| "canonical @persona annotation is missing description".to_string())?;
        let (_, after_tools) = after_description.split_once("\", tools: ").ok_or_else(|| {
            "canonical @persona annotation has an unsupported description form".to_string()
        })?;
        let (tools, after_autonomy) = after_tools
            .split_once(", autonomy: ")
            .ok_or_else(|| "canonical @persona annotation is missing autonomy".to_string())?;
        let (_, receipts) = after_autonomy
            .split_once(", receipts: ")
            .ok_or_else(|| "canonical @persona annotation is missing receipts".to_string())?;
        rewritten.push_str(indentation);
        rewritten.push_str("@persona(name: ");
        rewritten.push_str(&name);
        rewritten.push_str(", description: ");
        rewritten.push_str(&description);
        rewritten.push_str(", tools: ");
        rewritten.push_str(tools);
        rewritten.push_str(", autonomy: \"suggest\", receipts: ");
        rewritten.push_str(receipts);
        rewritten.push_str(newline);
    }
    if !found {
        return Err("canonical persona source is missing an @persona annotation".to_string());
    }
    Ok(rewritten)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    #[test]
    fn prompt_rendering_templates_declare_their_read_capability() {
        for template in [
            "deterministic-sweeper",
            "hybrid-classify-then-act",
            "frontier-judgment-loop",
        ] {
            let source = PERSONA_TEMPLATE_ASSETS
                .get_file(format!("{template}/src/template_persona.harn"))
                .and_then(include_dir::File::contents_utf8)
                .expect("canonical persona template source");
            let manifest = PERSONA_TEMPLATE_ASSETS
                .get_file(format!("{template}/harn.toml"))
                .and_then(include_dir::File::contents_utf8)
                .expect("canonical persona template manifest");
            let manifest = toml::from_str::<toml::Value>(manifest)
                .expect("canonical persona template manifest is valid TOML");
            let capabilities = manifest["personas"][0]["capabilities"]
                .as_array()
                .expect("canonical persona template capabilities");

            assert!(
                source.contains("render_prompt("),
                "{template} renders a prompt"
            );
            assert!(
                capabilities
                    .iter()
                    .any(|capability| capability.as_str() == Some("workspace.read_text")),
                "{template} must declare workspace.read_text for render_prompt"
            );
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn materialize_compiles_a_closed_blueprint_through_the_strict_scaffold() {
        let temp = tempfile::tempdir().unwrap();
        let blueprint = temp.path().join("blueprint.json");
        let goal = "Keep literal {{not_a_liquid_expression}} text.";
        fs::write(
            &blueprint,
            serde_json::json!({
                "schema_version": "1",
                "name": "reply_digest",
                "description": "Summarizes follow-up work.",
                "goal": goal,
                "template": "deterministic-sweeper",
                "cron": {"cron": "0 */4 * * *", "timezone": "America/Los_Angeles"},
            })
            .to_string(),
        )
        .unwrap();
        let output_root = temp.path().join("personas");

        let result = materialize_persona_package(&blueprint, &output_root, false)
            .await
            .expect("materialize closed blueprint");

        let manifest = toml::from_str::<toml::Value>(
            &fs::read_to_string(result.root.join("harn.toml")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            manifest["package"]["name"].as_str(),
            Some("harn-reply-digest-persona")
        );
        assert_eq!(
            manifest["personas"][0]["autonomy_tier"].as_str(),
            Some("suggest")
        );
        assert_eq!(
            manifest["personas"][0]["receipt_policy"].as_str(),
            Some("required")
        );
        assert!(manifest["personas"][0].get("triggers").is_none());
        assert!(manifest["personas"][0].get("schedules").is_none());
        let triggers = manifest["triggers"].as_array().unwrap();
        assert_eq!(triggers.len(), 1);
        assert_eq!(triggers[0]["kind"].as_str(), Some("cron"));
        assert_eq!(
            triggers[0]["handler"].as_str(),
            Some("persona://reply_digest")
        );
        assert_eq!(triggers[0]["schedule"].as_str(), Some("0 */4 * * *"));

        let source = fs::read_to_string(result.root.join("src/reply_digest.harn")).unwrap();
        assert!(source.contains("persona_goal: \"Keep literal {{not_a_liquid_expression}} text.\""));
        assert!(source.contains("autonomy: \"suggest\""));
        let prompt = fs::read_to_string(result.root.join("prompts/system.harn.prompt")).unwrap();
        assert!(prompt.contains("Goal: {{persona_goal}}"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn materialize_preserves_an_external_trigger_without_cron_fields() {
        let temp = tempfile::tempdir().unwrap();
        let blueprint = temp.path().join("blueprint.json");
        fs::write(
            &blueprint,
            serde_json::json!({
                "schema_version": "1",
                "name": "slack_triage",
                "description": "Classifies Slack follow-up.",
                "goal": "Classify every incoming message.",
                "template": "hybrid-classify-then-act",
                "external": {"provider": "slack", "event": "slack.message"},
            })
            .to_string(),
        )
        .unwrap();

        let result = materialize_persona_package(&blueprint, &temp.path().join("personas"), false)
            .await
            .expect("materialize external blueprint");

        let manifest = toml::from_str::<toml::Value>(
            &fs::read_to_string(result.root.join("harn.toml")).unwrap(),
        )
        .unwrap();
        let triggers = manifest["triggers"].as_array().unwrap();
        assert_eq!(triggers.len(), 1);
        assert_eq!(triggers[0]["provider"].as_str(), Some("slack"));
        assert_eq!(triggers[0]["kind"].as_str(), Some("webhook"));
        assert_eq!(
            triggers[0]["match"]["events"][0].as_str(),
            Some("slack.message")
        );
        assert!(triggers[0].get("schedule").is_none());
        assert!(triggers[0].get("timezone").is_none());
        assert_eq!(
            triggers[0]["secrets"]["signing_secret"].as_str(),
            Some("slack/signing_secret")
        );
        assert_eq!(
            triggers[0]["handler"].as_str(),
            Some("persona://slack_triage")
        );
    }

    #[test]
    fn materializer_rejects_a_foreign_lowering_profile_before_any_publish() {
        let response = r#"{
          "ok": true,
          "lowering": {
            "profile": "foreign_profile",
            "template": "deterministic-sweeper",
            "persona": {"name": "reply_digest", "description": "Digest", "goal": "Goal"},
            "policy": {"autonomy_tier": "suggest", "receipt_policy": "required"},
            "triggers": [{
              "id": "reply_digest-cron",
              "kind": "cron",
              "provider": "cron",
              "events": ["cron.tick"],
              "secrets": {},
              "schedule": "0 * * * *",
              "timezone": "UTC",
              "handler": "persona://reply_digest"
            }]
          },
          "error": null
        }"#;

        assert!(
            serde_json::from_str::<persona_prompt::MaterializeCompileResponse>(response).is_err()
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn strict_validation_rejects_a_missing_entry_symbol_before_publish() {
        let temp = tempfile::tempdir().unwrap();
        let output_root = temp.path().join("personas");
        let prepared = prepare_persona_package(
            &output_root,
            "reviewer",
            PersonaTemplateKind::DeterministicSweeper,
        )
        .unwrap();
        let source_path = prepared.staging.path().join("src").join("reviewer.harn");
        let source = fs::read_to_string(&source_path).unwrap();
        fs::write(
            &source_path,
            source.replace("pipeline run(", "pipeline renamed_run("),
        )
        .unwrap();

        let error = validate_prepared_persona(&prepared, "reviewer")
            .await
            .unwrap_err();

        assert!(error.contains("entry-symbol"), "{error}");
        assert!(!output_root.join("reviewer").exists());
    }

    #[test]
    fn failed_forced_publish_restores_the_previous_package() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("reviewer");
        let staged = temp.path().join(".staged-reviewer");
        fs::create_dir_all(&target).unwrap();
        fs::create_dir_all(&staged).unwrap();
        fs::write(target.join("user-owned.txt"), "keep me").unwrap();
        fs::write(staged.join("generated.txt"), "new package").unwrap();
        let calls = Cell::new(0);

        let error = publish_staged_persona_with(&staged, &target, true, |from, to| {
            let call = calls.get() + 1;
            calls.set(call);
            if call == 2 {
                Err(std::io::Error::other("injected publish failure"))
            } else {
                fs::rename(from, to)
            }
        })
        .unwrap_err();

        assert!(error.contains("restored the previous package"), "{error}");
        assert_eq!(
            fs::read_to_string(target.join("user-owned.txt")).unwrap(),
            "keep me"
        );
        assert!(staged.join("generated.txt").exists());
        assert!(fs::read_dir(temp.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains("harn-persona-backup")
        }));
    }
}
