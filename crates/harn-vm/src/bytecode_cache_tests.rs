//! Tests for [`crate::bytecode_cache`].
//!
//! Split out of `bytecode_cache.rs` to keep that file under the source-length
//! cap; they are still the `tests` child module of it, so private items stay
//! reachable through `use super::*`.

use super::*;
use crate::compile_source;

#[test]
fn header_round_trips_chunk() {
    let chunk = compile_source("__io_println(\"hello\")").expect("compile");
    let key = CacheKey::from_source(Path::new("/tmp/example.harn"), "__io_println(\"hello\")");
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("entry.harnbc");
    store_at(&path, &key, &chunk).expect("write");
    let loaded = read_entry_candidate(&path, &key).unwrap();
    assert!(loaded.is_some(), "expected cached chunk to load");
}

#[test]
fn serialize_chunk_artifact_matches_store_at() {
    // `serialize_chunk_artifact` packages an artifact into a buffer for
    // in-memory consumers (e.g. `harn pack` writing into a tar.zst
    // bundle). The contract is: the resulting bytes match what
    // `store_at` would have written for the same key+chunk, so the
    // shipped artifact is byte-identical to the on-disk cache form.
    let chunk = compile_source("__io_println(\"hi\")").expect("compile");
    let key = CacheKey::from_source(Path::new("/tmp/pack.harn"), "__io_println(\"hi\")");
    let tmp = tempfile::tempdir().unwrap();
    let on_disk = tmp.path().join("pack.harnbc");
    store_at(&on_disk, &key, &chunk).expect("write");
    let on_disk_bytes = std::fs::read(&on_disk).unwrap();
    let in_memory_bytes = serialize_chunk_artifact(&key, &chunk).expect("serialize");
    assert_eq!(in_memory_bytes, on_disk_bytes);
}

#[test]
fn cache_payload_rejects_trailing_bytes() {
    let chunk = compile_source("1 + 1").expect("compile");
    let cached = chunk.freeze_for_cache();
    let mut payload = serialize_cache_payload(&cached).expect("serialize");
    payload.push(0xFF);

    assert!(deserialize_cache_payload::<CachedChunk>(&payload).is_err());
}

#[test]
fn header_mismatch_returns_none() {
    let chunk = compile_source("1 + 1").expect("compile");
    let key = CacheKey::from_source(Path::new("/tmp/a.harn"), "1 + 1");
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("a.harnbc");
    store_at(&path, &key, &chunk).expect("write");
    let other = CacheKey {
        source_hash: [0xAB; 32],
        context_hash: key.context_hash,
        harn_version: HARN_VERSION,
        compiler_tag: key.compiler_tag,
    };
    assert!(read_entry_candidate(&path, &other).unwrap().is_none());
}

#[test]
fn schema_mismatch_returns_none() {
    let chunk = compile_source("1 + 1").expect("compile");
    let key = CacheKey::from_source(Path::new("/tmp/schema.harn"), "1 + 1");
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("schema.harnbc");
    store_at(&path, &key, &chunk).expect("write");

    let mut bytes = std::fs::read(&path).expect("read cache");
    bytes[8..12].copy_from_slice(&(SCHEMA_VERSION - 1).to_le_bytes());
    std::fs::write(&path, bytes).expect("rewrite cache");

    assert!(read_entry_candidate(&path, &key).unwrap().is_none());
}

#[test]
fn compiler_tag_mismatch_returns_none() {
    let chunk = compile_source("1 + 1").expect("compile");
    let key = CacheKey::from_source(Path::new("/tmp/b.harn"), "1 + 1");
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("b.harnbc");
    store_at(&path, &key, &chunk).expect("write");
    let other = CacheKey {
        compiler_tag: key.compiler_tag ^ 0xFF,
        ..key
    };
    assert!(
        read_entry_candidate(&path, &other).unwrap().is_none(),
        "flipped HARN_DISABLE_OPTIMIZATIONS must not reuse a chunk \
         compiled under the opposite setting"
    );
}

#[test]
fn codegen_fingerprint_is_populated() {
    // In-workspace builds always hash real compiler sources, so the
    // fingerprint must be a non-empty digest; an empty value would silently
    // disable the within-version compiler-staleness guard.
    assert!(!CODEGEN_FINGERPRINT.is_empty());
}

#[test]
fn codegen_fingerprint_changes_cache_key() {
    // A compiler whose code-generation source differs must produce a
    // different cache key for the *same* user source, so a stale artifact
    // compiled by a prior compiler at the same version misses on load
    // rather than being replayed (#2621). The fingerprint is a compile-time
    // constant, so exercise the parameterized inner hash directly.
    let tmp = tempfile::tempdir().unwrap();
    let entry = tmp.path().join("entry.harn");
    std::fs::write(&entry, "__io_println(\"hi\")\n").unwrap();
    let source = std::fs::read_to_string(&entry).unwrap();
    let a = hash_transitive_user_imports_fingerprinted(&entry, &source, "compiler-A");
    let b = hash_transitive_user_imports_fingerprinted(&entry, &source, "compiler-B");
    let a_again = hash_transitive_user_imports_fingerprinted(&entry, &source, "compiler-A");
    assert_ne!(
        a, b,
        "differing compiler fingerprints must change the cache key"
    );
    assert_eq!(
        a, a_again,
        "an unchanged compiler fingerprint must be stable"
    );
}

#[test]
fn module_context_hash_tracks_codegen_fingerprint() {
    let first = module_compilation_context_hash_fingerprinted("compiler-A");
    let second = module_compilation_context_hash_fingerprinted("compiler-B");
    assert_ne!(
        first, second,
        "module artifacts must miss when compiler code generation changes"
    );
    assert_eq!(
        first,
        module_compilation_context_hash_fingerprinted("compiler-A"),
        "an unchanged module compilation context must be stable"
    );
}

#[test]
fn module_key_excludes_dependency_graph_while_entry_key_tracks_it() {
    let tmp = tempfile::tempdir().unwrap();
    let dependency = tmp.path().join("value.harn");
    let importer = tmp.path().join("reader.harn");
    let importer_source = "import { value } from \"./value\"\npub fn read() { return value() }\n";
    std::fs::write(&dependency, "pub fn value() { return 1 }\n").unwrap();
    std::fs::write(&importer, importer_source).unwrap();

    let entry_before = CacheKey::from_source(&importer, importer_source);
    let module_before = CacheKey::from_module_source(&ModuleSource::from_text(importer_source));
    let dependency_before = CacheKey::from_module_source(&ModuleSource::from_text(
        std::fs::read_to_string(&dependency).unwrap(),
    ));

    std::fs::write(&dependency, "pub fn value() { return 2 }\n").unwrap();
    let future = std::fs::metadata(&dependency).unwrap().modified().unwrap()
        + std::time::Duration::from_secs(10);
    std::fs::OpenOptions::new()
        .write(true)
        .open(&dependency)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(future))
        .unwrap();

    let entry_after = CacheKey::from_source(&importer, importer_source);
    let module_after = CacheKey::from_module_source(&ModuleSource::from_text(importer_source));
    let dependency_after = CacheKey::from_module_source(&ModuleSource::from_text(
        std::fs::read_to_string(&dependency).unwrap(),
    ));

    assert_ne!(
        entry_before, entry_after,
        "entry chunks compile the full graph and must track dependency edits"
    );
    assert_eq!(
        module_before, module_after,
        "a parent module artifact must not be invalidated by dependency contents"
    );
    assert_ne!(
        dependency_before, dependency_after,
        "the edited dependency must invalidate its own module artifact"
    );
}

#[test]
fn module_artifact_is_relocatable_and_rebinds_exact_source_path() {
    let source = "pub fn answer() { fn inner() { return 42 }; return inner() }\n";
    let first_path = Path::new("/workspace/first/module.harn");
    let second_path = Path::new("/workspace/second/module.harn");
    let key = CacheKey::from_module_source(&ModuleSource::from_text(source));

    let artifact = crate::module_artifact::compile_module_artifact_from_source(first_path, source)
        .expect("compile module");
    let first_source_file = first_path.display().to_string();
    let second_source_file = second_path.display().to_string();
    assert_eq!(
        artifact.functions["answer"].chunk.source_file.as_deref(),
        Some(first_source_file.as_str())
    );

    let tmp = tempfile::tempdir().unwrap();
    let cache_path = tmp.path().join(key.module_filename());
    store_module_at(&cache_path, &key, &artifact).expect("store module");
    let first_loaded = read_module_if_matches(&cache_path, &key, first_path)
        .expect("read first module")
        .expect("first module key matches");
    let second_loaded = read_module_if_matches(&cache_path, &key, second_path)
        .expect("read second module")
        .expect("second module key matches");
    assert_eq!(
        first_loaded.functions["answer"]
            .chunk
            .source_file
            .as_deref(),
        Some(first_source_file.as_str())
    );
    assert_eq!(
        second_loaded.functions["answer"]
            .chunk
            .source_file
            .as_deref(),
        Some(second_source_file.as_str())
    );
    let nested = second_loaded.functions["answer"]
        .chunk
        .functions
        .first()
        .expect("nested function survives artifact roundtrip");
    assert_eq!(
        nested.chunk.source_file.as_deref(),
        Some(second_source_file.as_str()),
        "rebinding must reach nested compiled functions"
    );
}

#[test]
fn source_local_module_artifact_round_trips() {
    let source = "import \"./dependency\"\npub fn answer() { return 42 }\n";
    let source_path = Path::new("/tmp/source-local-module.harn");
    let artifact = crate::module_artifact::compile_module_artifact_from_source(source_path, source)
        .expect("compile module");
    let key = CacheKey::from_module_source(&ModuleSource::from_text(source));
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("source-local-module.harnmod");

    store_module_at(&path, &key, &artifact).expect("write module artifact");
    let loaded = read_module_if_matches(&path, &key, source_path)
        .expect("read module artifact")
        .expect("matching artifact");

    assert_eq!(loaded.imports.len(), 1);
    assert_eq!(loaded.imports[0].path, "./dependency");
    assert!(loaded.public_exports.contains_key("answer"));
}

#[test]
fn module_artifact_payload_round_trips() {
    let source = "pub fn answer() { fn inner() { return 42 }; return inner() }\n";
    let source_path = Path::new("/tmp/module-payload.harn");
    let artifact = crate::module_artifact::compile_module_artifact_from_source(source_path, source)
        .expect("compile module");

    let payload = serialize_cache_payload(&artifact).expect("serialize module artifact");
    let round_tripped: ModuleArtifact =
        deserialize_cache_payload(&payload).expect("deserialize module artifact");

    assert_eq!(
        round_tripped.public_exports.get("answer"),
        Some(&harn_modules::DefKind::Function)
    );
    assert!(round_tripped.functions["answer"]
        .chunk
        .functions
        .iter()
        .any(|function| function.name == "inner"));
}

#[test]
fn cache_enabled_respects_env() {
    std::env::set_var(CACHE_ENABLED_ENV, "0");
    assert!(!cache_enabled());
    std::env::set_var(CACHE_ENABLED_ENV, "1");
    assert!(cache_enabled());
    std::env::remove_var(CACHE_ENABLED_ENV);
    assert!(cache_enabled());
}

#[test]
fn import_hash_is_stable_across_import_order() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("a.harn"),
        "pub fn a() -> int { return 1 }\n",
    )
    .unwrap();
    std::fs::write(
        tmp.path().join("b.harn"),
        "pub fn b() -> int { return 2 }\n",
    )
    .unwrap();
    let ab = tmp.path().join("entry_ab.harn");
    std::fs::write(
        &ab,
        "import \"./a\"\nimport \"./b\"\n__io_println(\"hi\")\n",
    )
    .unwrap();
    let ba = tmp.path().join("entry_ba.harn");
    std::fs::write(
        &ba,
        "import \"./b\"\nimport \"./a\"\n__io_println(\"hi\")\n",
    )
    .unwrap();
    let hash_ab = hash_transitive_user_imports(&ab, &std::fs::read_to_string(&ab).unwrap());
    let hash_ba = hash_transitive_user_imports(&ba, &std::fs::read_to_string(&ba).unwrap());
    assert_eq!(
        hash_ab, hash_ba,
        "import-graph hash must be order-independent so reordering imports \
         does not bust the cache"
    );
}

#[test]
fn import_hash_picks_up_nested_imports() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("leaf.harn"),
        "pub fn x() -> int { return 1 }\n",
    )
    .unwrap();
    std::fs::write(
        tmp.path().join("mid.harn"),
        "import \"./leaf\"\npub fn y() -> int { return 2 }\n",
    )
    .unwrap();
    let entry = tmp.path().join("entry.harn");
    std::fs::write(&entry, "import \"./mid\"\n__io_println(\"hi\")\n").unwrap();

    let before = hash_transitive_user_imports(&entry, &std::fs::read_to_string(&entry).unwrap());
    std::fs::write(
        tmp.path().join("leaf.harn"),
        "pub fn x() -> int { return 999 }\n",
    )
    .unwrap();
    let after = hash_transitive_user_imports(&entry, &std::fs::read_to_string(&entry).unwrap());
    assert_ne!(
        before, after,
        "editing a transitively-imported file must change the import-graph hash"
    );
}

#[test]
fn import_hash_sees_a_dependency_that_appears_between_walks() {
    // The sibling tests cover a dependency whose *contents* change. This
    // one covers an import whose *resolution* changes: unresolved first,
    // resolved once the file lands. The walk records unresolved imports as
    // a sentinel, so nothing about the entry source moves — only what the
    // import resolves to.
    //
    // Recomputed in the same process, because the invariant is that import
    // resolution is walk-local: no answer may outlive the walk that
    // produced it. The walk resolves once per import edge and can only
    // dedup afterwards, which makes a cross-walk resolution cache a
    // standing temptation (#5545) — one would keep reporting the sentinel
    // and serve stale bytecode for a graph that just gained a real
    // dependency.
    let tmp = tempfile::tempdir().unwrap();
    let entry = tmp.path().join("entry.harn");
    std::fs::write(&entry, "import \"./late\"\n__io_println(\"hi\")\n").unwrap();

    let missing = hash_transitive_user_imports(&entry, &std::fs::read_to_string(&entry).unwrap());
    std::fs::write(
        tmp.path().join("late.harn"),
        "pub fn late() -> int { return 1 }\n",
    )
    .unwrap();
    let present = hash_transitive_user_imports(&entry, &std::fs::read_to_string(&entry).unwrap());

    assert_ne!(
        missing, present,
        "an import that resolves only after the file appears must change the \
         import-graph hash"
    );
}

#[test]
fn identical_import_strings_in_different_directories_resolve_separately() {
    // An import string is only meaningful relative to the directory of the
    // file that wrote it, and the same string appears in many directories:
    // `./dep` is one of the most repeated edges in a real graph. Anything
    // that resolves imports by string alone lets one directory's `./dep`
    // answer for another's, silently collapsing two distinct modules into
    // one and pinning the entry to bytecode built from the wrong file.
    let tmp = tempfile::tempdir().unwrap();
    for (dir, body) in [("left", "return 1"), ("right", "return 2")] {
        std::fs::create_dir_all(tmp.path().join(dir)).unwrap();
        std::fs::write(
            tmp.path().join(dir).join("dep.harn"),
            format!("pub fn dep() -> int {{ {body} }}\n"),
        )
        .unwrap();
        std::fs::write(
            tmp.path().join(dir).join("mod.harn"),
            "import \"./dep\"\npub fn use_dep() -> int { return dep() }\n",
        )
        .unwrap();
    }
    let entry = tmp.path().join("entry.harn");
    std::fs::write(
        &entry,
        "import \"./left/mod\"\nimport \"./right/mod\"\n__io_println(\"hi\")\n",
    )
    .unwrap();

    // Edit each directory's `dep` in turn and require the hash to move both
    // times. Resolving by import string alone lets one `./dep` answer for
    // the other, which leaves exactly one of these two files out of the
    // graph entirely — but *which* one depends on traversal order, so
    // editing a single file is not a reliable falsifier. Editing both is.
    let mut hash = hash_transitive_user_imports(&entry, &std::fs::read_to_string(&entry).unwrap());
    for dir in ["left", "right"] {
        std::fs::write(
            tmp.path().join(dir).join("dep.harn"),
            "pub fn dep() -> int { return 999 }\n",
        )
        .unwrap();
        let next = hash_transitive_user_imports(&entry, &std::fs::read_to_string(&entry).unwrap());
        assert_ne!(
            hash, next,
            "editing {dir}/dep.harn must move the hash: each directory's \
             `./dep` resolves to its own file"
        );
        hash = next;
    }
}

/// Build a graph, compile it, and write the adjacent artifact with the
/// manifest the walk produced. Returns the entry path and its source.
fn seed_entry_with_manifest(tmp: &Path, dep_body: &str) -> (PathBuf, String) {
    let entry = tmp.join("entry.harn");
    let entry_source = "import \"./dep\"\n__io_println(\"hi\")\n".to_string();
    std::fs::write(&entry, &entry_source).unwrap();
    std::fs::write(tmp.join("dep.harn"), dep_body).unwrap();

    let (context_hash, manifest) =
        hash_transitive_user_imports_with_manifest(&entry, &entry_source);
    assert!(
        manifest.is_some(),
        "a graph of ordinary readable files must produce a manifest"
    );
    let key = CacheKey {
        source_hash: sha256(entry_source.as_bytes()),
        context_hash,
        harn_version: HARN_VERSION,
        compiler_tag: compiler_options_tag(CompilerOptions::from_env()),
    };
    let chunk = compile_source(&entry_source).expect("compile");
    write_atomic_chunk(
        &adjacent_cache_path(&entry).unwrap(),
        &key,
        &chunk,
        manifest.as_ref(),
    )
    .unwrap();
    (entry, entry_source)
}

/// Rewrite `path` with `body` while restoring its previous mtime, so the
/// file's stat identity is unchanged but its content is not.
fn edit_preserving_stat(path: &Path, body: &str) {
    let before = std::fs::metadata(path).unwrap();
    assert_eq!(
        before.len(),
        body.len() as u64,
        "this helper only models an edit stats cannot see"
    );
    std::fs::write(path, body).unwrap();
    let times = std::fs::FileTimes::new().set_modified(before.modified().unwrap());
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_times(times)
        .unwrap();
}

#[test]
fn a_valid_manifest_serves_the_chunk_without_walking_the_graph() {
    // Proving a negative — "the walk did not run" — by making a change only
    // the walk could notice. The dependency's content changes while its
    // length and mtime do not, so the import-graph hash would differ if it
    // were recomputed. A hit therefore means the manifest answered.
    //
    // This is also the design's one accepted blind spot, stated as a fact
    // rather than left implicit: the same one Cargo, Zig and Bazel accept,
    // and the one `module_source`'s in-process read memo already accepts.
    let tmp = tempfile::tempdir().unwrap();
    let (entry, entry_source) =
        seed_entry_with_manifest(tmp.path(), "pub fn v() -> int { return 111 }\n");

    edit_preserving_stat(
        &tmp.path().join("dep.harn"),
        "pub fn v() -> int { return 222 }\n",
    );

    let walks_before = WALKS_PERFORMED.with(|c| c.get());
    assert!(
        load(&entry, &entry_source).chunk.is_some(),
        "a manifest whose stats all still match must serve the chunk"
    );
    assert_eq!(
        WALKS_PERFORMED.with(|c| c.get()),
        walks_before,
        "a valid manifest must serve the chunk without walking the graph"
    );
}

#[test]
fn a_directory_shadowed_import_does_not_defeat_the_manifest() {
    // A real 377-module tree turned out to contain two of these — an
    // `import "./types"` where `types/` is a directory, which resolves and then
    // fails to read. Treating an unreadable node as "cannot describe this graph"
    // dropped the manifest entirely, so the fast path was never once taken on
    // the graph it was built for, and the change was a pure regression.
    //
    // Nothing about the result was wrong, which is why only a measurement
    // caught it. This pins the manifest's *presence*, not just its answer.
    let tmp = tempfile::tempdir().unwrap();
    let entry = tmp.path().join("entry.harn");
    let entry_source = "import \"./types\"\nimport \"./dep\"\n__io_println(\"hi\")\n".to_string();
    std::fs::write(&entry, &entry_source).unwrap();
    std::fs::write(
        tmp.path().join("dep.harn"),
        "pub fn v() -> int { return 1 }\n",
    )
    .unwrap();
    std::fs::create_dir(tmp.path().join("types")).unwrap();

    let (_hash, manifest) = hash_transitive_user_imports_with_manifest(&entry, &entry_source);
    let manifest = manifest.expect("an unreadable node must not discard the manifest");
    assert_eq!(
        manifest.unreadable.len(),
        1,
        "the directory-shadowed import must be recorded, not dropped"
    );
    let anchor = module_source::canonical_identity(&entry);
    assert!(
        manifest.still_valid(&anchor),
        "nothing changed, so the manifest must still validate"
    );

    // And it must still *invalidate* when that node stops being unreadable:
    // replacing the directory with a real module adds a dependency.
    std::fs::remove_dir(tmp.path().join("types")).unwrap();
    std::fs::write(tmp.path().join("types"), "pub fn t() -> int { return 2 }\n").unwrap();
    assert!(
        !manifest.still_valid(&anchor),
        "a path that became readable changes the graph and must invalidate"
    );
}

#[test]
fn an_edited_dependency_still_misses() {
    // The manifest must not weaken the ordinary case: a real edit changes
    // length, the manifest rejects it, and the walk that follows finds a
    // different context hash.
    let tmp = tempfile::tempdir().unwrap();
    let (entry, entry_source) =
        seed_entry_with_manifest(tmp.path(), "pub fn v() -> int { return 1 }\n");

    std::fs::write(
        tmp.path().join("dep.harn"),
        "pub fn v() -> int { return 1234567 }\n",
    )
    .unwrap();

    assert!(
        load(&entry, &entry_source).chunk.is_none(),
        "an edited dependency must not be served from cache"
    );
}

#[test]
fn a_touched_dependency_refreshes_the_manifest_instead_of_re_walking_forever() {
    // `touch`, `git checkout` and `cp -p` all move mtime without changing
    // content. The key is unchanged, so the chunk is still good — but the
    // manifest no longer describes the tree, and without a refresh every
    // later spawn would pay for the walk again.
    let tmp = tempfile::tempdir().unwrap();
    let dep = tmp.path().join("dep.harn");
    let (entry, entry_source) =
        seed_entry_with_manifest(tmp.path(), "pub fn v() -> int { return 1 }\n");

    let stored_mtime = std::fs::metadata(&dep).unwrap().modified().unwrap();
    let bumped = stored_mtime + std::time::Duration::from_secs(5);
    std::fs::File::options()
        .write(true)
        .open(&dep)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(bumped))
        .unwrap();

    assert!(
        load(&entry, &entry_source).chunk.is_some(),
        "content is unchanged, so the chunk is still valid"
    );

    let refreshed = read_entry_candidate(
        &adjacent_cache_path(&entry).unwrap(),
        &CacheKey {
            source_hash: sha256(entry_source.as_bytes()),
            context_hash: [0u8; 32],
            harn_version: HARN_VERSION,
            compiler_tag: compiler_options_tag(CompilerOptions::from_env()),
        },
    )
    .unwrap()
    .expect("artifact is still readable");
    let recorded = refreshed
        .manifest
        .expect("refresh must write a manifest back")
        .files
        .iter()
        .find(|f| f.path.ends_with("dep.harn"))
        .expect("dep is in the manifest")
        .mtime_ns;
    assert_eq!(
        recorded,
        module_source::stat_identity(&dep).unwrap().1,
        "the rewritten manifest must record the mtime now on disk"
    );
}

#[test]
fn import_hash_busts_on_same_length_edit_in_same_process() {
    // The per-file read/scan memo is keyed by `(path, len, mtime_ns)`. The
    // hardest case for that key is an edit that preserves byte length: only
    // the mtime distinguishes the two versions. Guard that a same-length edit
    // to a transitively-imported file, recomputed in the SAME process so the
    // memo is warm, still busts the import-graph hash. Without a working
    // staleness check a warm long-lived process would replay stale bytecode.
    let tmp = tempfile::tempdir().unwrap();
    let leaf = tmp.path().join("leaf.harn");
    std::fs::write(&leaf, "pub fn x() -> int { return 111 }\n").unwrap();
    let entry = tmp.path().join("entry.harn");
    std::fs::write(&entry, "import \"./leaf\"\n__io_println(\"hi\")\n").unwrap();

    let before = hash_transitive_user_imports(&entry, &std::fs::read_to_string(&entry).unwrap());

    // Same byte length (`111` -> `222`), so the memo must rely on mtime.
    // Instead of sleeping out the coarsest plausible mtime granularity,
    // push the rewritten file's mtime deterministically into the future so
    // the `(path, len, mtime_ns)` stat key changes instantly on every
    // filesystem this runs on.
    std::fs::write(&leaf, "pub fn x() -> int { return 222 }\n").unwrap();
    // Bump from the file's own current mtime by a fixed margin instead of
    // sleeping or using a large absolute timestamp literal.
    let future =
        std::fs::metadata(&leaf).unwrap().modified().unwrap() + std::time::Duration::from_secs(10);
    std::fs::OpenOptions::new()
        .write(true)
        .open(&leaf)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(future))
        .unwrap();
    assert_eq!(
        std::fs::metadata(&leaf).unwrap().len(),
        33,
        "the two leaf versions must be the same byte length for this test to \
         exercise the mtime path"
    );

    let after = hash_transitive_user_imports(&entry, &std::fs::read_to_string(&entry).unwrap());
    assert_ne!(
        before, after,
        "a same-length edit to a transitively-imported file must still change \
         the import-graph hash when recomputed in a warm process"
    );
}

#[test]
fn import_hash_stable_across_repeated_calls_same_process() {
    // The memo must be a pure speed optimization: repeated `from_source`
    // calls over an unchanged tree (the cold-start module-load fan-out
    // pattern) must return byte-identical hashes.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("dep.harn"),
        "pub fn d() -> int { return 7 }\n",
    )
    .unwrap();
    let entry = tmp.path().join("entry.harn");
    std::fs::write(&entry, "import \"./dep\"\n__io_println(\"hi\")\n").unwrap();
    let src = std::fs::read_to_string(&entry).unwrap();
    let first = hash_transitive_user_imports(&entry, &src);
    for _ in 0..50 {
        assert_eq!(
            hash_transitive_user_imports(&entry, &src),
            first,
            "repeated import-graph hashing over an unchanged tree must be stable"
        );
    }
}

/// Seeds `dir` with an entry importing `./dep`, where the entry text is
/// identical for every caller and only the dependency body varies.
///
/// That is the shape #5591 turns on: the entry-chunk cache names files by entry
/// *source* hash alone, so two trees whose entries are byte-identical share one
/// cache identity while having genuinely different graphs.
fn seed_shared_identity_entry(dir: &Path, dep_body: &str) -> (PathBuf, String) {
    let entry = dir.join("entry.harn");
    let entry_source = "import \"./dep\"\n__io_println(\"same\")\n".to_string();
    std::fs::write(&entry, &entry_source).unwrap();
    std::fs::write(dir.join("dep.harn"), dep_body).unwrap();
    (entry, entry_source)
}

/// Compiles `entry` and writes its artifact beside it, manifest included.
///
/// Deliberately not `LookupOutcome::store`, which targets the shared cache
/// directory: these tests are about what happens when one entry reads another's
/// artifact, and writing into the developer's real cache would both leak and
/// let one run's leftovers decide the next one's answer.
fn store_beside(entry: &Path, source: &str) -> PathBuf {
    let (context_hash, manifest) = hash_transitive_user_imports_with_manifest(entry, source);
    let manifest = manifest.expect("a graph of ordinary readable files must produce a manifest");
    let key = CacheKey {
        source_hash: sha256(source.as_bytes()),
        context_hash,
        harn_version: HARN_VERSION,
        compiler_tag: compiler_options_tag(CompilerOptions::from_env()),
    };
    let artifact = adjacent_cache_path(entry).unwrap();
    let chunk = compile_source(source).expect("compile");
    write_atomic_chunk(&artifact, &key, &chunk, Some(&manifest)).unwrap();
    artifact
}

#[test]
fn a_manifest_does_not_vouch_for_a_different_entry() {
    // A manifest records stat identities of absolute paths, all of which stay
    // perfectly clean no matter which entry re-checks them. Only the anchor
    // distinguishes "this graph is unchanged" from "some graph is unchanged".
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let (entry_a, source) =
        seed_shared_identity_entry(a.path(), "pub fn v() -> int { return 1 }\n");
    let (entry_b, source_b) =
        seed_shared_identity_entry(b.path(), "pub fn v() -> int { return 2 }\n");
    assert_eq!(source, source_b, "the entries must be byte-identical");

    let (_hash, manifest) = hash_transitive_user_imports_with_manifest(&entry_a, &source);
    let manifest = manifest.expect("a graph of ordinary readable files must produce a manifest");

    assert!(
        manifest.still_valid(&module_source::canonical_identity(&entry_a)),
        "nothing moved under the entry it was walked from"
    );
    assert!(
        !manifest.still_valid(&module_source::canonical_identity(&entry_b)),
        "a manifest describing A's graph must not prove anything about B's"
    );
}

#[test]
fn an_artifact_from_an_identical_entry_elsewhere_is_not_served() {
    // End-to-end over `load`: the cache hands one entry an artifact another
    // entry wrote, because `CacheKey::filename` is the entry source hash and
    // nothing else. Here the artifact is planted at B's adjacent path rather
    // than in a shared directory, so the test needs no ambient cache state --
    // the candidate B reads is byte-for-byte the one A wrote either way, which
    // is the whole of the mechanism.
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let (entry_a, source) =
        seed_shared_identity_entry(a.path(), "pub fn v() -> int { return 1 }\n");
    let (entry_b, _) = seed_shared_identity_entry(b.path(), "pub fn v() -> int { return 992 }\n");

    let artifact = store_beside(&entry_a, &source);

    // A re-runs and takes the manifest fast path: the positive control, without
    // which a miss below would prove nothing.
    assert!(
        load(&entry_a, &source).chunk.is_some(),
        "A's own artifact must still validate for A"
    );

    // Now B finds exactly that artifact. Its own graph differs.
    std::fs::copy(&artifact, adjacent_cache_path(&entry_b).unwrap()).unwrap();
    assert!(
        load(&entry_b, &source).chunk.is_none(),
        "B must not run bytecode compiled against A's graph"
    );
}

#[test]
fn an_artifact_from_another_build_of_this_version_is_not_served() {
    // The manifest fast path proves the *graph* is unchanged and stops there,
    // so a chunk emitted by a different compiler at the same release -- every
    // edit to the lexer, parser, IR or codegen during development -- has an
    // immaculate manifest and nothing else to fail on. `harn_version` does not
    // move within a release and `compiler_tag` only tracks `CompilerOptions`,
    // which leaves the header's fingerprint as the one field that can notice.
    // See #5610, and #2621 for the class it belongs to.
    let tmp = tempfile::tempdir().unwrap();
    let (entry, source) =
        seed_shared_identity_entry(tmp.path(), "pub fn v() -> int { return 7 }\n");
    let (context_hash, manifest) = hash_transitive_user_imports_with_manifest(&entry, &source);
    let manifest = manifest.expect("a graph of ordinary readable files must produce a manifest");
    let key = CacheKey {
        source_hash: sha256(source.as_bytes()),
        context_hash,
        harn_version: HARN_VERSION,
        compiler_tag: compiler_options_tag(CompilerOptions::from_env()),
    };
    let payload = serialize_cache_payload(&EntryPayload {
        manifest: Some(manifest),
        chunk: compile_source(&source).expect("compile").freeze_for_cache(),
    })
    .unwrap();

    let artifact = adjacent_cache_path(&entry).unwrap();
    std::fs::write(
        &artifact,
        encode_artifact_fingerprinted(&key, KIND_ENTRY_CHUNK, &payload, "some-other-build"),
    )
    .unwrap();
    assert!(
        load(&entry, &source).chunk.is_none(),
        "a chunk this compiler did not emit must not be replayed"
    );

    // The same bytes under this build's fingerprint do load, so the miss above
    // is the fingerprint and not some other part of the artifact.
    std::fs::write(
        &artifact,
        encode_artifact_fingerprinted(&key, KIND_ENTRY_CHUNK, &payload, CODEGEN_FINGERPRINT),
    )
    .unwrap();
    assert!(
        load(&entry, &source).chunk.is_some(),
        "an artifact from this build must still take the fast path"
    );
}
