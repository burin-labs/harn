use std::path::Path;

use harn_vm::{llm::capabilities, provider_catalog};

use super::build::{generated_provider_capabilities, generated_provider_config};

/// Explicit source inputs shared by catalog documentation renderers.
/// Missing sources mean an installed CLI; present but invalid sources are errors.
pub(crate) struct ProviderSourceSnapshot {
    pub config: harn_vm::llm_config::ProvidersConfig,
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
    let (config, registration_capabilities) = if present(&catalog_source)? {
        let generated = generated_provider_config(&catalog_source)?;
        (generated.config, generated.registration_capabilities)
    } else {
        (
            harn_provider_catalog::config::embedded_config(None),
            String::new(),
        )
    };
    let capabilities = if present(&capability_source)? {
        generated_provider_capabilities(&capability_source, &registration_capabilities)?
            .capabilities
    } else if !registration_capabilities.is_empty() {
        return Err("registered model requests require capability source fragments".into());
    } else {
        harn_provider_catalog::capabilities::builtin_file().clone()
    };
    let catalog = provider_catalog::artifact_from_config_and_capabilities(&config, &capabilities);
    Ok(ProviderSourceSnapshot {
        config,
        catalog,
        capabilities,
    })
}
