//! Persisted module interfaces: the walk's answer, kept with its proof.

use std::borrow::Cow;
use std::io;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::{
    cache_dir, cache_enabled, compiler_options_tag, deserialize_cache_payload, encode_artifact,
    ensure_parent_dir, read_header_if_matches, seed_entry_context_hasher, serialize_cache_payload,
    sha256, CacheKey, ARTIFACT_MODE, CODEGEN_FINGERPRINT, HARN_VERSION, INTERFACE_CACHE_EXTENSION,
    KIND_MODULE_INTERFACE,
};
use crate::compiler::CompilerOptions;
use crate::context_manifest::{ContextManifest, ManifestCheck};
use crate::module_artifact::{ModuleCompilationContext, ModuleProvenance};
use crate::module_source;

/// One module's persisted imported interface, in one host tree.
///
/// Deriving a module's interface walks and parses its whole import closure,
/// because lowering consults names its dependencies export. Without a stored
/// answer every process asked that question of the same unchanged tree again,
/// so a warm run re-parsed every module of a large graph only to rebuild keys
/// for artifacts it then loaded from disk (#9403). The walk's answer is stored
/// with the manifest the walk produced, and a later process re-checks that
/// manifest with stats instead of walking: the proof, and the trust boundary,
/// that already let an entry chunk skip its walk.
///
/// The key names the module's bytes, its canonical path, the embedded stdlib,
/// and the compiler build. The path is in the key because an interface depends
/// on where imports resolve from: two byte-identical modules in different
/// directories have different interfaces and must not overwrite each other's
/// record. The manifest is still checked against the same anchor, so a record
/// can only ever vouch for the file it was walked from.
pub(crate) struct InterfaceSlot {
    path: PathBuf,
    key: CacheKey,
    anchor: PathBuf,
}

/// Borrowed form of [`InterfacePayload`], so a store serializes without
/// cloning the manifest.
#[derive(serde::Serialize)]
struct InterfacePayloadRef<'a> {
    context: &'a ModuleCompilationContext,
    manifest: &'a ContextManifest,
}

#[derive(serde::Deserialize)]
struct InterfacePayload {
    context: ModuleCompilationContext,
    manifest: ContextManifest,
}

impl InterfaceSlot {
    /// The slot for `source_path` holding `source`, or `None` when the cache
    /// is off and there is nowhere to read or write one.
    pub(crate) fn open(source_path: &Path, source: &str) -> Option<Self> {
        if !cache_enabled() {
            return None;
        }
        let dir = cache_dir()?;
        let anchor = module_source::canonical_identity(source_path);
        let key = CacheKey {
            source_hash: sha256(source.as_bytes()),
            context_hash: interface_context_hash(&anchor, CODEGEN_FINGERPRINT),
            harn_version: Cow::Borrowed(HARN_VERSION),
            compiler_tag: compiler_options_tag(CompilerOptions::from_env()),
            provenance: ModuleProvenance::User,
        };
        Some(Self {
            path: dir.join(key.identity_filename(INTERFACE_CACHE_EXTENSION)),
            key,
            anchor,
        })
    }

    /// The stored interface, when its manifest proves the closure unchanged.
    ///
    /// Any header mismatch, undecodable payload, or stale manifest is a miss,
    /// and the caller walks as it always did, so a record can only save work.
    pub(crate) fn recall(&self) -> Option<(ModuleCompilationContext, ContextManifest)> {
        let header = read_header_if_matches(&self.path, &self.key, Some(&self.key.context_hash))
            .ok()
            .flatten()?;
        if header.kind != KIND_MODULE_INTERFACE {
            return None;
        }
        let payload: InterfacePayload = deserialize_cache_payload(&header.payload).ok()?;
        match payload.manifest.check(&self.anchor) {
            ManifestCheck::Valid => Some((payload.context, payload.manifest)),
            // Settle racily clean entries so the next process decides by stats.
            ManifestCheck::ValidAfterRecheck { refreshed } => {
                let _ = self.write(&payload.context, &refreshed);
                Some((payload.context, refreshed))
            }
            ManifestCheck::Stale => None,
        }
    }

    /// Persist a freshly walked interface.
    pub(crate) fn store(&self, context: &ModuleCompilationContext, manifest: &ContextManifest) {
        // The walk anchors its manifest at this same canonical identity; a
        // record whose manifest named another entry could never re-check.
        if manifest.entry != self.anchor {
            return;
        }
        if let Err(err) = self.write(context, manifest) {
            if std::env::var_os("HARN_BYTECODE_CACHE_DEBUG").is_some() {
                eprintln!(
                    "[harn] module interface cache write skipped for {}: {err}",
                    self.anchor.display()
                );
            }
        }
    }

    fn write(
        &self,
        context: &ModuleCompilationContext,
        manifest: &ContextManifest,
    ) -> io::Result<()> {
        ensure_parent_dir(&self.path)?;
        let payload = serialize_cache_payload(&InterfacePayloadRef { context, manifest })?;
        let buf = encode_artifact(&self.key, KIND_MODULE_INTERFACE, &payload);
        crate::atomic_io::atomic_write_with_mode(&self.path, &buf, ARTIFACT_MODE)
    }

    /// Rewrite the stored record as if another compiler build of this version
    /// had produced it, payload untouched.
    #[cfg(test)]
    pub(crate) fn restamp_as_build(&self, codegen_fingerprint: &str) {
        let header = read_header_if_matches(&self.path, &self.key, Some(&self.key.context_hash))
            .expect("record readable")
            .expect("record present");
        let buf = super::encode_artifact_fingerprinted(
            &self.key,
            KIND_MODULE_INTERFACE,
            &header.payload,
            codegen_fingerprint,
        );
        crate::atomic_io::atomic_write_with_mode(&self.path, &buf, ARTIFACT_MODE)
            .expect("record rewritten");
    }
}

/// Context half of an interface record's key: everything besides the module's
/// own bytes that the derived interface is a function of and no manifest can
/// observe.
fn interface_context_hash(anchor: &Path, codegen_fingerprint: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"module-interface-v1\0");
    seed_entry_context_hasher(&mut hasher, codegen_fingerprint);
    hasher.update(b"anchor\0");
    hasher.update(anchor.to_string_lossy().as_bytes());
    hasher.finalize().into()
}
