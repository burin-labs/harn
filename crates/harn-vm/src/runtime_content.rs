//! Linked runtime content identity for embedding hosts.
//!
//! This module owns the digest of Harn's embedded standard library and the
//! composite identity of the runtime content compiled into this crate. Hosts
//! project the typed receipt instead of reconstructing version, stdlib, or
//! compatibility facts from their own build environment.

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

const FINGERPRINT_SCHEMA: &str = "harn.runtime_content_fingerprint.v1";
const CONTENT_DIGEST_DOMAIN: &[u8] = b"harn.runtime-content.v1\0";

/// Compatibility identities that decide whether linked Harn artifacts can be
/// interpreted by this VM.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RuntimeCompatibilityFingerprint {
    pub codegen_fingerprint: String,
    pub bytecode_schema_version: u32,
    pub linked_program_schema_version: u32,
    pub linker_algorithm_version: u32,
    pub build_features: RuntimeBuildFeatures,
}

macro_rules! define_runtime_build_features {
    ($( $field:ident => $cargo_name:literal ),+ $(,)?) => {
        /// Cargo feature set compiled into the linked VM.
        ///
        /// Every declared `harn-vm` feature is represented explicitly so a lean
        /// embedding cannot share an identity with the distributed full runtime.
        #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
        #[serde(deny_unknown_fields)]
        pub struct RuntimeBuildFeatures {
            $(pub $field: bool),+
        }

        impl RuntimeBuildFeatures {
            fn linked() -> Self {
                Self {$($field: cfg!(feature = $cargo_name)),+}
            }

            fn update_digest(&self, hasher: &mut Sha256) {
                $(update_bool_field(hasher, concat!("feature-", $cargo_name), self.$field);)+
            }

            #[cfg(test)]
            const CARGO_NAMES: &'static [&'static str] = &[$($cargo_name),+];

            #[cfg(test)]
            fn none() -> Self {
                Self {$($field: false),+}
            }

            #[cfg(test)]
            fn enable(&mut self, cargo_name: &str) {
                match cargo_name {
                    $($cargo_name => self.$field = true),+,
                    _ => unreachable!("feature list and typed fields share one declaration"),
                }
            }
        }
    };
}

define_runtime_build_features! {
    default => "default",
    full => "full",
    llm_bench_internals => "llm-bench-internals",
    vm_bench_internals => "vm-bench-internals",
    content => "content",
    compression => "compression",
    http_compression => "http-compression",
    cloud_aws => "cloud-aws",
    native_keyring => "native-keyring",
    postgres => "postgres",
    sqlite => "sqlite",
    otel => "otel",
    testbench_wasi => "testbench-wasi",
}

/// Typed identity of the Harn runtime content linked into an embedding host.
///
/// `content_sha256` is derived only from compiled/runtime content. The optional
/// source revision is provenance metadata and deliberately cannot change that
/// digest by itself.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RuntimeContentFingerprint {
    pub schema: String,
    pub content_sha256: String,
    pub harn_version: String,
    pub embedded_stdlib_sha256: String,
    pub compatibility: RuntimeCompatibilityFingerprint,
    pub source_revision: Option<String>,
}

/// Return the content fingerprint of the Harn VM linked into this process.
#[must_use]
pub fn runtime_content_fingerprint() -> &'static RuntimeContentFingerprint {
    static FINGERPRINT: OnceLock<RuntimeContentFingerprint> = OnceLock::new();
    FINGERPRINT.get_or_init(|| {
        let compatibility = RuntimeCompatibilityFingerprint {
            codegen_fingerprint: crate::bytecode_cache::CODEGEN_FINGERPRINT.to_string(),
            bytecode_schema_version: crate::bytecode_cache::SCHEMA_VERSION,
            linked_program_schema_version: crate::linked_program::LINKED_PROGRAM_SCHEMA_VERSION,
            linker_algorithm_version: crate::linked_program::LINKER_ALGORITHM_VERSION,
            build_features: RuntimeBuildFeatures::linked(),
        };
        fingerprint_from_parts(
            crate::bytecode_cache::HARN_VERSION,
            embedded_stdlib_digest_bytes(),
            compatibility,
            linked_source_revision(),
        )
    })
}

pub(crate) fn embedded_stdlib_digest_bytes() -> &'static [u8; 32] {
    static DIGEST: OnceLock<[u8; 32]> = OnceLock::new();
    DIGEST.get_or_init(|| {
        embedded_stdlib_digest_from_sources(
            harn_stdlib::STDLIB_SOURCES
                .iter()
                .map(|source| (source.module, source.source)),
        )
    })
}

fn embedded_stdlib_digest_from_sources<'a>(
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

fn fingerprint_from_parts(
    harn_version: &str,
    embedded_stdlib_digest: &[u8; 32],
    compatibility: RuntimeCompatibilityFingerprint,
    source_revision: Option<String>,
) -> RuntimeContentFingerprint {
    let mut hasher = Sha256::new();
    hasher.update(CONTENT_DIGEST_DOMAIN);
    update_field(&mut hasher, "harn-version", harn_version.as_bytes());
    update_field(
        &mut hasher,
        "embedded-stdlib-sha256",
        embedded_stdlib_digest,
    );
    update_field(
        &mut hasher,
        "codegen-fingerprint",
        compatibility.codegen_fingerprint.as_bytes(),
    );
    update_field(
        &mut hasher,
        "bytecode-schema-version",
        &compatibility.bytecode_schema_version.to_le_bytes(),
    );
    update_field(
        &mut hasher,
        "linked-program-schema-version",
        &compatibility.linked_program_schema_version.to_le_bytes(),
    );
    update_field(
        &mut hasher,
        "linker-algorithm-version",
        &compatibility.linker_algorithm_version.to_le_bytes(),
    );
    compatibility.build_features.update_digest(&mut hasher);
    RuntimeContentFingerprint {
        schema: FINGERPRINT_SCHEMA.to_string(),
        content_sha256: hex(&hasher.finalize()),
        harn_version: harn_version.to_string(),
        embedded_stdlib_sha256: hex(embedded_stdlib_digest),
        compatibility,
        source_revision,
    }
}

fn update_bool_field(hasher: &mut Sha256, name: &str, enabled: bool) {
    update_field(hasher, name, &[u8::from(enabled)]);
}

fn update_field(hasher: &mut Sha256, name: &str, value: &[u8]) {
    hasher.update(name.as_bytes());
    hasher.update(b"\0");
    hasher.update((value.len() as u64).to_le_bytes());
    hasher.update(value);
}

fn linked_source_revision() -> Option<String> {
    normalize_source_revision(option_env!("HARN_BUILD_REVISION")).map(str::to_string)
}

fn normalize_source_revision(raw: Option<&str>) -> Option<&str> {
    let revision = raw?.trim();
    if matches!(revision.len(), 40 | 64)
        && revision
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Some(revision)
    } else {
        None
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lean_features() -> RuntimeBuildFeatures {
        RuntimeBuildFeatures::none()
    }

    fn compatibility() -> RuntimeCompatibilityFingerprint {
        RuntimeCompatibilityFingerprint {
            codegen_fingerprint: "codegen-a".to_string(),
            bytecode_schema_version: 14,
            linked_program_schema_version: 1,
            linker_algorithm_version: 1,
            build_features: lean_features(),
        }
    }

    #[test]
    fn embedded_stdlib_digest_changes_when_one_source_byte_changes() {
        let original = embedded_stdlib_digest_from_sources([("agent", "pub fn run() {}")]);
        let changed = embedded_stdlib_digest_from_sources([("agent", "pub fn run() { }")]);
        assert_ne!(original, changed);
    }

    #[test]
    fn source_stamp_cannot_change_content_digest() {
        let stdlib = embedded_stdlib_digest_from_sources([("agent", "source")]);
        let first = fingerprint_from_parts(
            "0.10.123-dev",
            &stdlib,
            compatibility(),
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string()),
        );
        let second = fingerprint_from_parts(
            "0.10.123-dev",
            &stdlib,
            compatibility(),
            Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string()),
        );
        assert_eq!(first.content_sha256, second.content_sha256);
        assert_ne!(first.source_revision, second.source_revision);
    }

    #[test]
    fn compatibility_change_changes_content_digest() {
        let stdlib = embedded_stdlib_digest_from_sources([("agent", "source")]);
        let first = fingerprint_from_parts("0.10.123-dev", &stdlib, compatibility(), None);
        let mut changed = compatibility();
        changed.bytecode_schema_version += 1;
        let second = fingerprint_from_parts("0.10.123-dev", &stdlib, changed, None);
        assert_ne!(first.content_sha256, second.content_sha256);
    }

    #[test]
    fn full_and_lean_builds_have_distinct_content_digests() {
        let stdlib = embedded_stdlib_digest_from_sources([("agent", "source")]);
        let lean = fingerprint_from_parts("0.10.123-dev", &stdlib, compatibility(), None);
        let mut full_compatibility = compatibility();
        for name in [
            "default",
            "full",
            "postgres",
            "sqlite",
            "content",
            "compression",
            "http-compression",
            "cloud-aws",
            "native-keyring",
        ] {
            full_compatibility.build_features.enable(name);
        }
        let full = fingerprint_from_parts("0.10.123-dev", &stdlib, full_compatibility, None);
        assert_ne!(lean.content_sha256, full.content_sha256);
    }

    #[test]
    fn every_typed_build_feature_changes_content_digest() {
        let stdlib = embedded_stdlib_digest_from_sources([("agent", "source")]);
        let baseline = fingerprint_from_parts("0.10.123-dev", &stdlib, compatibility(), None);
        for name in RuntimeBuildFeatures::CARGO_NAMES {
            let mut changed = compatibility();
            changed.build_features.enable(name);
            let fingerprint = fingerprint_from_parts("0.10.123-dev", &stdlib, changed, None);
            assert_ne!(
                baseline.content_sha256, fingerprint.content_sha256,
                "feature {name} must affect runtime content identity"
            );
        }
    }

    #[test]
    fn typed_build_features_cover_every_declared_cargo_feature() {
        let manifest: toml::Value =
            toml::from_str(include_str!("../Cargo.toml")).expect("harn-vm Cargo.toml parses");
        let mut declared: Vec<&str> = manifest["features"]
            .as_table()
            .expect("features table")
            .keys()
            .map(String::as_str)
            .collect();
        declared.sort_unstable();
        let mut represented = RuntimeBuildFeatures::CARGO_NAMES.to_vec();
        represented.sort_unstable();
        assert_eq!(declared, represented);
    }

    #[test]
    fn missing_source_revision_remains_absent() {
        let stdlib = embedded_stdlib_digest_from_sources([("agent", "source")]);
        let fingerprint = fingerprint_from_parts("0.10.123-dev", &stdlib, compatibility(), None);
        assert_eq!(fingerprint.source_revision, None);
        assert_eq!(normalize_source_revision(Some("")), None);
        assert_eq!(normalize_source_revision(Some("not-a-revision")), None);
    }

    #[test]
    fn public_fingerprint_matches_current_binary_inputs() {
        let actual = runtime_content_fingerprint();
        assert_eq!(actual.schema, FINGERPRINT_SCHEMA);
        assert_eq!(actual.harn_version, crate::bytecode_cache::HARN_VERSION);
        assert_eq!(
            actual.embedded_stdlib_sha256,
            hex(embedded_stdlib_digest_bytes())
        );
        assert_eq!(
            actual.compatibility.codegen_fingerprint,
            crate::bytecode_cache::CODEGEN_FINGERPRINT
        );
        assert_eq!(
            actual.compatibility.build_features,
            RuntimeBuildFeatures::linked()
        );
        assert_eq!(actual.content_sha256.len(), 64);
    }
}
