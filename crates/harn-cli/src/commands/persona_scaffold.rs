use std::fs;
use std::path::{Path, PathBuf};

use include_dir::{include_dir, Dir};

use crate::cli::{PersonaNewArgs, PersonaTemplateKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonaScaffoldResult {
    pub root: PathBuf,
    pub files: Vec<PathBuf>,
}

static PERSONA_TEMPLATE_ASSETS: Dir<'_> =
    include_dir!("$CARGO_MANIFEST_DIR/assets/persona-templates");

async fn scaffold_persona(args: &PersonaNewArgs) -> Result<PersonaScaffoldResult, String> {
    let name = normalize_name(&args.name)?;
    let target_root = args.output_root.join(&name);
    if target_root.exists() && !args.force {
        return Err(format!(
            "{} already exists; pass --force to replace it after validation",
            target_root.display()
        ));
    }

    let prepared = prepare_persona_package(&args.output_root, &name, args.template)?;
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
        name: name.to_string(),
        template: parse_template_kind(template)?,
        output_root: output_root.to_path_buf(),
        force,
    };
    scaffold_persona(&args).await
}

pub(crate) async fn run_new(args: &PersonaNewArgs) -> Result<(), String> {
    let result = scaffold_persona(args).await?;
    println!("created persona package {}", result.root.display());
    for file in &result.files {
        println!("  create {}", file.display());
    }
    println!();
    println!("  harn persona doctor {}", normalize_name(&args.name)?);
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

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

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
