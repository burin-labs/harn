use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use fs2::FileExt;
use harn_modules::personas::{
    PersonaAutonomyTier, PersonaBudget, PersonaManifestEntry, PersonaModelPolicy,
    PersonaReceiptPolicy,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{load_root_persona_catalog, resolve_discoverable_persona_in_root, DiscoverablePersona};

const ACTIVATION_SCHEMA_VERSION: u32 = 2;
const LEGACY_ACTIVATION_SCHEMA_VERSION: u32 = 1;
const ACTIVATION_RECEIPT_SCHEMA_VERSION: u32 = 2;
const ACTIVATION_DIR: &str = ".harn/personas";
const ACTIVATION_FILE: &str = "activations.json";
const ACTIVATION_LOCK_FILE: &str = "activations.lock";

#[derive(Debug, thiserror::Error)]
pub enum PersonaActivationError {
    #[error("{0}")]
    Catalog(String),
    #[error("persona '{0}' is a root persona and does not require activation")]
    RootPersona(String),
    #[error(
        "installed persona '{0}' has no package content hash; run `harn install` before activation"
    )]
    MissingContentHash(String),
    #[error("installed persona '{0}' has no pinned package-generation lock digest")]
    MissingLockDigest(String),
    #[error("installed persona '{persona_id}' failed package integrity validation: {integrity}")]
    PackageIntegrity {
        persona_id: String,
        integrity: String,
    },
    #[error("activated persona '{persona_id}' is stale: {reason}; reactivate it before use")]
    StaleActivation { persona_id: String, reason: String },
    #[error("invalid persona attenuation: {0}")]
    InvalidAttenuation(String),
    #[error(
        "activation ledger {path} uses unsupported schema version {actual}; expected {expected}"
    )]
    UnsupportedSchema {
        path: String,
        actual: u32,
        expected: u32,
    },
    #[error("activation ledger {path} is invalid: {message}")]
    InvalidLedger { path: String, message: String },
    #[error("failed to {operation} activation state at {path}: {source}")]
    Io {
        operation: &'static str,
        path: String,
        #[source]
        source: io::Error,
    },
    #[error("failed to serialize activation state: {0}")]
    Serialize(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PersonaAttenuation {
    pub autonomy_tier: Option<PersonaAutonomyTier>,
    /// `None` inherits the exported set; `Some([])` denies the entire set.
    pub tools: Option<Vec<String>>,
    /// `None` inherits the exported set; `Some([])` denies the entire set.
    pub capabilities: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersonaEffectivePolicy {
    pub autonomy_tier: PersonaAutonomyTier,
    pub tools: Vec<String>,
    pub capabilities: Vec<String>,
}

#[derive(Serialize)]
struct PersonaExportContract {
    persona: PersonaManifestEntry,
    permissions: Vec<String>,
    host_requirements: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersonaActivationPackage {
    pub alias: String,
    pub version: Option<String>,
    pub content_hash: String,
    #[serde(default)]
    pub lock_digest: String,
    pub source: String,
    pub manifest_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersonaActivationRecord {
    pub persona_id: String,
    pub package: PersonaActivationPackage,
    pub exported_policy_digest: String,
    pub effective_policy_digest: String,
    pub effective_policy: PersonaEffectivePolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub migration: Option<PersonaActivationMigration>,
    pub activated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersonaActivationMigration {
    pub status: PersonaActivationMigrationStatus,
    pub source_schema_version: u32,
    pub legacy_effective_policy_digest: String,
    /// Validated schema-v1 policy retained for audit only. None of these
    /// archived values are treated as an active runtime grant.
    pub not_enforced_policy: serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonaActivationMigrationStatus {
    ReactivationRequired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersonaActivationLedger {
    pub schema_version: u32,
    pub activations: BTreeMap<String, PersonaActivationRecord>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyPersonaEffectivePolicyV1 {
    autonomy_tier: PersonaAutonomyTier,
    receipt_policy: PersonaReceiptPolicy,
    tools: Vec<String>,
    capabilities: Vec<String>,
    permissions: Vec<String>,
    host_requirements: Vec<String>,
    model_policy: PersonaModelPolicy,
    budget: PersonaBudget,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyPersonaActivationRecordV1 {
    persona_id: String,
    package: PersonaActivationPackage,
    exported_policy_digest: String,
    effective_policy_digest: String,
    effective_policy: LegacyPersonaEffectivePolicyV1,
    activated_at_ms: i64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyPersonaActivationLedgerV1 {
    schema_version: u32,
    activations: BTreeMap<String, LegacyPersonaActivationRecordV1>,
}

#[derive(Deserialize)]
struct ActivationLedgerVersion {
    schema_version: u32,
}

impl Default for PersonaActivationLedger {
    fn default() -> Self {
        Self {
            schema_version: ACTIVATION_SCHEMA_VERSION,
            activations: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonaActivationAction {
    Activate,
    Deactivate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersonaActivationReceipt {
    pub schema_version: u32,
    pub action: PersonaActivationAction,
    pub persona_id: String,
    pub changed: bool,
    pub occurred_at_ms: i64,
    pub ledger_path: String,
    pub activation: Option<PersonaActivationRecord>,
}

pub fn activate_persona(
    manifest: Option<&Path>,
    persona_id: &str,
    attenuation: &PersonaAttenuation,
    now_ms: i64,
) -> Result<PersonaActivationReceipt, PersonaActivationError> {
    let root = load_root_persona_catalog(manifest).map_err(PersonaActivationError::Catalog)?;
    let discovered = resolve_discoverable_persona_in_root(&root, persona_id)
        .map_err(PersonaActivationError::Catalog)?;
    let candidate = activation_record(&discovered, attenuation, now_ms)?;
    let ledger_path = activation_ledger_path(&root.manifest_dir);
    let candidate_id = candidate.persona_id.clone();
    let (changed, activation) = mutate_activation_ledger(&root.manifest_dir, |ledger| {
        if let Some(existing) = ledger.activations.get(&candidate_id) {
            let mut comparable = candidate.clone();
            comparable.activated_at_ms = existing.activated_at_ms;
            if &comparable == existing {
                return (false, Some(existing.clone()));
            }
        }
        ledger
            .activations
            .insert(candidate_id.clone(), candidate.clone());
        (true, Some(candidate))
    })?;
    Ok(PersonaActivationReceipt {
        schema_version: ACTIVATION_RECEIPT_SCHEMA_VERSION,
        action: PersonaActivationAction::Activate,
        persona_id: candidate_id,
        changed,
        occurred_at_ms: now_ms,
        ledger_path: ledger_path.display().to_string(),
        activation,
    })
}

pub fn deactivate_persona(
    manifest: Option<&Path>,
    persona_id: &str,
    now_ms: i64,
) -> Result<PersonaActivationReceipt, PersonaActivationError> {
    let root = load_root_persona_catalog(manifest).map_err(PersonaActivationError::Catalog)?;
    let ledger_path = activation_ledger_path(&root.manifest_dir);
    let (changed, activation) = mutate_activation_ledger(&root.manifest_dir, |ledger| {
        let removed = ledger.activations.remove(persona_id);
        (removed.is_some(), removed)
    })?;
    Ok(PersonaActivationReceipt {
        schema_version: ACTIVATION_RECEIPT_SCHEMA_VERSION,
        action: PersonaActivationAction::Deactivate,
        persona_id: persona_id.to_string(),
        changed,
        occurred_at_ms: now_ms,
        ledger_path: ledger_path.display().to_string(),
        activation,
    })
}

pub fn list_persona_activations(
    manifest: Option<&Path>,
) -> Result<Vec<PersonaActivationRecord>, PersonaActivationError> {
    let root = load_root_persona_catalog(manifest).map_err(PersonaActivationError::Catalog)?;
    Ok(load_activation_ledger(&root.manifest_dir)?
        .activations
        .into_values()
        .collect())
}

pub fn load_activation_ledger(
    project_root: &Path,
) -> Result<PersonaActivationLedger, PersonaActivationError> {
    let path = activation_ledger_path(project_root);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(PersonaActivationLedger::default());
        }
        Err(source) => return Err(io_error("read", &path, source)),
    };
    let ledger = decode_activation_ledger(&path, &bytes)?;
    validate_ledger(&path, &ledger)?;
    Ok(ledger)
}

pub fn activation_ledger_path(project_root: &Path) -> PathBuf {
    project_root.join(ACTIVATION_DIR).join(ACTIVATION_FILE)
}

pub(crate) fn materialize_activated_persona(
    discovered: &DiscoverablePersona,
    activation: &PersonaActivationRecord,
) -> Result<PersonaManifestEntry, PersonaActivationError> {
    if activation.migration.is_some() {
        return Err(stale_activation(
            activation,
            "schema-v1 policy requires explicit reactivation".to_string(),
        ));
    }
    let provenance = discovered
        .installed_provenance()
        .ok_or_else(|| PersonaActivationError::RootPersona(discovered.id.clone()))?;
    if activation.persona_id != discovered.id {
        return Err(stale_activation(
            activation,
            format!("resolved identity is '{}'", discovered.id),
        ));
    }
    if !matches!(provenance.integrity.as_str(), "ok" | "observed") {
        return Err(PersonaActivationError::PackageIntegrity {
            persona_id: discovered.id.clone(),
            integrity: provenance.integrity.clone(),
        });
    }
    for (field, pinned, current) in [
        (
            "package alias",
            activation.package.alias.as_str(),
            provenance.package_alias.as_str(),
        ),
        (
            "content hash",
            activation.package.content_hash.as_str(),
            provenance.content_hash.as_deref().unwrap_or(""),
        ),
        (
            "package-generation lock digest",
            activation.package.lock_digest.as_str(),
            provenance.lock_digest.as_deref().unwrap_or(""),
        ),
        (
            "package source",
            activation.package.source.as_str(),
            provenance.source.as_str(),
        ),
    ] {
        if pinned != current {
            return Err(stale_activation(
                activation,
                format!("pinned {field} '{pinned}' changed to '{current}'"),
            ));
        }
    }
    if activation.package.version != provenance.package_version {
        return Err(stale_activation(
            activation,
            format!(
                "pinned package version {:?} changed to {:?}",
                activation.package.version, provenance.package_version
            ),
        ));
    }

    let exported = exported_policy_contract(&discovered.persona, provenance)?;
    if policy_digest(&exported)? != activation.exported_policy_digest {
        return Err(stale_activation(
            activation,
            "exported persona policy changed".to_string(),
        ));
    }
    let effective = &activation.effective_policy;
    let recomputed = effective_policy(
        &discovered.persona,
        &PersonaAttenuation {
            autonomy_tier: Some(effective.autonomy_tier),
            tools: Some(effective.tools.clone()),
            capabilities: Some(effective.capabilities.clone()),
        },
    )?;
    if &recomputed != effective {
        return Err(stale_activation(
            activation,
            "effective policy no longer attenuates the exported policy".to_string(),
        ));
    }

    let mut persona = discovered.persona.clone();
    persona.autonomy_tier = Some(effective.autonomy_tier);
    persona.tools.clone_from(&effective.tools);
    persona.capabilities.clone_from(&effective.capabilities);
    Ok(persona)
}

fn stale_activation(
    activation: &PersonaActivationRecord,
    reason: String,
) -> PersonaActivationError {
    PersonaActivationError::StaleActivation {
        persona_id: activation.persona_id.clone(),
        reason,
    }
}

fn activation_record(
    discovered: &DiscoverablePersona,
    attenuation: &PersonaAttenuation,
    now_ms: i64,
) -> Result<PersonaActivationRecord, PersonaActivationError> {
    let provenance = discovered
        .installed_provenance()
        .ok_or_else(|| PersonaActivationError::RootPersona(discovered.id.clone()))?;
    let content_hash = provenance
        .content_hash
        .clone()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| PersonaActivationError::MissingContentHash(discovered.id.clone()))?;
    let lock_digest = provenance
        .lock_digest
        .clone()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| PersonaActivationError::MissingLockDigest(discovered.id.clone()))?;
    if !matches!(provenance.integrity.as_str(), "ok" | "observed") {
        return Err(PersonaActivationError::PackageIntegrity {
            persona_id: discovered.id.clone(),
            integrity: provenance.integrity.clone(),
        });
    }
    let exported_policy = exported_policy_contract(&discovered.persona, provenance)?;
    let effective_policy = effective_policy(&discovered.persona, attenuation)?;
    Ok(PersonaActivationRecord {
        persona_id: discovered.id.clone(),
        package: PersonaActivationPackage {
            alias: provenance.package_alias.clone(),
            version: provenance.package_version.clone(),
            content_hash,
            lock_digest,
            source: provenance.source.clone(),
            manifest_path: discovered.manifest_path.display().to_string(),
        },
        exported_policy_digest: policy_digest(&exported_policy)?,
        effective_policy_digest: policy_digest(&effective_policy)?,
        effective_policy,
        migration: None,
        activated_at_ms: now_ms,
    })
}

fn effective_policy(
    persona: &PersonaManifestEntry,
    attenuation: &PersonaAttenuation,
) -> Result<PersonaEffectivePolicy, PersonaActivationError> {
    let exported_autonomy = persona.autonomy_tier.ok_or_else(|| {
        PersonaActivationError::InvalidAttenuation("exported autonomy tier is missing".to_string())
    })?;
    let autonomy_tier = attenuation.autonomy_tier.unwrap_or(exported_autonomy);
    if autonomy_tier > exported_autonomy {
        return Err(PersonaActivationError::InvalidAttenuation(format!(
            "autonomy {} exceeds exported {}",
            autonomy_tier.as_str(),
            exported_autonomy.as_str()
        )));
    }
    let exported_capabilities = normalized_persona_capabilities(persona);
    Ok(PersonaEffectivePolicy {
        autonomy_tier,
        tools: attenuate_set("tool", &persona.tools, attenuation.tools.as_deref())?,
        capabilities: attenuate_set(
            "capability",
            &exported_capabilities,
            attenuation.capabilities.as_deref(),
        )?,
    })
}

fn exported_policy_contract(
    persona: &PersonaManifestEntry,
    provenance: &super::InstalledPersonaProvenance,
) -> Result<PersonaExportContract, PersonaActivationError> {
    persona.autonomy_tier.ok_or_else(|| {
        PersonaActivationError::InvalidAttenuation("exported autonomy tier is missing".to_string())
    })?;
    persona.receipt_policy.ok_or_else(|| {
        PersonaActivationError::InvalidAttenuation("exported receipt policy is missing".to_string())
    })?;
    let mut persona = persona.clone();
    persona.tools = normalize_set(&persona.tools);
    persona.capabilities = normalized_persona_capabilities(&persona);
    Ok(PersonaExportContract {
        persona,
        permissions: normalize_set(&provenance.permissions),
        host_requirements: normalize_set(&provenance.host_requirements),
    })
}

pub(crate) fn normalized_persona_capabilities(persona: &PersonaManifestEntry) -> Vec<String> {
    let mut capabilities = persona.capabilities.clone();
    if persona.model_policy.default_model.is_some()
        || persona.model_policy.escalation_model.is_some()
        || !persona.model_policy.fallback_models.is_empty()
    {
        capabilities.push("llm.call".to_string());
    }
    normalize_set(&capabilities)
}

fn attenuate_set(
    kind: &str,
    exported: &[String],
    requested: Option<&[String]>,
) -> Result<Vec<String>, PersonaActivationError> {
    let exported = normalize_set(exported);
    let Some(requested) = requested else {
        return Ok(exported);
    };
    if requested.iter().any(|value| value.trim().is_empty()) {
        return Err(PersonaActivationError::InvalidAttenuation(format!(
            "{kind} names must not be empty"
        )));
    }
    let requested = normalize_set(requested);
    if let Some(extra) = requested.iter().find(|value| !exported.contains(value)) {
        return Err(PersonaActivationError::InvalidAttenuation(format!(
            "{kind} '{extra}' is not exported"
        )));
    }
    Ok(requested)
}

fn normalize_set(values: &[String]) -> Vec<String> {
    let mut values = values
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    values.sort();
    values.dedup();
    values
}

fn policy_digest(policy: &impl Serialize) -> Result<String, PersonaActivationError> {
    let bytes = serde_json::to_vec(policy)?;
    Ok(format!("sha256:{}", hex::encode(Sha256::digest(bytes))))
}

fn decode_activation_ledger(
    path: &Path,
    bytes: &[u8],
) -> Result<PersonaActivationLedger, PersonaActivationError> {
    let probe: ActivationLedgerVersion =
        serde_json::from_slice(bytes).map_err(|error| invalid_ledger(path, error.to_string()))?;
    match probe.schema_version {
        ACTIVATION_SCHEMA_VERSION => {
            serde_json::from_slice(bytes).map_err(|error| invalid_ledger(path, error.to_string()))
        }
        LEGACY_ACTIVATION_SCHEMA_VERSION => {
            let legacy: LegacyPersonaActivationLedgerV1 = serde_json::from_slice(bytes)
                .map_err(|error| invalid_ledger(path, error.to_string()))?;
            migrate_legacy_activation_ledger(path, legacy)
        }
        actual => Err(PersonaActivationError::UnsupportedSchema {
            path: path.display().to_string(),
            actual,
            expected: ACTIVATION_SCHEMA_VERSION,
        }),
    }
}

fn migrate_legacy_activation_ledger(
    path: &Path,
    legacy: LegacyPersonaActivationLedgerV1,
) -> Result<PersonaActivationLedger, PersonaActivationError> {
    if legacy.schema_version != LEGACY_ACTIVATION_SCHEMA_VERSION {
        return Err(PersonaActivationError::UnsupportedSchema {
            path: path.display().to_string(),
            actual: legacy.schema_version,
            expected: ACTIVATION_SCHEMA_VERSION,
        });
    }
    let mut activations = BTreeMap::new();
    for (id, record) in legacy.activations {
        if id != record.persona_id {
            return Err(invalid_ledger(
                path,
                format!(
                    "activation key '{id}' does not match record id '{}'",
                    record.persona_id
                ),
            ));
        }
        if record.package.content_hash.trim().is_empty() {
            return Err(invalid_ledger(
                path,
                format!("activation '{id}' has an empty content hash"),
            ));
        }
        let legacy_digest = policy_digest(&record.effective_policy)?;
        if legacy_digest != record.effective_policy_digest {
            return Err(invalid_ledger(
                path,
                format!("activation '{id}' legacy effective policy digest does not match"),
            ));
        }
        let effective_policy = PersonaEffectivePolicy {
            autonomy_tier: record.effective_policy.autonomy_tier,
            tools: normalize_set(&record.effective_policy.tools),
            capabilities: normalize_set(&record.effective_policy.capabilities),
        };
        let effective_policy_digest = policy_digest(&effective_policy)?;
        let not_enforced_policy = serde_json::to_value(&record.effective_policy)?;
        activations.insert(
            id,
            PersonaActivationRecord {
                persona_id: record.persona_id,
                package: record.package,
                exported_policy_digest: record.exported_policy_digest,
                effective_policy_digest,
                effective_policy,
                migration: Some(PersonaActivationMigration {
                    status: PersonaActivationMigrationStatus::ReactivationRequired,
                    source_schema_version: LEGACY_ACTIVATION_SCHEMA_VERSION,
                    legacy_effective_policy_digest: legacy_digest,
                    not_enforced_policy,
                }),
                activated_at_ms: record.activated_at_ms,
            },
        );
    }
    Ok(PersonaActivationLedger {
        schema_version: ACTIVATION_SCHEMA_VERSION,
        activations,
    })
}

fn validate_ledger(
    path: &Path,
    ledger: &PersonaActivationLedger,
) -> Result<(), PersonaActivationError> {
    if ledger.schema_version != ACTIVATION_SCHEMA_VERSION {
        return Err(PersonaActivationError::UnsupportedSchema {
            path: path.display().to_string(),
            actual: ledger.schema_version,
            expected: ACTIVATION_SCHEMA_VERSION,
        });
    }
    for (id, activation) in &ledger.activations {
        if id != &activation.persona_id {
            return Err(invalid_ledger(
                path,
                format!(
                    "activation key '{id}' does not match record id '{}'",
                    activation.persona_id
                ),
            ));
        }
        if activation.package.content_hash.trim().is_empty() {
            return Err(invalid_ledger(
                path,
                format!("activation '{id}' has an empty content hash"),
            ));
        }
        if activation.migration.is_none() && activation.package.lock_digest.trim().is_empty() {
            return Err(invalid_ledger(
                path,
                format!("activation '{id}' has an empty package-generation lock digest"),
            ));
        }
        let actual_digest = policy_digest(&activation.effective_policy)?;
        if actual_digest != activation.effective_policy_digest {
            return Err(invalid_ledger(
                path,
                format!("activation '{id}' effective policy digest does not match its policy"),
            ));
        }
        if let Some(migration) = &activation.migration {
            if migration.source_schema_version != LEGACY_ACTIVATION_SCHEMA_VERSION {
                return Err(invalid_ledger(
                    path,
                    format!(
                        "activation '{id}' migration has unsupported source schema version {}",
                        migration.source_schema_version
                    ),
                ));
            }
            let legacy_policy: LegacyPersonaEffectivePolicyV1 =
                serde_json::from_value(migration.not_enforced_policy.clone())
                    .map_err(|error| invalid_ledger(path, error.to_string()))?;
            let legacy_digest = policy_digest(&legacy_policy)?;
            if legacy_digest != migration.legacy_effective_policy_digest {
                return Err(invalid_ledger(
                    path,
                    format!("activation '{id}' migrated policy digest does not match its archive"),
                ));
            }
        }
    }
    Ok(())
}

fn mutate_activation_ledger<T>(
    project_root: &Path,
    mutate: impl FnOnce(&mut PersonaActivationLedger) -> (bool, T),
) -> Result<(bool, T), PersonaActivationError> {
    let path = activation_ledger_path(project_root);
    let lock_path = project_root.join(ACTIVATION_DIR).join(ACTIVATION_LOCK_FILE);
    fs::create_dir_all(lock_path.parent().unwrap_or(project_root))
        .map_err(|source| io_error("create", &lock_path, source))?;
    let lock = open_lock_file(&lock_path)?;
    lock.lock_exclusive()
        .map_err(|source| io_error("lock", &lock_path, source))?;
    let result = (|| {
        let mut ledger = load_activation_ledger(project_root)?;
        let (changed, value) = mutate(&mut ledger);
        if changed {
            write_activation_ledger(&path, &ledger)?;
        }
        Ok((changed, value))
    })();
    let unlock_result =
        FileExt::unlock(&lock).map_err(|source| io_error("unlock", &lock_path, source));
    match (result, unlock_result) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(value), Ok(())) => Ok(value),
    }
}

fn open_lock_file(path: &Path) -> Result<File, PersonaActivationError> {
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .map_err(|source| io_error("open", path, source))
}

fn write_activation_ledger(
    path: &Path,
    ledger: &PersonaActivationLedger,
) -> Result<(), PersonaActivationError> {
    let mut bytes = serde_json::to_vec_pretty(ledger)?;
    bytes.push(b'\n');
    harn_vm::atomic_io::atomic_write(path, &bytes).map_err(|source| io_error("write", path, source))
}

fn invalid_ledger(path: &Path, message: String) -> PersonaActivationError {
    PersonaActivationError::InvalidLedger {
        path: path.display().to_string(),
        message,
    }
}

fn io_error(operation: &'static str, path: &Path, source: io::Error) -> PersonaActivationError {
    PersonaActivationError::Io {
        operation,
        path: path.display().to_string(),
        source,
    }
}

#[cfg(test)]
#[path = "persona_activation_tests.rs"]
mod tests;
