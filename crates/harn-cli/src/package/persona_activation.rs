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

const ACTIVATION_SCHEMA_VERSION: u32 = 1;
const ACTIVATION_RECEIPT_SCHEMA_VERSION: u32 = 1;
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
    #[error("installed persona '{persona_id}' failed package integrity validation: {integrity}")]
    PackageIntegrity {
        persona_id: String,
        integrity: String,
    },
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

#[derive(Debug, Clone, Default, PartialEq)]
pub struct PersonaAttenuation {
    pub autonomy_tier: Option<PersonaAutonomyTier>,
    pub daily_usd: Option<f64>,
    pub hourly_usd: Option<f64>,
    pub run_usd: Option<f64>,
    pub frontier_escalations: Option<u32>,
    pub max_tokens: Option<u64>,
    pub max_runtime_seconds: Option<u64>,
    /// `None` inherits the exported set; `Some([])` denies the entire set.
    pub tools: Option<Vec<String>>,
    /// `None` inherits the exported set; `Some([])` denies the entire set.
    pub capabilities: Option<Vec<String>>,
    /// `None` inherits the package grant; `Some([])` denies the entire set.
    pub permissions: Option<Vec<String>>,
    /// `None` inherits the package requirements; `Some([])` denies the entire set.
    pub host_requirements: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersonaEffectivePolicy {
    pub autonomy_tier: PersonaAutonomyTier,
    pub receipt_policy: PersonaReceiptPolicy,
    pub tools: Vec<String>,
    pub capabilities: Vec<String>,
    pub permissions: Vec<String>,
    pub host_requirements: Vec<String>,
    pub model_policy: PersonaModelPolicy,
    pub budget: PersonaBudget,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersonaActivationPackage {
    pub alias: String,
    pub version: Option<String>,
    pub content_hash: String,
    pub source: String,
    pub manifest_path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersonaActivationRecord {
    pub persona_id: String,
    pub package: PersonaActivationPackage,
    pub exported_policy_digest: String,
    pub effective_policy_digest: String,
    pub effective_policy: PersonaEffectivePolicy,
    pub activated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersonaActivationLedger {
    pub schema_version: u32,
    pub activations: BTreeMap<String, PersonaActivationRecord>,
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    let ledger: PersonaActivationLedger =
        serde_json::from_slice(&bytes).map_err(|error| PersonaActivationError::InvalidLedger {
            path: path.display().to_string(),
            message: error.to_string(),
        })?;
    validate_ledger(&path, &ledger)?;
    Ok(ledger)
}

pub fn activation_ledger_path(project_root: &Path) -> PathBuf {
    project_root.join(ACTIVATION_DIR).join(ACTIVATION_FILE)
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
    if provenance.integrity != "ok" {
        return Err(PersonaActivationError::PackageIntegrity {
            persona_id: discovered.id.clone(),
            integrity: provenance.integrity.clone(),
        });
    }
    let exported_policy = effective_policy(&discovered.persona, provenance, &Default::default())?;
    let effective_policy = effective_policy(&discovered.persona, provenance, attenuation)?;
    Ok(PersonaActivationRecord {
        persona_id: discovered.id.clone(),
        package: PersonaActivationPackage {
            alias: provenance.package_alias.clone(),
            version: provenance.package_version.clone(),
            content_hash,
            source: provenance.source.clone(),
            manifest_path: discovered.manifest_path.display().to_string(),
        },
        exported_policy_digest: policy_digest(&exported_policy)?,
        effective_policy_digest: policy_digest(&effective_policy)?,
        effective_policy,
        activated_at_ms: now_ms,
    })
}

fn effective_policy(
    persona: &PersonaManifestEntry,
    provenance: &super::InstalledPersonaProvenance,
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
    let receipt_policy = persona.receipt_policy.ok_or_else(|| {
        PersonaActivationError::InvalidAttenuation("exported receipt policy is missing".to_string())
    })?;
    Ok(PersonaEffectivePolicy {
        autonomy_tier,
        receipt_policy,
        tools: attenuate_set("tool", &persona.tools, attenuation.tools.as_deref())?,
        capabilities: attenuate_set(
            "capability",
            &persona.capabilities,
            attenuation.capabilities.as_deref(),
        )?,
        permissions: attenuate_set(
            "permission",
            &provenance.permissions,
            attenuation.permissions.as_deref(),
        )?,
        host_requirements: attenuate_set(
            "host requirement",
            &provenance.host_requirements,
            attenuation.host_requirements.as_deref(),
        )?,
        model_policy: persona.model_policy.clone(),
        budget: activation_budget(&persona.budget, attenuation)?,
    })
}

fn activation_budget(
    exported: &PersonaBudget,
    attenuation: &PersonaAttenuation,
) -> Result<PersonaBudget, PersonaActivationError> {
    Ok(PersonaBudget {
        daily_usd: attenuate_cost("daily_usd", exported.daily_usd, attenuation.daily_usd)?,
        hourly_usd: attenuate_cost("hourly_usd", exported.hourly_usd, attenuation.hourly_usd)?,
        run_usd: attenuate_cost("run_usd", exported.run_usd, attenuation.run_usd)?,
        frontier_escalations: attenuate_count(
            "frontier_escalations",
            exported.frontier_escalations,
            attenuation.frontier_escalations,
        )?,
        max_tokens: attenuate_count("max_tokens", exported.max_tokens, attenuation.max_tokens)?,
        max_runtime_seconds: attenuate_count(
            "max_runtime_seconds",
            exported.max_runtime_seconds,
            attenuation.max_runtime_seconds,
        )?,
        extra: exported.extra.clone(),
    })
}

fn attenuate_cost(
    field: &str,
    exported: Option<f64>,
    requested: Option<f64>,
) -> Result<Option<f64>, PersonaActivationError> {
    let Some(requested) = requested else {
        return Ok(exported);
    };
    if !requested.is_finite() || requested < 0.0 {
        return Err(PersonaActivationError::InvalidAttenuation(format!(
            "{field} must be a finite non-negative number"
        )));
    }
    if let Some(limit) = exported {
        if requested > limit {
            return Err(PersonaActivationError::InvalidAttenuation(format!(
                "{field} {requested} exceeds exported limit {limit}"
            )));
        }
    }
    Ok(Some(requested))
}

fn attenuate_count<T>(
    field: &str,
    exported: Option<T>,
    requested: Option<T>,
) -> Result<Option<T>, PersonaActivationError>
where
    T: Copy + Ord + std::fmt::Display,
{
    let Some(requested) = requested else {
        return Ok(exported);
    };
    if let Some(limit) = exported {
        if requested > limit {
            return Err(PersonaActivationError::InvalidAttenuation(format!(
                "{field} {requested} exceeds exported limit {limit}"
            )));
        }
    }
    Ok(Some(requested))
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

fn policy_digest(policy: &PersonaEffectivePolicy) -> Result<String, PersonaActivationError> {
    let bytes = serde_json::to_vec(policy)?;
    Ok(format!("sha256:{}", hex::encode(Sha256::digest(bytes))))
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
        let actual_digest = policy_digest(&activation.effective_policy)?;
        if actual_digest != activation.effective_policy_digest {
            return Err(invalid_ledger(
                path,
                format!("activation '{id}' effective policy digest does not match its policy"),
            ));
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
