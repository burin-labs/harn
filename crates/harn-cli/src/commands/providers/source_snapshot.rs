use std::path::Path;

use harn_vm::{llm::capabilities, provider_catalog};

use super::build::{generated_provider_capabilities, generated_provider_config};

/// Explicit source inputs shared by catalog documentation renderers.
/// Missing sources mean an installed CLI; present but invalid sources are errors.
pub(crate) struct ProviderSourceSnapshot {
    pub catalog: provider_catalog::ProviderCatalogArtifact,
    pub capabilities: capabilities::CapabilitiesFile,
}

pub(crate) fn load_source_snapshot() -> Result<ProviderSourceSnapshot, String> {
    let root = Path::new("crates/harn-vm/src/llm");
    let catalog_source = root.join("catalog_sources");
    let capability_source = root.join("capability_sources");
    let present = |path: &Path| {
        path.try_exists()
            .map_err(|error| format!("cannot inspect {}: {error}", path.display()))
    };
    let config = if present(&catalog_source)? {
        generated_provider_config(&catalog_source)?.config
    } else {
        harn_vm::llm_config::embedded_config(None)
    };
    let capabilities = if present(&capability_source)? {
        generated_provider_capabilities(&capability_source)?.capabilities
    } else {
        capabilities::builtin_file().clone()
    };
    let catalog = provider_catalog::artifact_from_config_and_capabilities(&config, &capabilities);
    Ok(ProviderSourceSnapshot {
        catalog,
        capabilities,
    })
}
