use super::*;

use crate::llm_config::LocalRuntimeStop;

#[test]
fn validation_rejects_incoherent_local_runtime_lifecycle_contract() {
    let mut catalog = artifact();
    let runtime = catalog
        .providers
        .iter_mut()
        .find(|provider| provider.id == "tgi")
        .and_then(|provider| provider.local_runtime.as_mut())
        .expect("TGI is a local runtime provider");
    runtime.stop = Some(LocalRuntimeStop::External);

    let report = validate_artifact(&catalog);
    assert!(
        report
            .errors
            .iter()
            .any(|message| message.contains("incoherent")),
        "expected incoherent local-runtime lifecycle error, got {:?}",
        report.errors
    );
}

#[test]
fn validation_requires_explicit_stop_ownership_for_local_runtimes() {
    let mut catalog = artifact();
    let runtime = catalog
        .providers
        .iter_mut()
        .find(|provider| provider.id == "tgi")
        .and_then(|provider| provider.local_runtime.as_mut())
        .expect("TGI is a local runtime provider");
    runtime.stop = None;

    let report = validate_artifact(&catalog);
    assert!(
        report
            .errors
            .iter()
            .any(|message| message.contains("local_runtime.stop cannot be empty")),
        "expected missing local-runtime stop validation error, got {:?}",
        report.errors
    );
}
