use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine;
use ed25519_dalek::pkcs8::{
    spki::der::pem::LineEnding, DecodePrivateKey, DecodePublicKey, EncodePrivateKey,
    EncodePublicKey,
};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::format_description::well_known::Rfc3339;
use url::Url;

use crate::package::load_skills_config;

pub(crate) const SIGNER_REGISTRY_URL_ENV: &str = "HARN_SKILL_SIGNER_REGISTRY_URL";
const SIG_SCHEMA: &str = "harn-skill-sig/v1";

#[derive(Debug, Clone)]
pub(crate) struct GeneratedKeypair {
    pub private_key_path: PathBuf,
    pub public_key_path: PathBuf,
    pub fingerprint: String,
}

#[derive(Debug, Clone)]
pub(crate) struct SignedSkill {
    pub signature_path: PathBuf,
    pub signer_fingerprint: String,
    pub skill_sha256: String,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct VerifyOptions {
    pub registry_url: Option<String>,
    pub allowed_signers: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VerificationStatus {
    Verified,
    MissingSignature,
    InvalidSignature,
    MissingSigner,
    UntrustedSigner,
}

#[derive(Debug, Clone)]
pub(crate) struct VerificationReport {
    pub skill_path: PathBuf,
    pub signature_path: PathBuf,
    pub skill_sha256: String,
    pub signer_fingerprint: Option<String>,
    pub signed: bool,
    pub trusted: bool,
    pub status: VerificationStatus,
    pub error: Option<String>,
}

impl VerificationReport {
    pub(crate) fn is_verified(&self) -> bool {
        self.status == VerificationStatus::Verified
    }

    pub(crate) fn human_summary(&self) -> String {
        match &self.error {
            Some(error) => error.clone(),
            None => match self.status {
                VerificationStatus::Verified => format!(
                    "{} verified by {}",
                    self.skill_path.display(),
                    self.signer_fingerprint.clone().unwrap_or_default()
                ),
                VerificationStatus::MissingSignature => format!(
                    "{} is missing {}",
                    self.skill_path.display(),
                    self.signature_path.display()
                ),
                VerificationStatus::InvalidSignature => {
                    format!("{} has an invalid signature", self.skill_path.display())
                }
                VerificationStatus::MissingSigner => format!(
                    "{} was signed by {}, but that signer is not installed locally and no registry resolved it",
                    self.skill_path.display(),
                    self.signer_fingerprint.clone().unwrap_or_default()
                ),
                VerificationStatus::UntrustedSigner => format!(
                    "{} was signed by {}, but that signer is not trusted for this skill",
                    self.skill_path.display(),
                    self.signer_fingerprint.clone().unwrap_or_default()
                ),
            },
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct TrustedSignerRecord {
    pub fingerprint: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SkillSignatureEnvelope {
    pub schema: String,
    pub signed_at: String,
    pub signer_fingerprint: String,
    pub ed25519_sig_base64: String,
    pub skill_sha256: String,
}

pub(crate) fn generate_keypair(out: impl AsRef<Path>) -> Result<GeneratedKeypair, String> {
    let private_key_path = out.as_ref().to_path_buf();
    if let Some(parent) = private_key_path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "failed to create private-key directory {}: {error}",
                parent.display()
            )
        })?;
    }
    let public_key_path = append_suffix(&private_key_path, ".pub");

    let seed: [u8; 32] = rand::random();
    let signing_key = SigningKey::from_bytes(&seed);
    let verifying_key = signing_key.verifying_key();
    let private_pem = signing_key
        .to_pkcs8_pem(LineEnding::LF)
        .map_err(|error| format!("failed to encode private key as PEM: {error}"))?;
    let public_pem = verifying_key
        .to_public_key_pem(LineEnding::LF)
        .map_err(|error| format!("failed to encode public key as PEM: {error}"))?;

    fs::write(&private_key_path, private_pem.as_bytes()).map_err(|error| {
        format!(
            "failed to write private key {}: {error}",
            private_key_path.display()
        )
    })?;
    fs::write(&public_key_path, public_pem.as_bytes()).map_err(|error| {
        format!(
            "failed to write public key {}: {error}",
            public_key_path.display()
        )
    })?;

    Ok(GeneratedKeypair {
        private_key_path,
        public_key_path,
        fingerprint: fingerprint_for_key(&verifying_key),
    })
}

pub(crate) fn sign_skill(
    skill_path: impl AsRef<Path>,
    private_key_path: impl AsRef<Path>,
) -> Result<SignedSkill, String> {
    let skill_path = skill_path.as_ref();
    let private_key_path = private_key_path.as_ref();
    let skill_bytes = fs::read(skill_path)
        .map_err(|error| format!("failed to read {}: {error}", skill_path.display()))?;
    let private_pem = fs::read_to_string(private_key_path)
        .map_err(|error| format!("failed to read {}: {error}", private_key_path.display()))?;
    let signing_key = SigningKey::from_pkcs8_pem(&private_pem)
        .map_err(|error| format!("failed to parse {}: {error}", private_key_path.display()))?;
    let signature = signing_key.sign(&skill_bytes);
    let signer_fingerprint = fingerprint_for_key(&signing_key.verifying_key());
    let skill_sha256 = sha256_hex(&skill_bytes);
    let signed_at = time::OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|error| format!("failed to format signed_at timestamp: {error}"))?;
    let envelope = SkillSignatureEnvelope {
        schema: SIG_SCHEMA.to_string(),
        signed_at,
        signer_fingerprint: signer_fingerprint.clone(),
        ed25519_sig_base64: base64::engine::general_purpose::STANDARD.encode(signature.to_bytes()),
        skill_sha256: skill_sha256.clone(),
    };
    let signature_path = signature_path_for(skill_path);
    let serialized = serde_json::to_string_pretty(&envelope)
        .map_err(|error| format!("failed to serialize signature: {error}"))?;
    fs::write(&signature_path, serialized.as_bytes()).map_err(|error| {
        format!(
            "failed to write signature {}: {error}",
            signature_path.display()
        )
    })?;

    Ok(SignedSkill {
        signature_path,
        signer_fingerprint,
        skill_sha256,
    })
}

pub(crate) fn verify_skill(
    skill_path: impl AsRef<Path>,
    options: &VerifyOptions,
) -> Result<VerificationReport, String> {
    let skill_path = skill_path.as_ref();
    let skill_bytes = fs::read(skill_path)
        .map_err(|error| format!("failed to read {}: {error}", skill_path.display()))?;
    let skill_sha256 = sha256_hex(&skill_bytes);
    let signature_path = signature_path_for(skill_path);
    let allowed_signers: BTreeSet<String> = options.allowed_signers.iter().cloned().collect();
    let base_report = VerificationReport {
        skill_path: skill_path.to_path_buf(),
        signature_path: signature_path.clone(),
        skill_sha256: skill_sha256.clone(),
        signer_fingerprint: None,
        signed: false,
        trusted: false,
        status: VerificationStatus::MissingSignature,
        error: None,
    };

    let signature_raw = match fs::read_to_string(&signature_path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(base_report),
        Err(error) => {
            return Err(format!(
                "failed to read signature {}: {error}",
                signature_path.display()
            ))
        }
    };
    let envelope: SkillSignatureEnvelope = match serde_json::from_str(&signature_raw) {
        Ok(envelope) => envelope,
        Err(error) => {
            return Ok(VerificationReport {
                error: Some(format!(
                    "{} is not valid {} JSON: {error}",
                    signature_path.display(),
                    SIG_SCHEMA
                )),
                status: VerificationStatus::InvalidSignature,
                ..base_report
            })
        }
    };
    if envelope.schema != SIG_SCHEMA {
        return Ok(VerificationReport {
            signer_fingerprint: Some(envelope.signer_fingerprint),
            status: VerificationStatus::InvalidSignature,
            error: Some(format!(
                "{} declares unsupported schema {}",
                signature_path.display(),
                envelope.schema
            )),
            ..base_report
        });
    }
    if envelope.skill_sha256 != skill_sha256 {
        return Ok(VerificationReport {
            signer_fingerprint: Some(envelope.signer_fingerprint),
            status: VerificationStatus::InvalidSignature,
            error: Some(format!(
                "{} does not match the current contents of {}",
                signature_path.display(),
                skill_path.display()
            )),
            ..base_report
        });
    }

    let signer_fingerprint = envelope.signer_fingerprint.clone();
    let base_report = VerificationReport {
        signer_fingerprint: Some(signer_fingerprint.clone()),
        signed: true,
        ..base_report
    };

    let verifying_key =
        match resolve_verifying_key(&signer_fingerprint, options.registry_url.as_deref())? {
            Some(key) => key,
            None => {
                return Ok(VerificationReport {
                    status: VerificationStatus::MissingSigner,
                    error: Some(format!(
                        "{} was signed by {}, but {} is not present in {}",
                        skill_path.display(),
                        signer_fingerprint,
                        signer_fingerprint,
                        trusted_signers_dir()?.display()
                    )),
                    ..base_report
                })
            }
        };
    let signature_bytes = match base64::engine::general_purpose::STANDARD
        .decode(envelope.ed25519_sig_base64.as_bytes())
    {
        Ok(bytes) => bytes,
        Err(error) => {
            return Ok(VerificationReport {
                status: VerificationStatus::InvalidSignature,
                error: Some(format!("signature is not valid base64: {error}")),
                ..base_report
            })
        }
    };
    let signature = match Signature::from_slice(&signature_bytes) {
        Ok(signature) => signature,
        Err(error) => {
            return Ok(VerificationReport {
                status: VerificationStatus::InvalidSignature,
                error: Some(format!("signature is not valid Ed25519 bytes: {error}")),
                ..base_report
            })
        }
    };
    if verifying_key.verify(&skill_bytes, &signature).is_err() {
        return Ok(VerificationReport {
            status: VerificationStatus::InvalidSignature,
            error: Some(format!(
                "{} failed Ed25519 verification for {}",
                signature_path.display(),
                skill_path.display()
            )),
            ..base_report
        });
    }
    if !allowed_signers.is_empty() && !allowed_signers.contains(&signer_fingerprint) {
        return Ok(VerificationReport {
            status: VerificationStatus::UntrustedSigner,
            error: Some(format!(
                "{} was signed by {}, which is not in the skill's trusted_signers allowlist",
                skill_path.display(),
                signer_fingerprint
            )),
            ..base_report
        });
    }

    Ok(VerificationReport {
        trusted: true,
        status: VerificationStatus::Verified,
        ..base_report
    })
}

pub(crate) fn trust_add(from: &str) -> Result<TrustedSignerRecord, String> {
    let verifying_key = verifying_key_from_source(from)?;
    let fingerprint = fingerprint_for_key(&verifying_key);
    let pem = verifying_key
        .to_public_key_pem(LineEnding::LF)
        .map_err(|error| format!("failed to encode public key PEM: {error}"))?;
    let dir = trusted_signers_dir()?;
    fs::create_dir_all(&dir)
        .map_err(|error| format!("failed to create {}: {error}", dir.display()))?;
    let path = dir.join(format!("{fingerprint}.pub"));
    fs::write(&path, pem.as_bytes())
        .map_err(|error| format!("failed to write {}: {error}", path.display()))?;
    Ok(TrustedSignerRecord { fingerprint, path })
}

pub(crate) fn trust_list() -> Result<Vec<TrustedSignerRecord>, String> {
    let dir = trusted_signers_dir()?;
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut records = Vec::new();
    let entries =
        fs::read_dir(&dir).map_err(|error| format!("failed to read {}: {error}", dir.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("pub") {
            continue;
        }
        let raw = fs::read_to_string(&path)
            .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
        let verifying_key = VerifyingKey::from_public_key_pem(&raw)
            .map_err(|error| format!("failed to parse {}: {error}", path.display()))?;
        records.push(TrustedSignerRecord {
            fingerprint: fingerprint_for_key(&verifying_key),
            path,
        });
    }
    records.sort_by(|left, right| left.fingerprint.cmp(&right.fingerprint));
    Ok(records)
}

pub(crate) fn configured_registry_url(anchor: Option<&Path>) -> Option<String> {
    if let Ok(raw) = std::env::var(SIGNER_REGISTRY_URL_ENV) {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    load_skills_config(anchor).and_then(|resolved| resolved.config.signer_registry_url)
}

pub(crate) fn signature_path_for(skill_path: &Path) -> PathBuf {
    append_suffix(skill_path, ".sig")
}

pub(crate) fn trusted_signers_dir() -> Result<PathBuf, String> {
    user_home_dir()
        .map(|home| home.join(".harn").join("trusted-signers"))
        .ok_or_else(|| "could not determine the current user's home directory".to_string())
}

fn resolve_verifying_key(
    fingerprint: &str,
    registry_url: Option<&str>,
) -> Result<Option<VerifyingKey>, String> {
    let local_path = trusted_signers_dir()?.join(format!("{fingerprint}.pub"));
    if local_path.is_file() {
        let pem = fs::read_to_string(&local_path)
            .map_err(|error| format!("failed to read {}: {error}", local_path.display()))?;
        let key = VerifyingKey::from_public_key_pem(&pem)
            .map_err(|error| format!("failed to parse {}: {error}", local_path.display()))?;
        return Ok(Some(key));
    }

    let Some(registry_url) = registry_url else {
        return Ok(None);
    };
    let pem = match fetch_registry_public_key(registry_url, fingerprint)? {
        Some(pem) => pem,
        None => return Ok(None),
    };
    let key = VerifyingKey::from_public_key_pem(&pem)
        .map_err(|error| format!("failed to parse signer from registry: {error}"))?;
    Ok(Some(key))
}

fn fetch_registry_public_key(
    registry_url: &str,
    fingerprint: &str,
) -> Result<Option<String>, String> {
    let filename = format!("{fingerprint}.pub");
    if let Some(path) = file_url_or_path(registry_url)? {
        let resolved = path.join(filename);
        return match fs::read_to_string(&resolved) {
            Ok(raw) => Ok(Some(raw)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(format!("failed to read {}: {error}", resolved.display())),
        };
    }

    let base = Url::parse(registry_url)
        .map_err(|error| format!("invalid signer registry URL {registry_url:?}: {error}"))?;
    let url = base
        .join(&filename)
        .map_err(|error| format!("failed to resolve signer URL from {registry_url:?}: {error}"))?;
    let response = reqwest::blocking::get(url.clone())
        .map_err(|error| format!("failed to fetch {url}: {error}"))?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let response = response
        .error_for_status()
        .map_err(|error| format!("failed to fetch {url}: {error}"))?;
    response
        .text()
        .map(Some)
        .map_err(|error| format!("failed to read {url}: {error}"))
}

fn verifying_key_from_source(from: &str) -> Result<VerifyingKey, String> {
    let raw = if let Some(path) = file_url_or_path(from)? {
        fs::read_to_string(&path)
            .map_err(|error| format!("failed to read {}: {error}", path.display()))?
    } else {
        let url = Url::parse(from).map_err(|error| format!("invalid URL {from:?}: {error}"))?;
        let response = reqwest::blocking::get(url.clone())
            .map_err(|error| format!("failed to fetch {url}: {error}"))?;
        let response = response
            .error_for_status()
            .map_err(|error| format!("failed to fetch {url}: {error}"))?;
        response
            .text()
            .map_err(|error| format!("failed to read {url}: {error}"))?
    };
    VerifyingKey::from_public_key_pem(&raw)
        .map_err(|error| format!("failed to parse Ed25519 public key: {error}"))
}

fn file_url_or_path(raw: &str) -> Result<Option<PathBuf>, String> {
    if raw.starts_with("http://") || raw.starts_with("https://") {
        return Ok(None);
    }
    if raw.starts_with("file://") {
        let url = Url::parse(raw).map_err(|error| format!("invalid file URL {raw:?}: {error}"))?;
        return url
            .to_file_path()
            .map(Some)
            .map_err(|_| format!("could not convert {raw:?} into a filesystem path"));
    }
    Ok(Some(PathBuf::from(raw)))
}

fn append_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut raw: OsString = path.as_os_str().to_os_string();
    raw.push(suffix);
    PathBuf::from(raw)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    hex_encode(&digest)
}

fn fingerprint_for_key(key: &VerifyingKey) -> String {
    let digest = Sha256::digest(key.as_bytes());
    hex_encode(&digest)
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn user_home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    use crate::tests::common::{cwd_lock::lock_cwd, env_lock::lock_env};

    fn write_skill(path: &Path, body: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    #[test]
    fn keygen_sign_and_verify_roundtrip() {
        let _cwd = lock_cwd();
        let _env = lock_env().blocking_lock();
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", tmp.path());

        let skill = tmp.path().join("skill").join("SKILL.md");
        write_skill(&skill, "---\nname: deploy\n---\nship it\n");
        let keys = generate_keypair(tmp.path().join("signer.pem")).unwrap();
        let signed = sign_skill(&skill, &keys.private_key_path).unwrap();
        let signer = trust_add(keys.public_key_path.to_str().unwrap()).unwrap();
        let report = verify_skill(&skill, &VerifyOptions::default()).unwrap();

        assert_eq!(signed.signer_fingerprint, keys.fingerprint);
        assert_eq!(signer.fingerprint, keys.fingerprint);
        assert!(report.is_verified());
        assert_eq!(
            report.signer_fingerprint.as_deref(),
            Some(keys.fingerprint.as_str())
        );
    }

    #[test]
    fn verify_rejects_tampered_skill_payload() {
        let _cwd = lock_cwd();
        let _env = lock_env().blocking_lock();
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", tmp.path());

        let skill = tmp.path().join("skill").join("SKILL.md");
        write_skill(&skill, "---\nname: deploy\n---\nship it\n");
        let keys = generate_keypair(tmp.path().join("signer.pem")).unwrap();
        sign_skill(&skill, &keys.private_key_path).unwrap();
        trust_add(keys.public_key_path.to_str().unwrap()).unwrap();
        fs::write(&skill, "---\nname: deploy\n---\nship it now\n").unwrap();

        let report = verify_skill(&skill, &VerifyOptions::default()).unwrap();
        assert_eq!(report.status, VerificationStatus::InvalidSignature);
    }

    #[test]
    fn verify_rejects_wrong_key_signature() {
        let _cwd = lock_cwd();
        let _env = lock_env().blocking_lock();
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", tmp.path());

        let skill = tmp.path().join("skill").join("SKILL.md");
        write_skill(&skill, "---\nname: deploy\n---\nship it\n");
        let signing_keys = generate_keypair(tmp.path().join("signer.pem")).unwrap();
        let trusted_keys = generate_keypair(tmp.path().join("trusted.pem")).unwrap();
        sign_skill(&skill, &signing_keys.private_key_path).unwrap();
        trust_add(trusted_keys.public_key_path.to_str().unwrap()).unwrap();

        let sig_path = signature_path_for(&skill);
        let mut envelope: SkillSignatureEnvelope =
            serde_json::from_str(&fs::read_to_string(&sig_path).unwrap()).unwrap();
        envelope.signer_fingerprint = trusted_keys.fingerprint.clone();
        fs::write(&sig_path, serde_json::to_string_pretty(&envelope).unwrap()).unwrap();

        let report = verify_skill(&skill, &VerifyOptions::default()).unwrap();
        assert_eq!(report.status, VerificationStatus::InvalidSignature);
    }

    #[test]
    fn verify_reports_missing_signer() {
        let _cwd = lock_cwd();
        let _env = lock_env().blocking_lock();
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", tmp.path());

        let skill = tmp.path().join("skill").join("SKILL.md");
        write_skill(&skill, "---\nname: deploy\n---\nship it\n");
        let keys = generate_keypair(tmp.path().join("signer.pem")).unwrap();
        sign_skill(&skill, &keys.private_key_path).unwrap();

        let report = verify_skill(&skill, &VerifyOptions::default()).unwrap();
        assert_eq!(report.status, VerificationStatus::MissingSigner);
        assert!(report.signed);
        assert!(!report.trusted);
    }

    #[test]
    fn verify_honors_allowed_signers() {
        let _cwd = lock_cwd();
        let _env = lock_env().blocking_lock();
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", tmp.path());

        let skill = tmp.path().join("skill").join("SKILL.md");
        write_skill(&skill, "---\nname: deploy\n---\nship it\n");
        let keys = generate_keypair(tmp.path().join("signer.pem")).unwrap();
        sign_skill(&skill, &keys.private_key_path).unwrap();
        trust_add(keys.public_key_path.to_str().unwrap()).unwrap();

        let report = verify_skill(
            &skill,
            &VerifyOptions {
                allowed_signers: vec!["not-the-signer".to_string()],
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(report.status, VerificationStatus::UntrustedSigner);
    }
}
