//! Bounded, one-shot transport into the existing process-local secret provider.
//! The caller owns the pipe. No secret is placed in argv, environment or a file.

use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::io::{Read, Write};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

use super::{
    MemorySecretProvider, RotationHandle, SecretBytes, SecretError, SecretId, SecretMeta,
    SecretProvider,
};

const MAX_FRAME_BYTES: usize = 1_048_576;
const MAX_SECRETS: usize = 256;
const MAX_SECRET_BYTES: usize = 65_536;

/// Shared by the CLI receiver and contained-process sender.
pub const PARENT_SECRET_HANDOFF_OPTION: &str = "parent-secret-stdin";

struct BoundedFrame(Zeroizing<Vec<u8>>);

impl Write for BoundedFrame {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > MAX_FRAME_BYTES.saturating_sub(self.0.len()) {
            return Err(std::io::Error::other("frame exceeds transport bound"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

tokio::task_local! {
    static PARENT_PROVIDER: Arc<dyn SecretProvider>;
}

pub(super) fn active_parent_provider() -> Option<Arc<dyn SecretProvider>> {
    PARENT_PROVIDER.try_with(Arc::clone).ok()
}

/// Scope the received store over complete child startup and execution.
/// All existing configured-store readers see the same closed provider.
pub async fn with_parent_secret_handoff<T>(
    handoff: Option<ParentSecretHandoff>,
    operation: impl Future<Output = T>,
) -> T {
    match handoff {
        Some(handoff) => {
            let provider: Arc<dyn SecretProvider> = Arc::new(handoff.into_provider());
            PARENT_PROVIDER
                .scope(
                    Arc::clone(&provider),
                    super::with_active_secret_provider(Some(provider), operation),
                )
                .await
        }
        None => operation.await,
    }
}

/// An explicitly selected snapshot of a parent's resolved store.
/// Receiving it creates a closed memory store, without ambient backend fallback.
pub struct ParentSecretHandoff {
    secrets: BTreeMap<SecretId, SecretBytes>,
}

/// Received grants convey read authority, never authority to mutate the parent.
struct ReceivedParentSecrets(MemorySecretProvider);

#[async_trait::async_trait]
impl SecretProvider for ReceivedParentSecrets {
    async fn get(&self, id: &SecretId) -> Result<SecretBytes, SecretError> {
        self.0.get(id).await
    }

    async fn put(&self, _id: &SecretId, _value: SecretBytes) -> Result<(), SecretError> {
        Err(SecretError::Unsupported {
            provider: self.namespace().into(),
            operation: "write_parent_grant",
        })
    }

    async fn rotate(&self, _id: &SecretId) -> Result<RotationHandle, SecretError> {
        Err(SecretError::Unsupported {
            provider: self.namespace().into(),
            operation: "rotate_parent_grant",
        })
    }

    async fn list(&self, prefix: &SecretId) -> Result<Vec<SecretMeta>, SecretError> {
        self.0.list(prefix).await
    }

    fn namespace(&self) -> &str {
        self.0.namespace()
    }
    fn supports_versions(&self) -> bool {
        self.0.supports_versions()
    }

    fn persists_writes(&self) -> bool {
        false
    }
}

impl fmt::Debug for ParentSecretHandoff {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ParentSecretHandoff")
            .field("secret_count", &self.secrets.len())
            .finish_non_exhaustive()
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Frame {
    version: u32,
    secrets: Vec<Entry>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    id: SecretId,
    value: Vec<u8>,
}

impl Drop for Entry {
    fn drop(&mut self) {
        self.value.zeroize();
    }
}

fn invalid(reason: &'static str) -> SecretError {
    SecretError::InvalidInput(format!("parent secret handoff: {reason}"))
}

impl ParentSecretHandoff {
    /// Resolve only the named references through the parent's existing owner.
    /// Failure is closed; neither absence nor a backend error invokes a fallback.
    pub async fn capture(
        provider: &dyn SecretProvider,
        ids: impl IntoIterator<Item = SecretId>,
    ) -> Result<Self, SecretError> {
        let mut secrets = BTreeMap::new();
        let mut captured_bytes = 0usize;
        for id in ids {
            if id.namespace.trim().is_empty() || id.name.trim().is_empty() {
                return Err(invalid("invalid reference"));
            }
            if secrets.contains_key(&id) {
                return Err(invalid("duplicate reference"));
            }
            if secrets.len() == MAX_SECRETS {
                return Err(invalid("too many references"));
            }
            let value = provider.get(&id).await?;
            if value.len() > MAX_SECRET_BYTES {
                return Err(invalid("secret exceeds the transport bound"));
            }
            captured_bytes = captured_bytes
                .saturating_add(id.namespace.len())
                .saturating_add(id.name.len())
                .saturating_add(value.len());
            if captured_bytes > MAX_FRAME_BYTES {
                return Err(invalid("selected secrets exceed the transport bound"));
            }
            secrets.insert(id, value);
        }
        Ok(Self { secrets })
    }

    /// Write a length-prefixed frame, then let the caller close its one-shot pipe.
    /// Temporary serialized values are zeroized on both success and failure.
    pub fn write_to(&self, mut pipe: impl Write) -> Result<(), SecretError> {
        let frame = Frame {
            version: 1,
            secrets: self
                .secrets
                .iter()
                .map(|(id, value)| Entry {
                    id: id.clone(),
                    value: value.with_exposed(<[u8]>::to_vec),
                })
                .collect(),
        };
        let mut encoded = BoundedFrame(Zeroizing::new(Vec::new()));
        serde_json::to_writer(&mut encoded, &frame)
            .map_err(|_| invalid("cannot encode bounded frame"))?;
        let bytes = encoded.0;
        pipe.write_all(&(bytes.len() as u32).to_be_bytes())
            .and_then(|()| pipe.write_all(&bytes))
            .and_then(|()| pipe.flush())
            .map_err(|_| invalid("cannot write frame"))
    }

    /// Consume one complete bounded frame. Diagnostics never include input bytes.
    pub fn read_from(mut pipe: impl Read) -> Result<Self, SecretError> {
        let mut header = [0; 4];
        pipe.read_exact(&mut header)
            .map_err(|_| invalid("missing frame header"))?;
        let length = u32::from_be_bytes(header) as usize;
        if length == 0 || length > MAX_FRAME_BYTES {
            return Err(invalid("invalid frame length"));
        }
        let mut bytes = Zeroizing::new(vec![0; length]);
        pipe.read_exact(&mut bytes)
            .map_err(|_| invalid("incomplete frame"))?;
        let frame: Frame = serde_json::from_slice(&bytes).map_err(|_| invalid("invalid frame"))?;
        if frame.version != 1 || frame.secrets.len() > MAX_SECRETS {
            return Err(invalid("unsupported frame"));
        }
        let mut secrets = BTreeMap::new();
        for entry in frame.secrets {
            if entry.id.namespace.trim().is_empty()
                || entry.id.name.trim().is_empty()
                || entry.value.len() > MAX_SECRET_BYTES
                || secrets.contains_key(&entry.id)
            {
                return Err(invalid("invalid or duplicate reference"));
            }
            secrets.insert(entry.id.clone(), SecretBytes::from(entry.value.as_slice()));
        }
        Ok(Self { secrets })
    }

    /// Install into the existing zeroizing memory owner, without a second store.
    pub fn into_provider(self) -> impl SecretProvider {
        ReceivedParentSecrets(MemorySecretProvider::from_resolved_snapshot(
            "parent-handoff",
            self.secrets,
        ))
    }
}

#[cfg(test)]
mod tests;
