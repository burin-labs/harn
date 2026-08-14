use async_trait::async_trait;

use crate::secrets::SecretBytes;

use super::{
    requirement_fingerprint, AuthorityRequirement, IdentityBrokerFacts, IdentityBrokerRequirement,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IdentityBrokerError {
    pub code: String,
    pub message: String,
}

impl IdentityBrokerError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for IdentityBrokerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for IdentityBrokerError {}

/// A short-lived view supplied only while the exact declared consumer is
/// running. It is deliberately not serializable or cloneable.
pub struct IdentityMaterial<'a>(&'a [u8]);

impl AsRef<[u8]> for IdentityMaterial<'_> {
    fn as_ref(&self) -> &[u8] {
        self.0
    }
}

/// A process-local, non-serializable identity handle. The credential material
/// remains zeroizing and can be consumed once, only through the requirement
/// whose complete broker/provider/audience/tenant/consumer binding created it.
pub struct OpaqueIdentityHandle {
    broker_id: String,
    requirement_fingerprint: String,
    expires_at_ms: Option<u64>,
    material: Option<SecretBytes>,
}

impl std::fmt::Debug for OpaqueIdentityHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OpaqueIdentityHandle")
            .field("broker_id", &self.broker_id)
            .field("requirement_fingerprint", &self.requirement_fingerprint)
            .field("expires_at_ms", &self.expires_at_ms)
            .field("material", &"<redacted>")
            .finish()
    }
}

impl OpaqueIdentityHandle {
    pub fn new(
        requirement: &IdentityBrokerRequirement,
        material: SecretBytes,
        expires_at_ms: Option<u64>,
    ) -> Self {
        Self {
            broker_id: requirement.broker_id.clone(),
            requirement_fingerprint: requirement_fingerprint(
                &AuthorityRequirement::IdentityBroker(requirement.clone()),
            ),
            expires_at_ms,
            material: Some(material),
        }
    }

    pub fn broker_id(&self) -> &str {
        &self.broker_id
    }

    pub fn requirement_fingerprint(&self) -> &str {
        &self.requirement_fingerprint
    }

    pub fn expires_at_ms(&self) -> Option<u64> {
        self.expires_at_ms
    }

    pub fn consume<R>(
        mut self,
        requirement: &IdentityBrokerRequirement,
        now_ms: u64,
        consumer: impl FnOnce(IdentityMaterial<'_>) -> R,
    ) -> Result<R, IdentityBrokerError> {
        let observed =
            requirement_fingerprint(&AuthorityRequirement::IdentityBroker(requirement.clone()));
        if observed != self.requirement_fingerprint || requirement.broker_id != self.broker_id {
            return Err(IdentityBrokerError::new(
                "identity_binding_mismatch",
                "opaque identity handle does not match the declared broker/provider/audience/tenant/consumer binding",
            ));
        }
        if self
            .expires_at_ms
            .is_some_and(|expires_at| now_ms > expires_at)
        {
            return Err(IdentityBrokerError::new(
                "identity_handle_expired",
                "opaque identity handle expired before consumer use",
            ));
        }
        let material = self.material.take().ok_or_else(|| {
            IdentityBrokerError::new(
                "identity_handle_consumed",
                "opaque identity handle was already consumed",
            )
        })?;
        Ok(material.with_exposed(|bytes| consumer(IdentityMaterial(bytes))))
    }
}

/// The shared local/hosted seam. Implementations acquire durable material
/// outside the workload sandbox and return only a binding-fingerprinted opaque
/// handle. PreparedRun readiness validates `facts()` before this method may be
/// called.
#[async_trait]
pub trait ConsumerBoundIdentityBroker: Send + Sync {
    fn facts(&self) -> IdentityBrokerFacts;

    async fn acquire(
        &self,
        requirement: &IdentityBrokerRequirement,
    ) -> Result<OpaqueIdentityHandle, IdentityBrokerError>;
}
