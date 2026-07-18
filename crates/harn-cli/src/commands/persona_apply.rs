use std::path::{Path, PathBuf};

use harn_modules::personas::PersonaAutonomyTier;
use serde::Serialize;

use crate::cli::PersonaMaterializeArgs;
use crate::package::{
    self, LocalDependencyInstallReceipt, PackageWorkspace, PersonaActivationReceipt,
    PersonaAttenuation,
};

use super::persona_scaffold::PersonaScaffoldResult;

const APPLY_RECEIPT_SCHEMA: &str = "harn.persona.apply.v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PersonaApplyStage {
    Preflight,
    Materialize,
    Install,
    Doctor,
    Activate,
    Verify,
    Complete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct PersonaApplyMaterialization {
    pub root: String,
    pub files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct PersonaApplyVerification {
    pub persona_id: String,
    pub content_hash: String,
    pub trigger_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct PersonaApplyError {
    pub code: &'static str,
    pub message: String,
    pub retryable: bool,
    pub installed_inert: bool,
    pub activation_present: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct PersonaApplyReceipt {
    pub schema_version: &'static str,
    pub ok: bool,
    pub stage: PersonaApplyStage,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub materialization: Option<PersonaApplyMaterialization>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub install: Option<LocalDependencyInstallReceipt>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activation: Option<PersonaActivationReceipt>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification: Option<PersonaApplyVerification>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<PersonaApplyError>,
}

impl Default for PersonaApplyReceipt {
    fn default() -> Self {
        Self {
            schema_version: APPLY_RECEIPT_SCHEMA,
            ok: false,
            stage: PersonaApplyStage::Preflight,
            materialization: None,
            install: None,
            activation: None,
            verification: None,
            error: None,
        }
    }
}

pub(crate) async fn run(
    manifest: Option<&Path>,
    args: &PersonaMaterializeArgs,
) -> Result<(), String> {
    let receipt = apply_reviewed_persona(manifest, args).await;
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&receipt).map_err(|error| error.to_string())?
        );
    } else if receipt.ok {
        let persona_id = receipt
            .verification
            .as_ref()
            .map(|verification| verification.persona_id.as_str())
            .ok_or_else(|| "successful persona apply has no verification receipt".to_string())?;
        println!("activated persona {persona_id}");
    }
    if receipt.ok {
        Ok(())
    } else {
        Err(receipt
            .error
            .as_ref()
            .map(|error| error.message.clone())
            .unwrap_or_else(|| "persona apply failed".to_string()))
    }
}

pub(crate) async fn apply_reviewed_persona(
    manifest: Option<&Path>,
    args: &PersonaMaterializeArgs,
) -> PersonaApplyReceipt {
    let mut receipt = PersonaApplyReceipt::default();
    let Some(manifest) = manifest else {
        return failed(
            receipt,
            PersonaApplyStage::Preflight,
            "manifest_required",
            "persona apply requires an explicit --manifest project path".to_string(),
            false,
        );
    };
    if args.compile_receipt.is_none() || args.blueprint.is_some() {
        return failed(
            receipt,
            PersonaApplyStage::Preflight,
            "reviewed_receipt_required",
            "persona apply accepts only --compile-receipt".to_string(),
            false,
        );
    }
    let project = match package::load_manifest_context_for_anchor(Some(manifest)) {
        Ok(project) => project,
        Err(error) => {
            return failed(
                receipt,
                PersonaApplyStage::Preflight,
                "project_invalid",
                error.to_string(),
                false,
            );
        }
    };
    let output_root = resolve_output_root(&project.dir, &args.output_root);
    let materialized = match Box::pin(super::persona_scaffold::materialize_persona_for_apply(
        args,
        &output_root,
    ))
    .await
    {
        Ok(materialized) => materialized,
        Err(error) => {
            return failed(
                receipt,
                PersonaApplyStage::Materialize,
                "materialization_failed",
                error,
                true,
            );
        }
    };
    receipt.stage = PersonaApplyStage::Materialize;
    receipt.materialization = Some(materialization_receipt(&materialized));

    let persona_name = match materialized_persona_name(&materialized.root) {
        Ok(name) => name,
        Err(error) => {
            return failed(
                receipt,
                PersonaApplyStage::Materialize,
                "persona_identity_invalid",
                error,
                true,
            );
        }
    };
    let workspace = PackageWorkspace::from_manifest_dir(&project.dir);
    let install = match package::install_local_package(&workspace, &materialized.root) {
        Ok(install) => install,
        Err(error) => {
            return failed(
                receipt,
                PersonaApplyStage::Install,
                "install_failed",
                error.to_string(),
                true,
            );
        }
    };
    let persona_id = format!("{}/{}", install.alias, persona_name);
    receipt.stage = PersonaApplyStage::Install;
    receipt.install = Some(install);

    match package::doctor_packages_in(&workspace) {
        Ok(report) if report.ok => receipt.stage = PersonaApplyStage::Doctor,
        Ok(report) => {
            let message = report
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.severity == "error")
                .map(|diagnostic| format!("{}: {}", diagnostic.code, diagnostic.message))
                .collect::<Vec<_>>()
                .join("; ");
            return failed(
                receipt,
                PersonaApplyStage::Doctor,
                "package_doctor_failed",
                message,
                true,
            );
        }
        Err(error) => {
            return failed(
                receipt,
                PersonaApplyStage::Doctor,
                "package_doctor_failed",
                error.to_string(),
                true,
            );
        }
    }

    let activation = match package::activate_persona(
        Some(&project.manifest_path()),
        &persona_id,
        &PersonaAttenuation {
            autonomy_tier: Some(PersonaAutonomyTier::Suggest),
            ..PersonaAttenuation::default()
        },
        harn_vm::persona_now_ms(),
    ) {
        Ok(activation) => activation,
        Err(error) => {
            return failed(
                receipt,
                PersonaApplyStage::Activate,
                "activation_failed",
                error.to_string(),
                true,
            );
        }
    };
    receipt.stage = PersonaApplyStage::Activate;
    receipt.activation = Some(activation);

    match verify_apply(&project.manifest_path(), &persona_id, &receipt) {
        Ok(verification) => {
            receipt.ok = true;
            receipt.stage = PersonaApplyStage::Complete;
            receipt.verification = Some(verification);
            receipt
        }
        Err(error) => failed(
            receipt,
            PersonaApplyStage::Verify,
            "verification_failed",
            error,
            false,
        ),
    }
}

fn verify_apply(
    manifest_path: &Path,
    persona_id: &str,
    receipt: &PersonaApplyReceipt,
) -> Result<PersonaApplyVerification, String> {
    let discovered = package::resolve_discoverable_persona(Some(manifest_path), persona_id)?;
    let expected_activation = receipt
        .activation
        .as_ref()
        .and_then(|activation| activation.activation.as_ref())
        .ok_or_else(|| "activation receipt has no pinned activation".to_string())?;
    let activations = package::list_persona_activations(Some(manifest_path))
        .map_err(|error| error.to_string())?;
    let activation = activations
        .iter()
        .find(|activation| activation.persona_id == persona_id)
        .ok_or_else(|| format!("activation ledger has no record for {persona_id}"))?;
    if activation != expected_activation {
        return Err(format!("activation ledger record for {persona_id} drifted"));
    }
    let extensions =
        package::try_load_runtime_extensions(manifest_path).map_err(|error| error.to_string())?;
    if !extensions
        .runtime_personas
        .iter()
        .any(|persona| persona.id == discovered.id)
    {
        return Err(format!(
            "runtime did not load activated persona {persona_id}"
        ));
    }
    let handler = format!("persona://{persona_id}");
    let mut trigger_ids = extensions
        .triggers
        .iter()
        .filter(|trigger| trigger.handler == handler)
        .map(|trigger| trigger.id.clone())
        .collect::<Vec<_>>();
    trigger_ids.sort();
    if trigger_ids.is_empty() {
        return Err(format!(
            "runtime found no trigger for activated persona {persona_id}"
        ));
    }
    Ok(PersonaApplyVerification {
        persona_id: persona_id.to_string(),
        content_hash: activation.package.content_hash.clone(),
        trigger_ids,
    })
}

fn failed(
    mut receipt: PersonaApplyReceipt,
    stage: PersonaApplyStage,
    code: &'static str,
    message: String,
    retryable: bool,
) -> PersonaApplyReceipt {
    receipt.ok = false;
    receipt.stage = stage;
    receipt.error = Some(PersonaApplyError {
        code,
        message,
        retryable,
        installed_inert: receipt.install.is_some() && receipt.activation.is_none(),
        activation_present: receipt.activation.is_some(),
    });
    receipt
}

fn resolve_output_root(project_root: &Path, output_root: &Path) -> PathBuf {
    if output_root.is_absolute() {
        output_root.to_path_buf()
    } else {
        project_root.join(output_root)
    }
}

fn materialization_receipt(result: &PersonaScaffoldResult) -> PersonaApplyMaterialization {
    PersonaApplyMaterialization {
        root: result.root.display().to_string(),
        files: result
            .files
            .iter()
            .map(|path| path.display().to_string())
            .collect(),
    }
}

fn materialized_persona_name(package_root: &Path) -> Result<String, String> {
    let catalog = package::load_personas_from_manifest_path(package_root).map_err(|errors| {
        errors
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ")
    })?;
    let names = catalog
        .personas
        .iter()
        .filter_map(|persona| persona.name.clone())
        .collect::<Vec<_>>();
    match names.as_slice() {
        [name] => Ok(name.clone()),
        _ => Err(format!(
            "materialized package must export exactly one persona, found {}",
            names.len()
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn apply_fixture(root: &Path) -> (PathBuf, PersonaMaterializeArgs) {
        fs::create_dir_all(root).unwrap();
        let manifest = root.join("harn.toml");
        fs::write(
            &manifest,
            "[package]\nname = \"consumer\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let compile_receipt = root.join("reviewed-receipt.json");
        fs::write(
            &compile_receipt,
            crate::commands::persona_test_support::reviewed_compile_receipt().to_string(),
        )
        .unwrap();
        (
            manifest,
            PersonaMaterializeArgs {
                blueprint: None,
                compile_receipt: Some(compile_receipt),
                output_root: PathBuf::from("personas"),
                force: false,
                activate: true,
                json: true,
            },
        )
    }

    #[tokio::test(flavor = "current_thread")]
    async fn reviewed_receipt_applies_idempotently_and_projects_its_trigger() {
        let temp = tempfile::tempdir().unwrap();
        let (manifest, args) = apply_fixture(temp.path());

        let first = apply_reviewed_persona(Some(&manifest), &args).await;
        assert!(first.ok, "{:#?}", first.error);
        assert_eq!(first.stage, PersonaApplyStage::Complete);
        let first_install = first.install.as_ref().unwrap();
        assert_eq!(first_install.alias, "harn-accepted-prompt-watch-persona");
        assert_eq!(first_install.path, "personas/accepted_prompt_watch");
        assert!(first_install.manifest_changed);
        assert!(first_install.generation_changed);
        let first_activation = first.activation.as_ref().unwrap();
        assert!(first_activation.changed);
        let first_verification = first.verification.as_ref().unwrap();
        assert_eq!(
            first_verification.persona_id,
            "harn-accepted-prompt-watch-persona/accepted_prompt_watch"
        );
        assert_eq!(
            first_verification.trigger_ids,
            vec!["harn-accepted-prompt-watch-persona/accepted_prompt_watch-cron"]
        );
        let manifest_after_first = fs::read(&manifest).unwrap();

        let second = apply_reviewed_persona(Some(&manifest), &args).await;
        assert!(second.ok, "{:#?}", second.error);
        let second_install = second.install.as_ref().unwrap();
        assert_eq!(second_install.alias, first_install.alias);
        assert!(!second_install.manifest_changed);
        assert!(!second_install.generation_changed);
        assert_eq!(second_install.generation, first_install.generation);
        assert!(!second.activation.as_ref().unwrap().changed);
        assert_eq!(fs::read(&manifest).unwrap(), manifest_after_first);

        let extensions = package::try_load_runtime_extensions(&manifest).unwrap();
        let handler = format!("persona://{}", first_verification.persona_id);
        let trigger = extensions
            .triggers
            .iter()
            .find(|trigger| trigger.handler == handler)
            .unwrap();
        assert_eq!(
            trigger.kind_specific.get("schedule"),
            Some(&toml::Value::String("0 9 * * *".to_string()))
        );
        assert_eq!(
            trigger.kind_specific.get("timezone"),
            Some(&toml::Value::String("UTC".to_string()))
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn apply_requires_an_explicit_project_manifest() {
        let temp = tempfile::tempdir().unwrap();
        let (_, args) = apply_fixture(temp.path());

        let receipt = apply_reviewed_persona(None, &args).await;

        assert!(!receipt.ok);
        assert_eq!(receipt.stage, PersonaApplyStage::Preflight);
        assert!(receipt.materialization.is_none());
        assert!(receipt.install.is_none());
        assert_eq!(receipt.error.unwrap().code, "manifest_required");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn activation_failure_reports_the_installed_package_as_inert() {
        let temp = tempfile::tempdir().unwrap();
        let (manifest, args) = apply_fixture(temp.path());
        let ledger = temp.path().join(".harn/personas/activations.json");
        fs::create_dir_all(ledger.parent().unwrap()).unwrap();
        fs::write(&ledger, "{\"schema_version\":99,\"activations\":{}}\n").unwrap();

        let receipt = apply_reviewed_persona(Some(&manifest), &args).await;

        assert!(!receipt.ok);
        assert_eq!(receipt.stage, PersonaApplyStage::Activate);
        assert!(receipt.install.is_some());
        assert!(receipt.activation.is_none());
        let error = receipt.error.unwrap();
        assert_eq!(error.code, "activation_failed");
        assert!(error.retryable);
        assert!(error.installed_inert);
        assert!(!error.activation_present);
        let ledger_value: serde_json::Value =
            serde_json::from_slice(&fs::read(&ledger).unwrap()).unwrap();
        assert_eq!(ledger_value["activations"], serde_json::json!({}));
    }

    #[test]
    fn local_dependency_alias_never_overwrites_an_unrelated_dependency() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let unrelated = root.join("unrelated");
        let package = root.join("generated");
        fs::create_dir_all(&unrelated).unwrap();
        fs::create_dir_all(&package).unwrap();
        fs::write(
            unrelated.join("harn.toml"),
            "[package]\nname = \"unrelated\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        fs::write(
            package.join("harn.toml"),
            "[package]\nname = \"generated-persona\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        fs::write(
            root.join("harn.toml"),
            "[package]\nname = \"consumer\"\nversion = \"0.1.0\"\n\n[dependencies]\ngenerated-persona = { path = \"unrelated\" }\n",
        )
        .unwrap();
        let workspace = PackageWorkspace::from_manifest_dir(root);

        let first = package::install_local_package(&workspace, &package).unwrap();
        let second = package::install_local_package(&workspace, &package).unwrap();

        assert!(first.alias.starts_with("generated-persona-"));
        assert_eq!(second.alias, first.alias);
        assert!(!second.manifest_changed);
        assert!(!second.generation_changed);
        let manifest = fs::read_to_string(root.join("harn.toml")).unwrap();
        assert!(manifest.contains("generated-persona = { path = \"unrelated\" }"));
        assert!(manifest.contains(&format!("{} = {{ path = \"generated\" }}", first.alias)));
    }
}
