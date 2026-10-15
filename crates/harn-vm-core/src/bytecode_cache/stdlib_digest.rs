use sha2::{Digest, Sha256};
use std::sync::OnceLock;

pub fn embedded_stdlib_digest_bytes() -> &'static [u8; 32] {
    static DIGEST: OnceLock<[u8; 32]> = OnceLock::new();
    DIGEST.get_or_init(|| {
        embedded_stdlib_digest_from_sources(
            harn_modules::stdlib_catalog()
                .sources
                .iter()
                .map(|source| (source.module, source.source)),
        )
    })
}

pub fn embedded_stdlib_digest_from_sources<'a>(
    sources: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> [u8; 32] {
    let mut entries: Vec<(&str, &str)> = sources.into_iter().collect();
    entries.sort_by(|left, right| left.0.cmp(right.0));
    let mut hasher = Sha256::new();
    for (module, source) in entries {
        hasher.update(module.as_bytes());
        hasher.update(b"\0");
        hasher.update(source.as_bytes());
        hasher.update(b"\0");
    }
    hasher.finalize().into()
}
