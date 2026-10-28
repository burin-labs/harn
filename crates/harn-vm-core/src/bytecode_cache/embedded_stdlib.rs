//! Embedded-stdlib module artifacts compiled into a host binary.
//!
//! The stdlib is compiled from source embedded in the binary, so a first run
//! on a new machine compiled every stdlib module it reached and wrote each one
//! into the shared cache: 148 files and about 13 s for an agent-stack script
//! on an unoptimized build (harn#9652). Those artifacts are keyed by embedded
//! content, Harn version, compiler fingerprint and compiler options, with no
//! path in the key, so the same build produces the same bytes on every
//! machine and OS. A host that links `harn-stdlib-artifacts` compiles them at
//! build time and registers the table here once at startup; the loader then
//! answers a stdlib import from memory before it reads the shared cache.
//!
//! The table is read-only and judged by the same header check as a cache
//! file. An entry whose key no longer matches (another Harn version, compiler
//! fingerprint, or compiler options) is a miss, and the import takes the path
//! it took before: the shared cache, then a compile.
//!
//! Each artifact is stored zstd-compressed, about a quarter of its size, and
//! decoded when its module is first imported. The caller keeps the decoded
//! module for the rest of the process.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use super::header::{read_artifact_build, ArtifactBuild};
use super::{artifact_reads_switched_on, module_from_header, read_header_from, CacheKey};
use crate::module_artifact::ModuleArtifact;

/// One stdlib module artifact embedded in a host binary, stored under the
/// file name the shared cache would give it.
#[derive(Debug, Clone, Copy)]
pub struct EmbeddedStdlibArtifact {
    pub file_name: &'static str,
    /// The artifact's size once decoded.
    pub len: usize,
    /// The artifact, compressed as one zstd frame.
    pub zstd: &'static [u8],
}

impl EmbeddedStdlibArtifact {
    /// The artifact's bytes, or `None` after warning when they do not decode
    /// to exactly `len` bytes. The import then compiles, much more slowly, so
    /// a broken table says so instead of only costing time.
    fn decode(&self) -> Option<Vec<u8>> {
        match zstd::bulk::decompress(self.zstd, self.len) {
            Ok(bytes) if bytes.len() == self.len => Some(bytes),
            outcome => {
                static WARNED: AtomicBool = AtomicBool::new(false);
                if !WARNED.swap(true, Ordering::Relaxed) {
                    let problem = outcome.map_or_else(
                        |error| error.to_string(),
                        |bytes| format!("{} bytes, expected {}", bytes.len(), self.len),
                    );
                    eprintln!(
                        "warning: embedded stdlib artifact {} does not decode ({problem}); \
                         stdlib imports compile instead",
                        self.file_name
                    );
                }
                None
            }
        }
    }
}

static EMBEDDED_STDLIB_ARTIFACTS: OnceLock<&'static [EmbeddedStdlibArtifact]> = OnceLock::new();

/// Why [`register_embedded_stdlib_artifacts`] refused a table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmbeddedStdlibRegistrationError {
    /// A table is already registered in this process.
    AlreadyRegistered,
    /// Entries must be sorted by file name with no duplicates, so a lookup is
    /// a binary search. Carries the first out-of-order name.
    Unsorted(&'static str),
    /// The table was compiled by another Harn build or builtin contract, so
    /// every lookup would miss. The usual cause is a host whose `harn-vm`
    /// features differ from the ones `harn-stdlib-artifacts` compiled with.
    ForeignBuild { embedded: String, running: String },
}

impl std::fmt::Display for EmbeddedStdlibRegistrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyRegistered => {
                f.write_str("an embedded stdlib table is already registered")
            }
            Self::Unsorted(name) => write!(f, "embedded stdlib table is not sorted at {name}"),
            Self::ForeignBuild { embedded, running } => write!(
                f,
                "embedded stdlib was compiled by {embedded}, but this binary runs {running}; \
                 select the same harn-vm features on harn-stdlib-artifacts"
            ),
        }
    }
}

impl std::error::Error for EmbeddedStdlibRegistrationError {}

fn describe(build: &ArtifactBuild) -> String {
    format!(
        "harn {} (schema {}, compiler {:.12}, builtin contracts {:.12})",
        build.harn_version,
        build.schema,
        build.codegen_fingerprint,
        hex::encode(build.compiler_contracts),
    )
}

/// Register the stdlib artifacts a host compiled into its binary. Call once,
/// before the first stdlib import; a second table is refused, and so is one
/// this binary's compiler would never accept.
pub fn register_embedded_stdlib_artifacts(
    table: &'static [EmbeddedStdlibArtifact],
) -> Result<(), EmbeddedStdlibRegistrationError> {
    if let Some(pair) = table
        .windows(2)
        .find(|pair| pair[0].file_name >= pair[1].file_name)
    {
        return Err(EmbeddedStdlibRegistrationError::Unsorted(pair[1].file_name));
    }
    // One build script writes every row, so the first row speaks for the table.
    if let Some(first) = table.first() {
        let running = ArtifactBuild::running();
        match first.decode().as_deref().and_then(read_artifact_build) {
            Some(embedded) if embedded == running => {}
            embedded => {
                return Err(EmbeddedStdlibRegistrationError::ForeignBuild {
                    embedded: embedded.as_ref().map_or_else(
                        || format!("an unreadable header in {}", first.file_name),
                        describe,
                    ),
                    running: describe(&running),
                })
            }
        }
    }
    EMBEDDED_STDLIB_ARTIFACTS
        .set(table)
        .map_err(|_| EmbeddedStdlibRegistrationError::AlreadyRegistered)
}

/// The embedded artifact stored under `key`, when one is registered and its
/// header carries exactly this key. `HARN_BYTECODE_CACHE=0` turns it off with
/// every other artifact read, so a compiler change is always exercised. A
/// missing cache directory does not: the table needs none.
pub(crate) fn load_embedded_stdlib_module(
    key: &CacheKey,
    synthetic: &Path,
) -> Option<ModuleArtifact> {
    if !artifact_reads_switched_on() {
        return None;
    }
    lookup(EMBEDDED_STDLIB_ARTIFACTS.get()?, key, synthetic)
}

/// The artifact in `table` stored for exactly `key`.
pub(crate) fn lookup(
    table: &[EmbeddedStdlibArtifact],
    key: &CacheKey,
    synthetic: &Path,
) -> Option<ModuleArtifact> {
    let name = key.module_filename();
    let index = table
        .binary_search_by(|entry| entry.file_name.cmp(name.as_str()))
        .ok()?;
    read_module_from_bytes(&table[index].decode()?, key, synthetic)
}

/// The module artifact in `bytes`, when they were written for exactly `key`.
fn read_module_from_bytes(
    bytes: &[u8],
    key: &CacheKey,
    synthetic: &Path,
) -> Option<ModuleArtifact> {
    read_header_from(bytes, key, Some(&key.context_hash))
        .and_then(|header| module_from_header(header, synthetic))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::module_artifact::ModuleProvenance;
    use crate::module_source::ModuleSource;

    fn leak(rows: Vec<(String, Vec<u8>)>) -> &'static [EmbeddedStdlibArtifact] {
        let rows = rows
            .into_iter()
            .map(|(name, bytes)| EmbeddedStdlibArtifact {
                file_name: Box::leak(name.into_boxed_str()),
                len: bytes.len(),
                zstd: Box::leak(
                    zstd::bulk::compress(&bytes, 3)
                        .expect("compress fixture")
                        .into_boxed_slice(),
                ),
            })
            .collect::<Vec<_>>();
        Box::leak(rows.into_boxed_slice())
    }

    fn key_for(source: &str) -> CacheKey {
        CacheKey::from_embedded_stdlib_module_content_hash(
            ModuleSource::from_text(source).sha256(),
            ModuleProvenance::EmbeddedStdlib,
        )
    }

    #[test]
    fn a_compiled_artifact_answers_its_own_key_and_refuses_another_build() {
        let module = "cache-fixture";
        let source = "pub fn answer() { return 41 }";
        let compiled =
            crate::vm::compile_stdlib_module_artifact(module, source).expect("fixture compiles");
        let key = key_for(source);
        let table = leak(vec![(compiled.file_name.clone(), compiled.bytes.clone())]);
        let synthetic = Path::new("<stdlib>/cache-fixture.harn");
        assert!(
            lookup(table, &key, synthetic).is_some(),
            "the producer and the loader must agree on the key"
        );

        // An artifact another Harn build wrote under the same file name is a
        // miss, so the import compiles exactly as it did before embedding.
        let other_build = key.clone().for_artifact_version("0.0.0-other");
        let artifact = read_module_from_bytes(&compiled.bytes, &key, synthetic).expect("decodes");
        let foreign = leak(vec![(
            key.module_filename(),
            super::super::serialize_module_artifact(&other_build, &artifact).expect("encodes"),
        )]);
        assert!(lookup(foreign, &key, synthetic).is_none());

        // Bytes that do not decode are a miss too, never a panic.
        let corrupt = [EmbeddedStdlibArtifact {
            zstd: b"not a zstd frame",
            ..table[0]
        }];
        assert!(lookup(&corrupt, &key, synthetic).is_none());

        // Registration reads the same build identity, so a foreign table is
        // refused up front instead of registering and missing on every import.
        assert_eq!(
            read_artifact_build(&compiled.bytes),
            Some(ArtifactBuild::running())
        );
        assert!(matches!(
            register_embedded_stdlib_artifacts(foreign),
            Err(EmbeddedStdlibRegistrationError::ForeignBuild { .. })
        ));
        assert!(EMBEDDED_STDLIB_ARTIFACTS.get().is_none());
    }

    #[test]
    fn registration_refuses_an_unsorted_table() {
        let table = leak(vec![
            ("b".to_string(), Vec::new()),
            ("a".to_string(), Vec::new()),
        ]);
        assert_eq!(
            register_embedded_stdlib_artifacts(table),
            Err(EmbeddedStdlibRegistrationError::Unsorted("a"))
        );
        assert!(EMBEDDED_STDLIB_ARTIFACTS.get().is_none());
    }
}
