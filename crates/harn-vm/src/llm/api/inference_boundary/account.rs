//! Trusted host account confirmations, bound to the credential they describe.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::llm_config::NoTrainingRequirement;

pub const HOST_ACCOUNT_ATTESTATIONS_ENV: &str = "HARN_PROVIDER_ACCOUNT_ATTESTATIONS_JSON";

/// User-confirmed facts for one credential. Unknown is distinct from false.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProviderAccountAttestation {
    provider: String,
    credential_fingerprint: String,
    billing_enabled: Option<bool>,
    contributor_sharing_enabled: Option<bool>,
}

impl std::fmt::Debug for ProviderAccountAttestation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderAccountAttestation")
            .field("provider", &self.provider)
            .field("billing_enabled", &self.billing_enabled)
            .field(
                "contributor_sharing_enabled",
                &self.contributor_sharing_enabled,
            )
            .finish_non_exhaustive()
    }
}

impl ProviderAccountAttestation {
    /// Bind explicit account confirmations to the credential selected by the host.
    pub fn new(
        provider: &str,
        credential: &str,
        billing_enabled: Option<bool>,
        contributor_sharing_enabled: Option<bool>,
    ) -> Option<Self> {
        if provider.trim().is_empty() || credential.trim().is_empty() {
            return None;
        }
        Some(Self {
            provider: provider.to_owned(),
            credential_fingerprint: fingerprint(credential),
            billing_enabled,
            contributor_sharing_enabled,
        })
    }

    fn satisfies(
        &self,
        provider: &str,
        credential: &str,
        requirements: &[NoTrainingRequirement],
    ) -> bool {
        !credential.trim().is_empty()
            && self.provider == provider
            && self.credential_fingerprint == fingerprint(credential)
            && !requirements.is_empty()
            && requirements.iter().all(|requirement| match requirement {
                NoTrainingRequirement::BillingEnabled => self.billing_enabled == Some(true),
                NoTrainingRequirement::ContributorSharingDisabled => {
                    self.contributor_sharing_enabled == Some(false)
                }
            })
    }
}

fn fingerprint(credential: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"harn.provider-account-attestation.v1\0");
    digest.update(credential.as_bytes());
    hex::encode(digest.finalize())
}

/// Read the launcher input once, before project-controlled execution begins.
pub fn capture_process_attestations() -> Result<Vec<ProviderAccountAttestation>, String> {
    match std::env::var(HOST_ACCOUNT_ATTESTATIONS_ENV) {
        Ok(raw) if raw.len() <= 65_536 => serde_json::from_str(&raw)
            .map_err(|_| "inference_boundary.host_account_attestations_malformed".into()),
        Ok(_) | Err(std::env::VarError::NotUnicode(_)) => {
            Err("inference_boundary.host_account_attestations_malformed".into())
        }
        Err(std::env::VarError::NotPresent) => Ok(Vec::new()),
    }
}

pub(crate) fn confirmed(
    provider: &str,
    credential: Option<&str>,
    requirements: &[NoTrainingRequirement],
) -> bool {
    let Some(credential) = credential else {
        return false;
    };
    crate::security::session_environment::with_current_session_environment(|environment| {
        environment.is_some_and(|environment| {
            let credential_fingerprint = fingerprint(credential);
            let mut matching =
                environment
                    .host_account_attestations()
                    .iter()
                    .filter(|attestation| {
                        attestation.provider == provider
                            && attestation.credential_fingerprint == credential_fingerprint
                    });
            matching.next().is_some_and(|attestation| {
                attestation.satisfies(provider, credential, requirements)
            }) && matching.next().is_none()
        })
    })
}

pub(super) fn has_attestations() -> bool {
    crate::security::session_environment::with_current_session_environment(|environment| {
        environment.is_some_and(|environment| !environment.host_account_attestations().is_empty())
    })
}
