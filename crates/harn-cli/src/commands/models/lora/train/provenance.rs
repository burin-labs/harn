use std::path::Path;

use super::super::{
    read_trainer_identity_file, refresh_lora_promotion_evidence, trainer_environment_check,
    trainer_identity_check,
};
use super::LoraTrainReport;

/// Finalize Harn-owned provenance after a backend has produced its observations.
pub(super) fn finalize_executed_trainer_provenance(
    report: &mut LoraTrainReport,
) -> Result<(), String> {
    let identity_path = Path::new(&report.backend.trainer_identity_path);
    let observed = if identity_path.exists() {
        let file_observed = read_trainer_identity_file(identity_path)?;
        if let (Some(existing), Some(from_file)) =
            (&report.training.trainer_identity.observed, &file_observed)
        {
            if existing != from_file {
                report.ok = false;
                report.backend.status = "completed_trainer_identity_conflict".to_string();
                return Err(format!(
                    "backend result trainer identity {}={} conflicts with {} identity sidecar {}={}",
                    existing.kind,
                    existing.value,
                    identity_path.display(),
                    from_file.kind,
                    from_file.value
                ));
            }
        }
        file_observed
    } else {
        report.training.trainer_identity.observed.clone()
    };
    report.training.trainer_identity =
        trainer_identity_check(report.training.trainer_identity.expected.clone(), observed);
    let observation = report
        .backend
        .result
        .as_ref()
        .and_then(|result| result.trainer_environment_observation.clone());
    report.training.trainer_environment = trainer_environment_check(
        report.training.trainer_identity.expected.clone(),
        observation,
    );
    refresh_lora_promotion_evidence(
        &mut report.promotion,
        &report.training.trainer_identity,
        &report.training.trainer_environment,
    );
    if !report.training.trainer_identity.promotable
        || !report.training.trainer_environment.promotable
    {
        report.ok = false;
        if report.backend.status == "completed" {
            report.backend.status = "completed_non_promotable".to_string();
        }
        report.warnings.extend(
            report
                .training
                .trainer_identity
                .errors
                .iter()
                .map(|error| format!("trainer identity: {error}")),
        );
        report.warnings.extend(
            report
                .training
                .trainer_environment
                .errors
                .iter()
                .map(|error| format!("trainer environment: {error}")),
        );
    }
    Ok(())
}
