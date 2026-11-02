//! The CLI projection of the VM's value-free inference admission preview.

use harn_vm::llm::api::{preflight_inference_admission, InferenceAdmissionRequest};

use crate::cli::{ProviderAdmissionArgs, ProviderAttestAccountArgs};

pub(crate) fn run(args: ProviderAdmissionArgs) -> Result<(), String> {
    let _environment = crate::commands::run::environment::process_host_environment_scope()?;
    let request: InferenceAdmissionRequest = serde_json::from_str(&args.request)
        .map_err(|_| "inference admission request is malformed".to_string())?;
    let snapshot = match preflight_inference_admission(&request) {
        Ok(snapshot) => snapshot,
        Err(reason) => {
            println!("{}", serde_json::json!({"configuration_error": reason}));
            return Err(reason.to_string());
        }
    };
    let output = serde_json::to_string(&snapshot)
        .map_err(|_| "inference admission snapshot could not be encoded".to_string())?;
    println!("{output}");
    Ok(())
}

/// The caller obtains the user's confirmation; the credential never enters argv.
pub(crate) fn attest(args: ProviderAttestAccountArgs) -> Result<(), String> {
    let _environment = crate::commands::run::environment::process_host_environment_scope()?;
    let credential = harn_vm::llm::resolve_api_key(&args.provider)
        .map_err(|_| "could not resolve the selected provider credential".to_string())?;
    let attestation = harn_vm::llm::api::ProviderAccountAttestation::new(
        &args.provider,
        &credential,
        args.billing_enabled,
        args.contributor_sharing_enabled,
    )
    .ok_or_else(|| "provider and credential must be present".to_string())?;
    println!(
        "{}",
        serde_json::to_string(&attestation)
            .map_err(|_| "could not encode provider account confirmation".to_string())?
    );
    Ok(())
}
