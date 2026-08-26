//! Demand-driven projection of manifest connector declarations.

use super::*;

/// Connector declarations plus the generation lease that keeps their module
/// paths stable while clients initialize.
pub struct ResolvedProviderConnectors {
    pub configs: Vec<ResolvedProviderConnectorConfig>,
    _package_snapshot: Option<Arc<harn_modules::package_snapshot::PackageSnapshot>>,
}

/// Materialize and resolve only the connector declarations reachable through
/// the nearest project. This is the narrow demand boundary used by connector
/// calls: unrelated persona, trigger, and hook graphs cannot delay or break a
/// script merely because it uses one connector.
pub fn try_load_provider_connectors(
    anchor: &Path,
) -> Result<ResolvedProviderConnectors, PackageError> {
    try_load_provider_connectors_with_packages(anchor, true)
}

/// Resolve only connector declarations owned by the nearest root manifest.
///
/// Root declarations take precedence over package contributions, so a call to
/// one of them must not validate or materialize unrelated dependencies merely
/// to rediscover that precedence.
pub fn try_load_root_provider_connectors(
    anchor: &Path,
) -> Result<ResolvedProviderConnectors, PackageError> {
    try_load_provider_connectors_with_packages(anchor, false)
}

fn try_load_provider_connectors_with_packages(
    anchor: &Path,
    include_packages: bool,
) -> Result<ResolvedProviderConnectors, PackageError> {
    if include_packages {
        ensure_dependencies_materialized(anchor)?;
    }
    let Some((manifest, manifest_dir)) = load_nearest_manifest(anchor).into_result()? else {
        return Ok(ResolvedProviderConnectors {
            configs: Vec::new(),
            _package_snapshot: None,
        });
    };
    let mut providers = resolved_provider_connectors_from_manifest(&manifest, &manifest_dir);
    let package_snapshot = if include_packages {
        dependency_package_snapshot(&manifest, &manifest_dir)?.map(Arc::new)
    } else {
        None
    };
    if let Some(snapshot) = package_snapshot.as_ref() {
        providers.extend(installed_package_provider_connectors(
            snapshot,
            snapshot.packages_root(),
        )?);
    }
    Ok(ResolvedProviderConnectors {
        configs: dedupe_provider_connectors(providers),
        _package_snapshot: package_snapshot,
    })
}

pub(crate) fn installed_package_provider_connectors(
    snapshot: &harn_modules::package_snapshot::PackageSnapshot,
    packages_dir: &Path,
) -> Result<Vec<ResolvedProviderConnectorConfig>, PackageError> {
    let lock = LockFile::load(snapshot.lock_path())?.ok_or_else(|| {
        PackageError::Lockfile(format!(
            "published package generation is missing {}",
            snapshot.lock_path().display()
        ))
    })?;
    let mut providers = Vec::new();
    for entry in &lock.packages {
        validate_package_alias(&entry.name)?;
        let package_dir = packages_dir.join(&entry.name);
        if package_dir.is_dir() {
            if let Some(manifest) = read_package_manifest_from_dir(&package_dir)? {
                providers.extend(resolved_provider_connectors_from_manifest(
                    &manifest,
                    &package_dir,
                ));
            }
            continue;
        }

        let package_file = packages_dir.join(format!("{}.harn", entry.name));
        if package_file.is_file() {
            continue;
        }

        return Err(PackageError::Manifest(format!(
            "installed package {} is missing under {}; run `harn install`",
            entry.name,
            packages_dir.display()
        )));
    }
    Ok(providers)
}

pub(crate) fn dedupe_provider_connectors(
    providers: Vec<ResolvedProviderConnectorConfig>,
) -> Vec<ResolvedProviderConnectorConfig> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for provider in providers {
        if seen.insert(provider.id.as_str().to_string()) {
            out.push(provider);
        }
    }
    out
}

/// Load one manifest-declared provider connector behind the runtime's common
/// connector trait. Rust builtins are already present in the default registry.
pub async fn load_provider_connector(
    config: &ResolvedProviderConnectorConfig,
) -> Result<Option<Box<dyn harn_vm::Connector>>, PackageError> {
    match &config.connector {
        ResolvedProviderConnectorKind::RustBuiltin => Ok(None),
        ResolvedProviderConnectorKind::Invalid(message) => {
            Err(PackageError::Validation(message.clone()))
        }
        ResolvedProviderConnectorKind::Harn { module } => {
            let module_path = harn_vm::resolve_module_import_path(&config.manifest_dir, module);
            let connector = harn_vm::HarnConnector::load(&module_path)
                .await
                .map_err(|error| {
                    PackageError::Validation(format!(
                        "failed to load Harn connector '{}' for provider '{}': {error}",
                        module_path.display(),
                        config.id.as_str()
                    ))
                })?;
            let observed = harn_vm::Connector::provider_id(&connector);
            if observed != &config.id {
                return Err(PackageError::Validation(format!(
                    "provider '{}' resolves to connector module '{}' which declares provider_id '{}'",
                    config.id.as_str(),
                    module_path.display(),
                    observed.as_str()
                )));
            }
            Ok(Some(Box::new(connector)))
        }
    }
}
