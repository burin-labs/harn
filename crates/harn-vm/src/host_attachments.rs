//! Host-neutral materialization for durable attachment pointers.
//!
//! Harn owns attachment delivery policy, while embedders own artifact storage.
//! This registry is the narrow bridge between those responsibilities: a host
//! resolves an opaque pointer into provider-neutral content, and Harn decides
//! whether that content may enter the active model's context.

use std::sync::{Arc, OnceLock, RwLock};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MaterializedAttachment {
    Text(String),
    ImageUrl(String),
    ImageBase64 { media_type: String, data: String },
}

pub trait HostAttachmentResolver: Send + Sync {
    fn resolve(
        &self,
        artifact_pointer: &str,
        media_type: &str,
    ) -> Result<MaterializedAttachment, String>;
}

struct Entry {
    id: u64,
    #[cfg(test)]
    owner: std::thread::ThreadId,
    resolver: Arc<dyn HostAttachmentResolver>,
}

fn registry() -> &'static RwLock<Vec<Entry>> {
    static REGISTRY: OnceLock<RwLock<Vec<Entry>>> = OnceLock::new();
    REGISTRY.get_or_init(|| RwLock::new(Vec::new()))
}

#[must_use = "dropping the registration unregisters the attachment resolver"]
pub struct HostAttachmentResolverRegistration {
    id: u64,
}

impl Drop for HostAttachmentResolverRegistration {
    fn drop(&mut self) {
        if let Ok(mut entries) = registry().write() {
            entries.retain(|entry| entry.id != self.id);
        }
    }
}

pub fn register_host_attachment_resolver(
    resolver: Arc<dyn HostAttachmentResolver>,
) -> HostAttachmentResolverRegistration {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    registry()
        .write()
        .expect("host attachment resolver registry poisoned")
        .push(Entry {
            id,
            #[cfg(test)]
            owner: std::thread::current().id(),
            resolver,
        });
    HostAttachmentResolverRegistration { id }
}

pub(crate) fn materialize(
    artifact_pointer: &str,
    media_type: &str,
) -> Result<MaterializedAttachment, String> {
    let resolver = registry()
        .read()
        .map_err(|_| "host attachment resolver registry poisoned".to_string())?
        .iter()
        .rev()
        .find(|entry| {
            #[cfg(test)]
            {
                entry.owner == std::thread::current().id()
            }
            #[cfg(not(test))]
            {
                let _ = entry;
                true
            }
        })
        .map(|entry| Arc::clone(&entry.resolver));
    match resolver {
        Some(resolver) => resolver.resolve(artifact_pointer, media_type),
        None => Err("no host attachment resolver registered".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TextResolver;

    impl HostAttachmentResolver for TextResolver {
        fn resolve(
            &self,
            pointer: &str,
            _media_type: &str,
        ) -> Result<MaterializedAttachment, String> {
            Ok(MaterializedAttachment::Text(format!("resolved:{pointer}")))
        }
    }

    #[test]
    fn registration_is_scoped() {
        assert!(materialize("artifact:one", "text/plain").is_err());
        {
            let _registration = register_host_attachment_resolver(Arc::new(TextResolver));
            assert_eq!(
                materialize("artifact:one", "text/plain").unwrap(),
                MaterializedAttachment::Text("resolved:artifact:one".into())
            );
        }
        assert!(materialize("artifact:one", "text/plain").is_err());
    }
}
