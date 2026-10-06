//! The CLI projection of the VM's value-free inference admission preview.

use harn_vm::llm::api::{preview_inference_admission, InferenceAdmissionRequest};

use crate::cli::ProviderAdmissionArgs;

pub(crate) fn run(args: ProviderAdmissionArgs) -> Result<(), String> {
    let request: InferenceAdmissionRequest = serde_json::from_str(&args.request)
        .map_err(|_| "inference admission request is malformed".to_string())?;
    let snapshot = preview_inference_admission(&request);
    let output = serde_json::to_string(&snapshot)
        .map_err(|_| "inference admission snapshot could not be encoded".to_string())?;
    println!("{output}");
    Ok(())
}
