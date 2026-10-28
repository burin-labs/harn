//! Artifact identity includes the contracts linked into this execution backend.

use harn_builtin_registry::BuiltinManifestEntry;
use sha2::{Digest, Sha256};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, OnceLock};

/// One declaration shared by the linked compiler and stale-generator validation.
pub const COMPILER_SOURCE_IDENTITY: harn_source_fingerprint::SourceIdentity<'static> = {
    use harn_source_fingerprint::{SourceContribution, SourceIdentity};
    SourceIdentity {
        domain: "harn.compiler-source.v2",
        components: &[
            SourceContribution::crate_sources("harn-lexer", harn_lexer::SOURCE_FINGERPRINT),
            SourceContribution::crate_sources("harn-parser", harn_parser::SOURCE_FINGERPRINT),
            SourceContribution::crate_sources("harn-kernel", harn_kernel::SOURCE_FINGERPRINT),
            SourceContribution::crate_sources("harn-modules", harn_modules::SOURCE_FINGERPRINT),
            SourceContribution::crate_sources(
                "harn-builtin-meta",
                harn_builtin_meta::SOURCE_FINGERPRINT,
            ),
            SourceContribution::crate_sources(
                "harn-builtin-registry",
                harn_builtin_registry::SOURCE_FINGERPRINT,
            ),
            SourceContribution::crate_sources(
                "harn-capability-contracts",
                harn_capability_contracts::SOURCE_FINGERPRINT,
            ),
            SourceContribution {
                name: "harn-vm-core",
                fingerprint: env!("HARN_CORE_COMPILER_SOURCE_FINGERPRINT"),
                roots: crate::compiler_sources::CORE_COMPILER_PATHS,
            },
        ],
    }
};

struct Cached {
    manifest: Arc<[&'static BuiltinManifestEntry]>,
    contracts: [u8; 32],
    compiler: Arc<str>,
}

fn current() -> &'static Mutex<Option<Cached>> {
    static CURRENT: OnceLock<Mutex<Option<Cached>>> = OnceLock::new();
    CURRENT.get_or_init(|| Mutex::new(None))
}

fn with_identity<R>(read: impl FnOnce(&Cached) -> R) -> R {
    harn_builtin_registry::install_builtin_manifest(crate::vm::all_builtin_manifest());
    let manifest = harn_builtin_registry::installed_manifest();
    let mut cached = current()
        .lock()
        .expect("compiler contract identity lock poisoned");
    if !cached
        .as_ref()
        .is_some_and(|entry| Arc::ptr_eq(&entry.manifest, &manifest))
    {
        let mut contracts = Sha256::new();
        contracts.update(b"harn.complete-builtin-contracts.v1\0");
        contracts.update(fingerprint_entries(&manifest));
        contracts.update(runtime_contracts());
        let contracts: [u8; 32] = contracts.finalize().into();
        let mut compiler = Sha256::new();
        compiler.update(b"harn.compiler-with-contracts.v1\0");
        compiler.update(super::CODEGEN_FINGERPRINT.as_bytes());
        compiler.update(contracts);
        *cached = Some(Cached {
            manifest,
            contracts,
            compiler: Arc::from(hex::encode(compiler.finalize())),
        });
    }
    read(cached.as_ref().expect("compiler identity initialized"))
}

pub(super) fn fingerprint() -> [u8; 32] {
    with_identity(|entry| entry.contracts)
}

/// Compiler source and the complete linked builtin signature/authority set.
/// Artifacts from a core-only backend cannot be replayed by a native backend.
pub fn compiler_fingerprint() -> Arc<str> {
    with_identity(|entry| entry.compiler.clone())
}

fn fingerprint_entries(entries: &[&BuiltinManifestEntry]) -> [u8; 32] {
    let mut entries = entries.to_vec();
    entries.sort_unstable_by_key(|entry| entry.name);
    let mut digest = ContractHasher(Sha256::new());
    digest.write(b"harn.builtin-contracts.v1\0");
    entries.hash(&mut digest);
    digest.0.finalize().into()
}

fn runtime_contracts() -> &'static [u8; 32] {
    static RUNTIME: OnceLock<[u8; 32]> = OnceLock::new();
    RUNTIME.get_or_init(|| fingerprint_runtime_defs(crate::vm::all_builtin_defs()))
}

fn fingerprint_runtime_defs(definitions: &[&crate::vm::VmBuiltinDef]) -> [u8; 32] {
    let mut definitions = definitions
        .iter()
        .copied()
        .filter(|def| def.runtime_only)
        .collect::<Vec<_>>();
    definitions.sort_unstable_by_key(|def| def.sig.name);
    let mut digest = ContractHasher(Sha256::new());
    digest.write(b"harn.runtime-only-builtin-contracts.v1\0");
    for definition in definitions {
        (definition.sig, definition.contract, definition.aliases).hash(&mut digest);
    }
    digest.0.finalize().into()
}

struct ContractHasher(Sha256);

macro_rules! integer_writers {
    ($($method:ident: $ty:ty),* $(,)?) => {
        $(fn $method(&mut self, value: $ty) { self.write(&value.to_le_bytes()); })*
    };
}

impl Hasher for ContractHasher {
    fn finish(&self) -> u64 {
        let digest = self.0.clone().finalize();
        u64::from_le_bytes(digest[..8].try_into().expect("SHA-256 holds eight bytes"))
    }
    fn write(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }
    fn write_usize(&mut self, value: usize) {
        self.write_u64(value as u64);
    }
    fn write_isize(&mut self, value: isize) {
        self.write_i64(value as i64);
    }
    integer_writers! {
        write_u8: u8, write_u16: u16, write_u32: u32, write_u64: u64, write_u128: u128,
        write_i8: i8, write_i16: i16, write_i32: i32, write_i64: i64, write_i128: i128,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harn_builtin_meta::{BuiltinContract, BuiltinSignature, Ty};

    const SIGNATURE: BuiltinSignature = BuiltinSignature::simple("probe", &[], Ty::Named("int"));
    static PURE: BuiltinManifestEntry = BuiltinManifestEntry {
        name: "probe",
        canonical_name: "probe",
        signature: &SIGNATURE,
        contract: BuiltinContract::PURE,
    };
    #[test]
    fn contract_identity_tracks_authority_and_signature_and_ignores_link_order() {
        let private = BuiltinManifestEntry {
            contract: BuiltinContract::privileged_wire(&[]),
            ..PURE
        };
        assert_ne!(
            fingerprint_entries(&[&PURE]),
            fingerprint_entries(&[&private])
        );
        let signature = BuiltinSignature::simple("probe", &[], Ty::Named("string"));
        let signature = Box::leak(Box::new(signature));
        let changed = BuiltinManifestEntry { signature, ..PURE };
        assert_ne!(
            fingerprint_entries(&[&PURE]),
            fingerprint_entries(&[&changed])
        );
        let neighbor = BuiltinManifestEntry {
            name: "neighbor",
            ..PURE
        };
        assert_eq!(
            fingerprint_entries(&[&PURE, &neighbor]),
            fingerprint_entries(&[&neighbor, &PURE])
        );
        assert_ne!(fingerprint_entries(&[&PURE]), fingerprint_entries(&[]));
    }

    #[test]
    fn runtime_only_authority_is_part_of_artifact_identity() {
        let internal = crate::vm::VmBuiltinDef {
            contract: BuiltinContract::RUNTIME_INTERNAL,
            runtime_only: true,
            ..crate::vm::VmBuiltinDef::new(SIGNATURE, crate::vm::VmBuiltinHandler::None)
        };
        let stdlib = crate::vm::VmBuiltinDef {
            contract: BuiltinContract::STDLIB_INTERNAL,
            ..internal
        };
        assert_ne!(
            fingerprint_runtime_defs(&[&internal]),
            fingerprint_runtime_defs(&[&stdlib])
        );
        assert_ne!(
            fingerprint_runtime_defs(&[&internal]),
            fingerprint_runtime_defs(&[])
        );
    }
}
