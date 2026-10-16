//! Source owners hash their own inputs; consumers compose the compiled values.
//!
//! Build scripts use `harn-source-fingerprint-build` to collect inputs.
//! Composition needs no `std` and preserves a 64-character SHA-256 identity.

#![no_std]

/// Local inputs shared by standard source owners and checkout verification.
pub const CRATE_SOURCE_ROOTS: &[&str] = &["src", "build.rs"];

/// One compiled source owner and the crate-local paths it fingerprints.
#[derive(Clone, Copy, Debug)]
pub struct SourceContribution<'a> {
    pub name: &'a str,
    pub fingerprint: &'a str,
    pub roots: &'a [&'a str],
}

impl<'a> SourceContribution<'a> {
    /// The standard contribution for a crate's `src` tree and build script.
    pub const fn crate_sources(name: &'a str, fingerprint: &'a str) -> Self {
        Self {
            name,
            fingerprint,
            roots: CRATE_SOURCE_ROOTS,
        }
    }
}

/// The source owners for one compiler or check-cache identity.
#[derive(Clone, Copy, Debug)]
pub struct SourceIdentity<'a> {
    pub domain: &'a str,
    pub components: &'a [SourceContribution<'a>],
}

impl SourceIdentity<'_> {
    /// Compose the domain and every named contribution at compile time.
    pub const fn fingerprint(&self) -> SourceFingerprint {
        compose(self.domain, self.components)
    }
}

/// A validated, lowercase hexadecimal SHA-256 fingerprint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceFingerprint([u8; 64]);

impl SourceFingerprint {
    /// Encode an already-computed SHA-256 digest.
    pub const fn from_digest(digest: [u8; 32]) -> Self {
        let digits = b"0123456789abcdef";
        let mut hex = [0; 64];
        let mut index = 0;
        while index < digest.len() {
            hex[index * 2] = digits[(digest[index] >> 4) as usize];
            hex[index * 2 + 1] = digits[(digest[index] & 15) as usize];
            index += 1;
        }
        Self(hex)
    }

    /// The immutable hexadecimal representation, including in constant values.
    pub const fn as_str(&self) -> &str {
        match core::str::from_utf8(&self.0) {
            Ok(value) => value,
            Err(_) => panic!("fingerprint encoder produced invalid UTF-8"),
        }
    }
}

/// Compose named source contributions without looking for dependency files.
///
/// Names must be unique and contributions must be nonempty SHA-256 values.
/// The domain, names, order, and all contributions participate in the digest.
pub const fn compose(domain: &str, components: &[SourceContribution<'_>]) -> SourceFingerprint {
    assert!(!domain.is_empty(), "fingerprint domain must be nonempty");
    assert!(
        !components.is_empty(),
        "fingerprint components must be nonempty"
    );
    let mut hash = sha2_const::Sha256::new()
        .update(domain.as_bytes())
        .update(&[0]);
    let mut index = 0;
    while index < components.len() {
        let SourceContribution {
            name,
            fingerprint: contribution,
            ..
        } = components[index];
        assert!(
            !name.is_empty(),
            "fingerprint component name must be nonempty"
        );
        let bytes = contribution.as_bytes();
        assert!(bytes.len() == 64, "source contribution must be SHA-256");
        let mut byte = 0;
        while byte < bytes.len() {
            assert!(
                (bytes[byte] >= b'0' && bytes[byte] <= b'9')
                    || (bytes[byte] >= b'a' && bytes[byte] <= b'f'),
                "source contribution must be lowercase hexadecimal"
            );
            byte += 1;
        }
        let mut prior = 0;
        while prior < index {
            assert!(
                !same_text(name, components[prior].name),
                "duplicate source component"
            );
            prior += 1;
        }
        hash = hash
            .update(name.as_bytes())
            .update(&[0])
            .update(bytes)
            .update(&[0]);
        index += 1;
    }
    SourceFingerprint::from_digest(hash.finalize())
}

const fn same_text(left: &str, right: &str) -> bool {
    let left = left.as_bytes();
    let right = right.as_bytes();
    if left.len() != right.len() {
        return false;
    }
    let mut index = 0;
    while index < left.len() {
        if left[index] != right[index] {
            return false;
        }
        index += 1;
    }
    true
}
