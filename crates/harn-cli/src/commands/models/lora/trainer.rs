use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const LORA_TRAINER_IDENTITY_SCHEMA_VERSION: u64 = 1;
const LORA_TRAINER_ENVIRONMENT_SCHEMA_VERSION: u64 = 1;

pub(super) fn normalize_lora_trainer(raw: &str) -> Result<String, String> {
    let trainer = raw.trim().to_ascii_lowercase().replace('-', "_");
    match trainer.as_str() {
        "trl" | "trl_sft" | "trl_sft_trainer" => Ok("trl_sft_trainer".to_string()),
        "unsloth" | "unsloth_sft" | "unsloth_trl_sft" => Ok("unsloth_sft".to_string()),
        "mlx" | "mlx_lm" | "mlx_lm_sft" | "mlx_lora" => Ok("mlx_lm".to_string()),
        "external" | "external_sft" | "external_sft_trainer" => {
            Ok("external_sft_trainer".to_string())
        }
        _ => Err(format!(
            "unsupported LoRA trainer `{raw}`; expected `trl_sft_trainer`, `unsloth_sft`, `mlx_lm`, or `external_sft_trainer`"
        )),
    }
}

pub(super) fn trainer_identity_from_args(
    trainer_identity: Option<&str>,
    trainer_version: Option<&str>,
) -> Result<Option<TrainerIdentity>, String> {
    if let Some(raw) = trainer_identity {
        return parse_trainer_identity(raw).map(Some);
    }
    trainer_version
        .map(|version| {
            make_trainer_identity("version", version)
                .map_err(|error| format!("invalid --trainer-version `{version}`: {error}"))
        })
        .transpose()
}

pub(super) fn parse_trainer_identity(raw: &str) -> Result<TrainerIdentity, String> {
    let Some((kind, value)) = raw.split_once('=') else {
        return Err(format!(
            "invalid trainer identity `{raw}`; expected KIND=VALUE"
        ));
    };
    make_trainer_identity(kind, value)
}

pub(super) fn make_trainer_identity(kind: &str, value: &str) -> Result<TrainerIdentity, String> {
    let kind = kind.trim().to_ascii_lowercase().replace('-', "_");
    let value = value.trim();
    if value.is_empty() {
        return Err("value must be non-empty".to_string());
    }
    if !matches!(
        kind.as_str(),
        "version" | "revision" | "lockfile_sha256" | "container_digest" | "backend_fingerprint"
    ) {
        return Err(format!(
            "unsupported trainer identity kind `{kind}`; expected version, revision, lockfile_sha256, container_digest, or backend_fingerprint"
        ));
    }
    Ok(TrainerIdentity {
        schema_version: LORA_TRAINER_IDENTITY_SCHEMA_VERSION,
        kind,
        value: value.to_string(),
    })
}

pub(super) fn read_trainer_identity_file(path: &Path) -> Result<Option<TrainerIdentity>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(path).map_err(|error| {
        format!(
            "failed to read trainer identity {}: {error}",
            path.display()
        )
    })?;
    let value = serde_json::from_str::<serde_json::Value>(&raw).map_err(|error| {
        format!(
            "failed to parse trainer identity {}: {error}",
            path.display()
        )
    })?;
    trainer_identity_from_value(&value)
        .map(Some)
        .map_err(|error| format!("invalid trainer identity {}: {error}", path.display()))
}

fn trainer_identity_from_value(value: &serde_json::Value) -> Result<TrainerIdentity, String> {
    let object = value
        .get("trainer_identity")
        .or_else(|| value.get("identity"))
        .unwrap_or(value);
    let kind = object
        .get("kind")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "missing kind".to_string())?;
    let value = object
        .get("value")
        .or_else(|| object.get("fingerprint"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "missing value".to_string())?;
    make_trainer_identity(kind, value)
}

pub(super) fn trainer_identity_check(
    expected: Option<TrainerIdentity>,
    observed: Option<TrainerIdentity>,
) -> TrainerIdentityCheck {
    let mut errors = Vec::new();
    let status = match (&expected, &observed) {
        (Some(expected), Some(observed)) if expected == observed => "matched",
        (Some(expected), Some(observed)) => {
            errors.push(format!(
                "trainer identity mismatch: expected {}={} observed {}={}",
                expected.kind, expected.value, observed.kind, observed.value
            ));
            "mismatched"
        }
        (Some(_), None) => {
            errors.push("observed trainer identity is missing".to_string());
            "missing_observed"
        }
        (None, Some(_)) | (None, None) => {
            errors.push("expected trainer identity is missing".to_string());
            "missing_expected"
        }
    }
    .to_string();
    TrainerIdentityCheck {
        schema_version: LORA_TRAINER_IDENTITY_SCHEMA_VERSION,
        expected,
        observed,
        status: status.clone(),
        promotable: status == "matched",
        errors,
    }
}

/// Raw backend observation. Harn never accepts a backend-supplied digest,
/// status, or promotion bit: it normalizes these candidate facts once and
/// derives the attestation below.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct TrainerEnvironmentObservation {
    pub(super) schema_version: u64,
    #[serde(default)]
    pub(super) resolver: Option<BTreeMap<String, String>>,
    #[serde(default)]
    pub(super) runtime: Option<BTreeMap<String, String>>,
    #[serde(default)]
    pub(super) packages: Option<BTreeMap<String, String>>,
    #[serde(default)]
    pub(super) optional_extensions: Option<BTreeMap<String, String>>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(super) struct TrainerEnvironmentAttestation {
    pub(super) schema_version: u64,
    pub(super) declared_trainer_identity: TrainerIdentity,
    pub(super) resolver: BTreeMap<String, String>,
    pub(super) runtime: BTreeMap<String, String>,
    pub(super) packages: BTreeMap<String, String>,
    pub(super) optional_extensions: BTreeMap<String, String>,
    pub(super) digest: String,
}

/// Canonical digest preimage. The digest is an output of the attestation, never
/// an input, so it must not be serialized back into its own hash.
#[derive(Serialize)]
struct TrainerEnvironmentDigestInput<'a> {
    schema_version: u64,
    declared_trainer_identity: &'a TrainerIdentity,
    resolver: &'a BTreeMap<String, String>,
    runtime: &'a BTreeMap<String, String>,
    packages: &'a BTreeMap<String, String>,
    optional_extensions: &'a BTreeMap<String, String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(in crate::commands::models) struct TrainerEnvironmentCheck {
    pub(super) schema_version: u64,
    pub(super) status: String,
    pub(super) promotable: bool,
    pub(super) attestation: Option<TrainerEnvironmentAttestation>,
    pub(super) errors: Vec<String>,
}

pub(super) fn trainer_environment_check(
    declared_trainer_identity: Option<TrainerIdentity>,
    observation: Option<TrainerEnvironmentObservation>,
) -> TrainerEnvironmentCheck {
    let mut errors = Vec::new();
    let Some(declared_trainer_identity) = declared_trainer_identity else {
        errors.push("declared trainer identity is missing".to_string());
        return TrainerEnvironmentCheck {
            schema_version: LORA_TRAINER_ENVIRONMENT_SCHEMA_VERSION,
            status: "missing_declared_identity".to_string(),
            promotable: false,
            attestation: None,
            errors,
        };
    };
    let Some(observation) = observation else {
        errors.push("trainer environment observation is missing".to_string());
        return TrainerEnvironmentCheck {
            schema_version: LORA_TRAINER_ENVIRONMENT_SCHEMA_VERSION,
            status: "missing_observation".to_string(),
            promotable: false,
            attestation: None,
            errors,
        };
    };
    if observation.schema_version != LORA_TRAINER_ENVIRONMENT_SCHEMA_VERSION {
        errors.push(format!(
            "unsupported trainer environment observation schema_version {}; expected {}",
            observation.schema_version, LORA_TRAINER_ENVIRONMENT_SCHEMA_VERSION
        ));
    }
    let resolver = normalize_environment_facts("resolver", observation.resolver, &mut errors);
    let runtime = normalize_environment_facts("runtime", observation.runtime, &mut errors);
    let packages = normalize_environment_facts("packages", observation.packages, &mut errors);
    let optional_extensions = normalize_environment_facts(
        "optional_extensions",
        observation.optional_extensions,
        &mut errors,
    );
    require_environment_facts("resolver", &resolver, &mut errors);
    require_environment_facts("runtime", &runtime, &mut errors);
    require_environment_facts("packages", &packages, &mut errors);
    if !errors.is_empty() {
        return TrainerEnvironmentCheck {
            schema_version: LORA_TRAINER_ENVIRONMENT_SCHEMA_VERSION,
            status: "invalid_observation".to_string(),
            promotable: false,
            attestation: None,
            errors,
        };
    }
    let mut attestation = TrainerEnvironmentAttestation {
        schema_version: LORA_TRAINER_ENVIRONMENT_SCHEMA_VERSION,
        declared_trainer_identity,
        resolver,
        runtime,
        packages,
        optional_extensions,
        digest: String::new(),
    };
    attestation.digest = trainer_environment_digest(&attestation);
    TrainerEnvironmentCheck {
        schema_version: LORA_TRAINER_ENVIRONMENT_SCHEMA_VERSION,
        status: "attested".to_string(),
        promotable: true,
        attestation: Some(attestation),
        errors,
    }
}

fn normalize_environment_facts(
    field: &str,
    facts: Option<BTreeMap<String, String>>,
    errors: &mut Vec<String>,
) -> BTreeMap<String, String> {
    let Some(facts) = facts else {
        errors.push(format!(
            "trainer environment observation is missing {field}"
        ));
        return BTreeMap::new();
    };
    let mut normalized = BTreeMap::new();
    for (key, value) in facts {
        let normalized_key = normalize_environment_key(field, &key, errors);
        let normalized_value = normalize_environment_value(field, &key, &value, errors);
        if let (Some(normalized_key), Some(normalized_value)) = (normalized_key, normalized_value) {
            if normalized
                .insert(normalized_key.clone(), normalized_value)
                .is_some()
            {
                errors.push(format!(
                    "trainer environment {field} has duplicate canonical key `{normalized_key}`"
                ));
            }
        }
    }
    normalized
}

/// A promotable attestation must describe the realized trainer, not just its schema.
fn require_environment_facts(
    field: &str,
    facts: &BTreeMap<String, String>,
    errors: &mut Vec<String>,
) {
    if facts.is_empty() {
        errors.push(format!(
            "trainer environment observation {field} must contain at least one fact"
        ));
    }
}

fn normalize_environment_key(field: &str, key: &str, errors: &mut Vec<String>) -> Option<String> {
    let normalized = key.trim().to_ascii_lowercase();
    if normalized.is_empty()
        || !normalized.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
        })
    {
        errors.push(format!(
            "trainer environment {field} key `{key}` must use a non-empty ASCII identifier"
        ));
        return None;
    }
    Some(normalized)
}

fn normalize_environment_value(
    field: &str,
    key: &str,
    value: &str,
    errors: &mut Vec<String>,
) -> Option<String> {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        errors.push(format!(
            "trainer environment {field}.{key} must be non-empty"
        ));
        return None;
    }
    if normalized.chars().any(char::is_control) || is_machine_local_path(&normalized) {
        errors.push(format!(
            "trainer environment {field}.{key} must not contain a machine-local path"
        ));
        return None;
    }
    Some(normalized)
}

fn is_machine_local_path(value: &str) -> bool {
    let value = value.trim();
    value.starts_with('/')
        || value.starts_with('\\')
        || value.starts_with("~/")
        || value.starts_with("~\\")
        || value.starts_with("./")
        || value.starts_with(".\\")
        || value.starts_with("../")
        || value.starts_with("..\\")
        || value
            .as_bytes()
            .get(1..3)
            .is_some_and(|prefix| prefix[0] == b':' && matches!(prefix[1], b'/' | b'\\'))
            && value
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphabetic)
}

fn trainer_environment_digest(attestation: &TrainerEnvironmentAttestation) -> String {
    let canonical = TrainerEnvironmentDigestInput {
        schema_version: attestation.schema_version,
        declared_trainer_identity: &attestation.declared_trainer_identity,
        resolver: &attestation.resolver,
        runtime: &attestation.runtime,
        packages: &attestation.packages,
        optional_extensions: &attestation.optional_extensions,
    };
    let bytes = serde_json::to_vec(&canonical)
        .expect("trainer environment attestation is JSON-serializable");
    let mut hasher = Sha256::new();
    hasher.update(b"harn_trainer_environment_attestation_v1\0");
    hasher.update(bytes);
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

pub(super) fn trainer_identity_args(identity: Option<&TrainerIdentity>) -> Vec<String> {
    identity
        .map(|identity| {
            vec![
                "--trainer-identity".to_string(),
                format!("{}={}", identity.kind, identity.value),
            ]
        })
        .unwrap_or_default()
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct TrainerIdentity {
    pub(super) schema_version: u64,
    pub(super) kind: String,
    pub(super) value: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(in crate::commands::models) struct TrainerIdentityCheck {
    pub(super) schema_version: u64,
    pub(super) expected: Option<TrainerIdentity>,
    pub(super) observed: Option<TrainerIdentity>,
    pub(super) status: String,
    pub(super) promotable: bool,
    pub(super) errors: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trainer_environment_requires_extension_visibility_and_supports_non_python_backends() {
        let declared =
            make_trainer_identity("revision", "mlx-trainer-r1").expect("declared identity");
        let missing_extensions: TrainerEnvironmentObservation = serde_json::from_str(
            r#"{
              "schema_version": 1,
              "resolver": {},
              "runtime": {"engine": "mlx"},
              "packages": {"mlx_lm": "0.23.0"}
            }"#,
        )
        .expect("missing extension visibility is readable");
        let missing_check =
            trainer_environment_check(Some(declared.clone()), Some(missing_extensions));
        assert!(!missing_check.promotable);
        assert!(missing_check.attestation.is_none());

        let mlx: TrainerEnvironmentObservation = serde_json::from_str(
            r#"{
              "schema_version": 1,
              "resolver": {"tool": "pixi 0.39.0"},
              "runtime": {"engine": "mlx", "swift": "6.1"},
              "packages": {"mlx_lm": "0.23.0"},
              "optional_extensions": {"metal": "present"}
            }"#,
        )
        .expect("non-python observation");
        let mlx_check = trainer_environment_check(Some(declared), Some(mlx));
        assert!(mlx_check.promotable);
        assert_eq!(mlx_check.status, "attested");
    }

    #[test]
    fn trainer_environment_rejects_vacuous_core_facts() {
        let declared =
            make_trainer_identity("revision", "mlx-trainer-r1").expect("declared identity");
        let empty: TrainerEnvironmentObservation = serde_json::from_str(
            r#"{
              "schema_version": 1,
              "resolver": {},
              "runtime": {},
              "packages": {},
              "optional_extensions": {}
            }"#,
        )
        .expect("empty observation is syntactically valid");

        let check = trainer_environment_check(Some(declared), Some(empty));

        assert_eq!(check.status, "invalid_observation");
        assert!(!check.promotable);
        assert!(check.attestation.is_none());
        assert_eq!(
            check.errors,
            vec![
                "trainer environment observation resolver must contain at least one fact",
                "trainer environment observation runtime must contain at least one fact",
                "trainer environment observation packages must contain at least one fact",
            ]
        );
    }

    #[test]
    fn trainer_environment_accepts_minimal_core_facts_with_no_extensions() {
        let declared =
            make_trainer_identity("revision", "mlx-trainer-r1").expect("declared identity");
        let observation: TrainerEnvironmentObservation = serde_json::from_str(
            r#"{
              "schema_version": 1,
              "resolver": {" Tool ": "  pixi   0.39.0 "},
              "runtime": {"Engine": " mlx "},
              "packages": {"mlx_lm": " 0.23.0 "},
              "optional_extensions": {}
            }"#,
        )
        .expect("minimal observation");

        let check = trainer_environment_check(Some(declared.clone()), Some(observation));

        assert_eq!(check.status, "attested");
        assert!(check.promotable);
        assert!(check.errors.is_empty());
        let attestation = check.attestation.expect("promotable attestation");
        assert_eq!(attestation.declared_trainer_identity, declared);
        assert_eq!(
            attestation.resolver,
            BTreeMap::from([("tool".to_string(), "pixi 0.39.0".to_string())])
        );
        assert_eq!(
            attestation.runtime,
            BTreeMap::from([("engine".to_string(), "mlx".to_string())])
        );
        assert_eq!(
            attestation.packages,
            BTreeMap::from([("mlx_lm".to_string(), "0.23.0".to_string())])
        );
        assert!(attestation.optional_extensions.is_empty());
        assert!(attestation.digest.starts_with("sha256:"));
    }
}
