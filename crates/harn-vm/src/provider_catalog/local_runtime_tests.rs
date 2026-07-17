use super::*;

use crate::llm_config::{LocalRuntimeKind, LocalRuntimeStop};

#[test]
fn tgi_catalog_row_has_data_driven_local_runtime_metadata() {
    let catalog = artifact();
    let runtime = catalog
        .providers
        .iter()
        .find(|provider| provider.id == "tgi")
        .and_then(|provider| provider.local_runtime.as_ref())
        .expect("TGI is a local runtime provider");

    assert_eq!(runtime.kind, Some(LocalRuntimeKind::ManagedProcess));
    assert_eq!(runtime.command.as_deref(), Some("text-generation-launcher"));
    assert_eq!(runtime.model_source_env.as_deref(), Some("TGI_MODEL"));
    assert_eq!(runtime.default_port, Some(8080));
    assert_eq!(runtime.model_arg.as_deref(), Some("--model-id"));
    assert_eq!(runtime.host_arg.as_deref(), Some("--hostname"));
    assert_eq!(runtime.port_arg.as_deref(), Some("--port"));
    assert_eq!(runtime.stop, Some(LocalRuntimeStop::Pid));
    assert_eq!(
        runtime.source_url.as_deref(),
        Some("https://huggingface.co/docs/text-generation-inference/en/basic_tutorials/using_cli")
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
