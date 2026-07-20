use std::path::{Component, Path, PathBuf};

use harn_modules::personas::PersonaAutonomyTier;
use serde::Serialize;

use crate::cli::PersonaMaterializeArgs;
use crate::package::{
    self, LocalDependencyInstall, LocalDependencyInstallReceipt, PackageWorkspace,
    PersonaActivationReceipt, PersonaAttenuation,
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
    apply_reviewed_persona_with_verifier(manifest, args, verify_apply).await
}

async fn apply_reviewed_persona_with_verifier(
    manifest: Option<&Path>,
    args: &PersonaMaterializeArgs,
    verifier: impl FnOnce(&Path, &str, &PersonaApplyReceipt) -> Result<PersonaApplyVerification, String>,
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
    let output_root = match resolve_output_root(&project.dir, &args.output_root) {
        Ok(output_root) => output_root,
        Err(error) => {
            return failed(
                receipt,
                PersonaApplyStage::Preflight,
                "output_root_invalid",
                error,
                false,
            );
        }
    };
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
    let mutation_lock = match package::acquire_project_mutation_lock(&project.dir) {
        Ok(lock) => lock,
        Err(error) => {
            return failed(
                receipt,
                PersonaApplyStage::Install,
                "mutation_lock_failed",
                error.to_string(),
                true,
            );
        }
    };
    let install_transaction =
        match package::install_local_package_locked(&workspace, &materialized.root, &mutation_lock)
        {
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
    let persona_id = format!("{}/{}", install_transaction.receipt().alias, persona_name);
    receipt.stage = PersonaApplyStage::Install;
    receipt.install = Some(install_transaction.receipt().clone());

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
            return failed_with_install_rollback(
                receipt,
                install_transaction,
                PersonaApplyStage::Doctor,
                "package_doctor_failed",
                message,
                true,
            );
        }
        Err(error) => {
            return failed_with_install_rollback(
                receipt,
                install_transaction,
                PersonaApplyStage::Doctor,
                "package_doctor_failed",
                error.to_string(),
                true,
            );
        }
    }

    let (activation, previous_activation) = match package::activate_persona_with_previous_locked(
        Some(&project.manifest_path()),
        &persona_id,
        &PersonaAttenuation {
            autonomy_tier: Some(PersonaAutonomyTier::Suggest),
            ..PersonaAttenuation::default()
        },
        harn_vm::persona_now_ms(),
        &mutation_lock,
    ) {
        Ok(transaction) => transaction,
        Err(error) => {
            return failed_with_install_rollback(
                receipt,
                install_transaction,
                PersonaApplyStage::Activate,
                "activation_failed",
                error.to_string(),
                true,
            );
        }
    };
    receipt.stage = PersonaApplyStage::Activate;
    receipt.activation = Some(activation);

    match verifier(&project.manifest_path(), &persona_id, &receipt) {
        Ok(verification) => {
            let committed = install_transaction.commit();
            receipt.install = Some(committed);
            receipt.ok = true;
            receipt.stage = PersonaApplyStage::Complete;
            receipt.verification = Some(verification);
            receipt
        }
        Err(error) => {
            if receipt
                .activation
                .as_ref()
                .is_some_and(|activation| activation.changed)
            {
                let expected = receipt
                    .activation
                    .as_ref()
                    .and_then(|activation| activation.activation.as_ref())
                    .expect("changed activation receipt must contain its record");
                if let Err(activation_rollback_error) = package::restore_persona_activation_locked(
                    Some(&project.manifest_path()),
                    expected,
                    previous_activation.clone(),
                    &mutation_lock,
                ) {
                    return failed(
                        receipt,
                        PersonaApplyStage::Verify,
                        "verification_rollback_failed",
                        format!(
                            "persona verification failed: {error}; activation rollback failed: {activation_rollback_error}"
                        ),
                        false,
                    );
                }
                if previous_activation.is_none() {
                    receipt.activation = None;
                }
            }
            failed_with_install_rollback(
                receipt,
                install_transaction,
                PersonaApplyStage::Verify,
                "verification_failed",
                error,
                false,
            )
        }
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
    let resolved_persona = extensions
        .runtime_personas
        .iter()
        .find(|persona| persona.id == discovered.id)
        .ok_or_else(|| format!("runtime did not load activated persona {persona_id}"))?;
    if resolved_persona.execution_guard.is_none()
        || resolved_persona.manifest_path != discovered.manifest_path
    {
        return Err(format!(
            "runtime persona {persona_id} lacks installed-package execution provenance"
        ));
    }
    let handler = format!("persona://{persona_id}");
    let mut expected_trigger_ids =
        package::installed_persona_trigger_configs(std::slice::from_ref(resolved_persona))
            .map_err(|error| error.to_string())?
            .into_iter()
            .map(|trigger| trigger.id)
            .collect::<Vec<_>>();
    expected_trigger_ids.sort();
    expected_trigger_ids.dedup();
    if expected_trigger_ids.is_empty() {
        return Err(format!(
            "installed persona package defines no trigger for {persona_id}"
        ));
    }
    let trigger_ids = verified_projected_trigger_ids(
        &extensions.triggers,
        &handler,
        &discovered.manifest_path,
        &expected_trigger_ids,
    )?;
    Ok(PersonaApplyVerification {
        persona_id: persona_id.to_string(),
        content_hash: activation.package.content_hash.clone(),
        trigger_ids,
    })
}

fn verified_projected_trigger_ids(
    triggers: &[package::ResolvedTriggerConfig],
    handler: &str,
    manifest_path: &Path,
    expected_trigger_ids: &[String],
) -> Result<Vec<String>, String> {
    let mut actual = triggers
        .iter()
        .filter(|trigger| {
            trigger.handler == handler
                && trigger.execution_guard.is_some()
                && trigger.manifest_path == manifest_path
        })
        .map(|trigger| trigger.id.clone())
        .collect::<Vec<_>>();
    actual.sort();
    actual.dedup();
    if actual != expected_trigger_ids {
        return Err(format!(
            "runtime trigger projection for {handler} is incomplete: expected {expected_trigger_ids:?}, found {actual:?}"
        ));
    }
    Ok(actual)
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

fn failed_with_install_rollback(
    receipt: PersonaApplyReceipt,
    install: LocalDependencyInstall,
    stage: PersonaApplyStage,
    code: &'static str,
    message: String,
    retryable: bool,
) -> PersonaApplyReceipt {
    match install.rollback() {
        Ok(_) => {
            let mut receipt = failed(receipt, stage, code, message, retryable);
            if let Some(error) = receipt.error.as_mut() {
                error.installed_inert = false;
            }
            receipt
        }
        Err(rollback_error) => failed(
            receipt,
            stage,
            "install_rollback_failed",
            format!("{message}; local package rollback failed: {rollback_error}"),
            false,
        ),
    }
}

fn resolve_output_root(project_root: &Path, output_root: &Path) -> Result<PathBuf, String> {
    if output_root.is_absolute() {
        return Ok(output_root.to_path_buf());
    }
    if output_root.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err(format!(
            "relative persona output root {} escapes the selected project",
            output_root.display()
        ));
    }

    let canonical_project = project_root.canonicalize().map_err(|error| {
        format!(
            "failed to canonicalize selected project {}: {error}",
            project_root.display()
        )
    })?;
    let target = canonical_project.join(output_root);
    let mut ancestor = target.as_path();
    while !ancestor.exists() {
        ancestor = ancestor.parent().ok_or_else(|| {
            format!(
                "persona output root {} has no existing ancestor",
                target.display()
            )
        })?;
    }
    let canonical_ancestor = ancestor.canonicalize().map_err(|error| {
        format!(
            "failed to canonicalize persona output ancestor {}: {error}",
            ancestor.display()
        )
    })?;
    if !canonical_ancestor.starts_with(&canonical_project) {
        return Err(format!(
            "relative persona output root {} resolves outside the selected project",
            output_root.display()
        ));
    }
    Ok(target)
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
    use std::future::Future;
    use std::sync::{mpsc, Arc, Mutex};
    use std::thread;

    use harn_modules::package_snapshot::PackageSnapshot;

    use super::*;

    /// Persona apply materializes through compile + doctor paths that drive the
    /// Harn VM. That work needs more than libtest's default 2 MiB worker stack
    /// and can otherwise SIGABRT the whole `harn-cli` test binary (#5250).
    fn block_on_cli_stack_async<Fut>(build: impl FnOnce() -> Fut + Send + 'static) -> Fut::Output
    where
        Fut: Future + 'static,
        Fut::Output: Send + 'static,
    {
        thread::Builder::new()
            .name("persona-apply-test".into())
            .stack_size(crate::CLI_RUNTIME_STACK_SIZE)
            .spawn(move || {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("persona-apply test runtime")
                    .block_on(build())
            })
            .expect("failed to spawn persona-apply test thread")
            .join()
            .expect("persona-apply test thread panicked")
    }

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
    async fn concurrent_identical_apply_waits_for_failed_transaction_then_succeeds() {
        let temp = tempfile::tempdir().unwrap();
        let (manifest, first_args) = apply_fixture(temp.path());
        let second_args = PersonaMaterializeArgs {
            blueprint: first_args.blueprint.clone(),
            compile_receipt: first_args.compile_receipt.clone(),
            output_root: first_args.output_root.clone(),
            force: first_args.force,
            activate: first_args.activate,
            json: first_args.json,
        };
        let (attempted_tx, attempted_rx) = mpsc::channel();
        let second_handle = Arc::new(Mutex::new(None));
        let second_handle_for_verifier = Arc::clone(&second_handle);
        let second_manifest = manifest.clone();

        let first =
            apply_reviewed_persona_with_verifier(Some(&manifest), &first_args, move |_, _, _| {
                let handle = thread::spawn(move || {
                    package::project_mutation_lock_test_probe::install(move || {
                        attempted_tx.send(()).unwrap();
                    });
                    tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap()
                        .block_on(apply_reviewed_persona(Some(&second_manifest), &second_args))
                });
                *second_handle_for_verifier.lock().unwrap() = Some(handle);
                attempted_rx.recv().unwrap();
                Err("forced verification failure".to_string())
            })
            .await;
        assert!(!first.ok);
        assert_eq!(first.stage, PersonaApplyStage::Verify);

        let second = second_handle
            .lock()
            .unwrap()
            .take()
            .unwrap()
            .join()
            .unwrap();
        assert!(second.ok, "{:#?}", second.error);
        let persona_id = second.verification.unwrap().persona_id;
        assert!(package::list_persona_activations(Some(&manifest))
            .unwrap()
            .iter()
            .any(|activation| activation.persona_id == persona_id));
        let project = package::load_manifest_context_for_anchor(Some(&manifest)).unwrap();
        assert!(project
            .manifest
            .dependencies
            .contains_key("harn-accepted-prompt-watch-persona"));
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
    async fn apply_rejects_relative_output_root_traversal_before_materialization() {
        let temp = tempfile::tempdir().unwrap();
        let (manifest, mut args) = apply_fixture(temp.path());
        args.output_root = PathBuf::from("../outside");

        let receipt = apply_reviewed_persona(Some(&manifest), &args).await;

        assert!(!receipt.ok);
        assert_eq!(receipt.stage, PersonaApplyStage::Preflight);
        assert!(receipt.materialization.is_none());
        assert!(receipt.install.is_none());
        assert_eq!(receipt.error.unwrap().code, "output_root_invalid");
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn apply_rejects_relative_output_root_symlink_escape() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let (manifest, mut args) = apply_fixture(temp.path());
        symlink(outside.path(), temp.path().join("linked-personas")).unwrap();
        args.output_root = PathBuf::from("linked-personas");

        let receipt = apply_reviewed_persona(Some(&manifest), &args).await;

        assert!(!receipt.ok);
        assert_eq!(receipt.stage, PersonaApplyStage::Preflight);
        assert!(receipt.materialization.is_none());
        assert_eq!(receipt.error.unwrap().code, "output_root_invalid");
        assert!(!outside.path().join("accepted_prompt_watch").exists());
    }

    #[test]
    fn activation_failure_rolls_back_the_local_package_install() {
        block_on_cli_stack_async(|| async {
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
            assert!(!error.installed_inert);
            assert!(!error.activation_present);
            assert!(!fs::read_to_string(&manifest)
                .unwrap()
                .contains("accepted_prompt_watch ="));
            assert!(temp.path().join("harn.lock").exists());
            assert!(PackageSnapshot::acquire(temp.path())
                .unwrap()
                .unwrap()
                .package_names()
                .is_empty());
            let ledger_value: serde_json::Value =
                serde_json::from_slice(&fs::read(&ledger).unwrap()).unwrap();
            assert_eq!(ledger_value["activations"], serde_json::json!({}));
        });
    }

    #[tokio::test(flavor = "current_thread")]
    async fn verification_failure_restores_the_exact_prior_activation() {
        let temp = tempfile::tempdir().unwrap();
        let (manifest, args) = apply_fixture(temp.path());
        let first = apply_reviewed_persona(Some(&manifest), &args).await;
        assert!(first.ok, "{:#?}", first.error);
        let persona_id = first.verification.unwrap().persona_id;

        package::activate_persona(
            Some(&manifest),
            &persona_id,
            &PersonaAttenuation {
                autonomy_tier: Some(PersonaAutonomyTier::Shadow),
                ..PersonaAttenuation::default()
            },
            123_456,
        )
        .unwrap();
        let ledger = package::activation_ledger_path(temp.path());
        let ledger_before = fs::read(&ledger).unwrap();
        let activation_before = package::list_persona_activations(Some(&manifest)).unwrap();

        let failed = apply_reviewed_persona_with_verifier(Some(&manifest), &args, |_, _, _| {
            Err("forced verification failure".to_string())
        })
        .await;

        assert!(!failed.ok);
        assert_eq!(failed.stage, PersonaApplyStage::Verify);
        assert_eq!(failed.error.unwrap().code, "verification_failed");
        assert_eq!(fs::read(&ledger).unwrap(), ledger_before);
        assert_eq!(
            package::list_persona_activations(Some(&manifest)).unwrap(),
            activation_before
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn activation_rollback_refuses_to_clobber_a_newer_record() {
        let temp = tempfile::tempdir().unwrap();
        let (manifest, args) = apply_fixture(temp.path());
        let applied = apply_reviewed_persona(Some(&manifest), &args).await;
        assert!(applied.ok, "{:#?}", applied.error);
        let expected = applied
            .activation
            .unwrap()
            .activation
            .expect("successful apply activation record");

        package::activate_persona(
            Some(&manifest),
            &expected.persona_id,
            &PersonaAttenuation {
                autonomy_tier: Some(PersonaAutonomyTier::Shadow),
                ..PersonaAttenuation::default()
            },
            234_567,
        )
        .unwrap();
        let newer = package::list_persona_activations(Some(&manifest)).unwrap();

        let error = package::restore_persona_activation(Some(&manifest), &expected, None)
            .expect_err("rollback must compare-and-swap the expected activation");

        assert!(matches!(
            error,
            package::PersonaActivationError::RollbackConflict { .. }
        ));
        assert_eq!(
            package::list_persona_activations(Some(&manifest)).unwrap(),
            newer
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn activation_mutation_returns_the_atomic_replaced_record() {
        let temp = tempfile::tempdir().unwrap();
        let (manifest, args) = apply_fixture(temp.path());
        let applied = apply_reviewed_persona(Some(&manifest), &args).await;
        assert!(applied.ok, "{:#?}", applied.error);
        let persona_id = applied.verification.unwrap().persona_id;
        let stale_observation = package::list_persona_activations(Some(&manifest))
            .unwrap()
            .pop()
            .unwrap();

        package::activate_persona(
            Some(&manifest),
            &persona_id,
            &PersonaAttenuation {
                autonomy_tier: Some(PersonaAutonomyTier::Shadow),
                ..PersonaAttenuation::default()
            },
            345_678,
        )
        .unwrap();
        let interleaved = package::list_persona_activations(Some(&manifest))
            .unwrap()
            .pop()
            .unwrap();
        let (replacement, previous) = package::activate_persona_with_previous(
            Some(&manifest),
            &persona_id,
            &PersonaAttenuation {
                autonomy_tier: Some(PersonaAutonomyTier::Suggest),
                ..PersonaAttenuation::default()
            },
            456_789,
        )
        .unwrap();

        assert_ne!(interleaved, stale_observation);
        assert_eq!(previous.as_ref(), Some(&interleaved));
        package::restore_persona_activation(
            Some(&manifest),
            replacement.activation.as_ref().unwrap(),
            previous,
        )
        .unwrap();
        assert_eq!(
            package::list_persona_activations(Some(&manifest)).unwrap(),
            vec![interleaved]
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn trigger_verification_requires_guarded_complete_package_projection() {
        let temp = tempfile::tempdir().unwrap();
        let (manifest, args) = apply_fixture(temp.path());
        let applied = apply_reviewed_persona(Some(&manifest), &args).await;
        assert!(applied.ok, "{:#?}", applied.error);
        let verification = applied.verification.unwrap();
        let extensions = package::try_load_runtime_extensions(&manifest).unwrap();
        let installed = extensions
            .triggers
            .iter()
            .find(|trigger| verification.trigger_ids.contains(&trigger.id))
            .unwrap()
            .clone();
        let handler = format!("persona://{}", verification.persona_id);

        let mut root_trigger = installed.clone();
        root_trigger.execution_guard = None;
        root_trigger.manifest_path.clone_from(&manifest);
        assert!(verified_projected_trigger_ids(
            &[root_trigger],
            &handler,
            &installed.manifest_path,
            &verification.trigger_ids,
        )
        .is_err());

        let mut two_expected = verification.trigger_ids.clone();
        two_expected.push("missing/second-trigger".to_string());
        two_expected.sort();
        assert!(verified_projected_trigger_ids(
            &[installed],
            &handler,
            &extensions
                .runtime_personas
                .iter()
                .find(|persona| persona.id == verification.persona_id)
                .unwrap()
                .manifest_path,
            &two_expected,
        )
        .is_err());
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

        let first = package::install_local_package(&workspace, &package)
            .unwrap()
            .commit();
        let second = package::install_local_package(&workspace, &package)
            .unwrap()
            .commit();

        assert!(first.alias.starts_with("generated-persona-"));
        assert_eq!(second.alias, first.alias);
        assert!(!second.manifest_changed);
        assert!(!second.generation_changed);
        let manifest = fs::read_to_string(root.join("harn.toml")).unwrap();
        assert!(manifest.contains("generated-persona = { path = \"unrelated\" }"));
        assert!(manifest.contains(&format!("{} = {{ path = \"generated\" }}", first.alias)));
    }
}
