use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Arc;

use async_trait::async_trait;

use crate::secrets::SecretBytes;

use super::{
    requirement_fingerprint, AuthorityRequirement, AuthorityUse, IdentityBrokerFacts,
    IdentityBrokerRequirement, IdentityRenewalMode, SecretConsumerBinding,
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

/// Process-local broker adapters available to one prepared execution. Broker
/// facts are re-read at consumption time so readiness cannot bless an adapter
/// that later drifts to a different provider, audience, tenant, or consumer.
#[derive(Clone, Default)]
pub struct IdentityBrokerRegistry {
    brokers: BTreeMap<String, Arc<dyn ConsumerBoundIdentityBroker>>,
}

impl IdentityBrokerRegistry {
    pub fn insert(
        &mut self,
        broker_id: impl Into<String>,
        broker: Arc<dyn ConsumerBoundIdentityBroker>,
    ) -> Option<Arc<dyn ConsumerBoundIdentityBroker>> {
        self.brokers.insert(broker_id.into(), broker)
    }

    fn get(&self, broker_id: &str) -> Option<Arc<dyn ConsumerBoundIdentityBroker>> {
        self.brokers.get(broker_id).cloned()
    }
}

#[derive(Clone)]
pub(crate) struct PreparedIdentityContext {
    authority: AuthorityUse,
    brokers: IdentityBrokerRegistry,
    consumer: SecretConsumerBinding,
}

tokio::task_local! {
    static PREPARED_IDENTITY_CONTEXT: PreparedIdentityContext;
}

pub(crate) async fn scope_prepared_identity<F>(
    authority: AuthorityUse,
    brokers: IdentityBrokerRegistry,
    consumer: SecretConsumerBinding,
    future: F,
) -> F::Output
where
    F: Future,
{
    PREPARED_IDENTITY_CONTEXT
        .scope(
            PreparedIdentityContext {
                authority,
                brokers,
                consumer,
            },
            future,
        )
        .await
}

/// Consume the exact prepared identity bound to a platform-managed provider.
/// `Ok(None)` is the explicit non-prepared compatibility path. When a prepared
/// context is present, every mismatch fails closed and ambient provider
/// credential discovery is unreachable.
pub(crate) async fn consume_provider_identity<R>(
    provider: &str,
    audience: &str,
    tenant: Option<&str>,
    consume: impl Fn(IdentityMaterial<'_>) -> Result<R, IdentityBrokerError>,
) -> Result<Option<R>, IdentityBrokerError> {
    let context = match PREPARED_IDENTITY_CONTEXT.try_with(Clone::clone) {
        Ok(context) => context,
        Err(_) => return Ok(None),
    };
    let requirements = context.authority.identity_requirements();
    let mut matches = requirements.iter().filter(|requirement| {
        requirement.binding.provider == provider
            && requirement.binding.audience == audience
            && requirement.binding.tenant.as_deref() == tenant
            && requirement.binding.consumer == context.consumer
    });
    let Some(requirement) = matches.next().cloned() else {
        if let Some(requirement) = requirements.first() {
            context.authority.record_denial(
                requirement,
                "prepared identity consumer binding did not match the live runtime",
            );
        }
        return Err(IdentityBrokerError::new(
            "prepared_identity_missing",
            "the prepared lease has no identity for the exact provider/audience/tenant/consumer binding",
        ));
    };
    if matches.next().is_some() {
        context.authority.record_denial(
            &requirement,
            "multiple prepared identities match one provider consumption",
        );
        return Err(IdentityBrokerError::new(
            "prepared_identity_ambiguous",
            "multiple prepared identities match the exact provider binding",
        ));
    }
    let Some(broker) = context.brokers.get(&requirement.broker_id) else {
        context.authority.record_denial(
            &requirement,
            "prepared identity broker is unavailable at use time",
        );
        return Err(IdentityBrokerError::new(
            "prepared_identity_broker_missing",
            "the prepared identity broker is unavailable at use time",
        ));
    };
    let facts = broker.facts();
    if facts.broker_id != requirement.broker_id
        || !facts.material_outside_sandbox
        || !facts.opaque_process_local_handles
        || !facts.sources.contains(&requirement.source)
        || !facts.renewal_modes.contains(&requirement.renewal)
        || !facts.bindings.contains(&requirement.binding)
    {
        context.authority.record_denial(
            &requirement,
            "prepared identity broker facts drifted before use",
        );
        return Err(IdentityBrokerError::new(
            "prepared_identity_broker_drift",
            "prepared identity broker facts drifted before use",
        ));
    }
    let authority_requirement = AuthorityRequirement::IdentityBroker(requirement.clone());
    let granted_fingerprint = context
        .authority
        .check(&authority_requirement)
        .map_err(|_| {
            IdentityBrokerError::new(
                "prepared_identity_denied",
                "the live authority lease denied identity consumption",
            )
        })?;

    let mut attempts = 0;
    loop {
        attempts += 1;
        let handle = broker.acquire(&requirement).await.map_err(|error| {
            context.authority.record_denial(
                &requirement,
                format!("identity broker acquisition failed ({})", error.code),
            );
            IdentityBrokerError::new(error.code, "identity broker acquisition failed")
        })?;
        match handle.consume(&requirement, context.authority.now_ms(), &consume) {
            Ok(Ok(value)) => {
                context.authority.mark_used(granted_fingerprint.clone());
                return Ok(Some(value));
            }
            Ok(Err(error)) => {
                context.authority.record_denial(
                    &requirement,
                    format!("identity material was malformed ({})", error.code),
                );
                return Err(IdentityBrokerError::new(
                    error.code,
                    "identity material was malformed",
                ));
            }
            Err(error)
                if error.code == "identity_handle_expired"
                    && requirement.renewal == IdentityRenewalMode::BrokerManaged
                    && attempts == 1 =>
            {
                continue;
            }
            Err(error) => {
                context.authority.record_denial(
                    &requirement,
                    format!("identity handle consumption failed ({})", error.code),
                );
                return Err(IdentityBrokerError::new(
                    error.code,
                    "identity handle consumption failed",
                ));
            }
        }
    }
}
