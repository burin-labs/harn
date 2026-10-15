use super::*;

fn generation_value(cache: &PreparedModuleCache, path: &Path) -> crate::value::VmValue {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut vm = crate::vm::test_runtime::new_vm();
    vm.set_prepared_module_generation(cache.clone());
    runtime.block_on(async {
        let exports = vm.load_module_exports(path).await.unwrap();
        vm.call_closure_pub(exports.get("value").unwrap(), &[])
            .await
            .unwrap()
    })
}

#[test]
fn warm_graph_preparation_reuses_parsed_sources() {
    let dir = tempfile::tempdir().unwrap();
    let library = dir.path().join("library.harn");
    let root = dir.path().join("main.harn");
    // Keep the artifact cold even when earlier test runs populated disk caches.
    let source = format!(
        "pub fn value() {{ return {:?} }}\n",
        library.display().to_string()
    );
    std::fs::write(&library, source).unwrap();
    std::fs::write(&root, "import \"./library\"\n").unwrap();
    let cache = PreparedModuleCache::default();
    let parses = || crate::module_artifact::MODULE_PARSES.with(std::cell::Cell::get);
    let reads = || crate::module_source::SOURCE_READS.with(std::cell::Cell::get);

    let before = parses();
    cache.prepare_import_graph(std::slice::from_ref(&root));
    assert!(
        parses() > before,
        "cold compilation must exercise the parse counter"
    );
    assert_eq!(cache.stats().insertions, 1, "the import must be prepared");

    // Positive control for the source-read instrument, through its owning path.
    let before = reads();
    crate::module_source::read(&library).unwrap();
    assert_eq!(reads(), before + 1);
    let (before_parses, before_reads) = (parses(), reads());
    cache.prepare_import_graph(std::slice::from_ref(&root));
    assert_eq!(
        cache.stats().hits,
        1,
        "the warm import must reach the cache"
    );
    assert_eq!(
        parses(),
        before_parses,
        "context projection must reuse the graph AST"
    );
    assert_eq!(
        reads(),
        before_reads,
        "preparation must reuse the graph's source bytes"
    );
}

#[test]
fn graph_generation_rejects_invalid_source_and_refreshes_after_repair() {
    let dir = tempfile::tempdir().unwrap();
    let library = dir.path().join("library.harn");
    let root = dir.path().join("main.harn");
    std::fs::write(&root, "import \"./library\"\n").unwrap();
    std::fs::write(&library, "pub fn value( {\n").unwrap();
    let cache = PreparedModuleCache::for_immutable_generation();
    cache.prepare_import_graph(std::slice::from_ref(&root));
    assert_eq!(
        cache.stats().insertions,
        0,
        "invalid imports remain uncached"
    );
    let error = cache
        .prepare_module_generation(std::slice::from_ref(&root))
        .unwrap_err();
    assert!(
        matches!(error, VmError::Runtime(ref message) if message.contains("Import parse error in")),
        "{error}"
    );

    let source = "pub fn value() -> int { return 42 }\n";
    std::fs::write(&library, source).unwrap();
    let generation = cache
        .prepare_module_generation(std::slice::from_ref(&root))
        .unwrap();
    assert_eq!(generation.source_modules, 2);
    assert_eq!(
        cache.source_snapshot()[&harn_modules::canonical_path(&library)].as_ref(),
        source
    );
    assert!(matches!(
        generation_value(&cache, &library),
        crate::value::VmValue::Int(42)
    ));

    let changed = "pub fn value() -> int { return 99 }\n";
    assert_eq!(source.len(), changed.len());
    let modified = std::fs::metadata(&library).unwrap().modified().unwrap();
    std::fs::write(&library, changed).unwrap();
    std::fs::File::options()
        .write(true)
        .open(&library)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(modified))
        .unwrap();
    assert_eq!(
        std::fs::metadata(&library).unwrap().modified().unwrap(),
        modified
    );
    let replacement = cache
        .prepare_module_generation(std::slice::from_ref(&root))
        .unwrap();
    assert_ne!(
        replacement.source_digest_blake3,
        generation.source_digest_blake3
    );
    assert_eq!(
        cache.source_snapshot()[&harn_modules::canonical_path(&library)].as_ref(),
        changed
    );
    assert!(matches!(
        generation_value(&cache, &library),
        crate::value::VmValue::Int(99)
    ));

    for (invalid, diagnostic) in [
        (changed.replace('}', "{"), "Import parse error in"),
        (changed.replace("99", "\"x"), "Import lex error in"),
    ] {
        std::fs::write(&library, changed).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&library)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
        assert_eq!(
            crate::module_source::read(&library).unwrap().as_str(),
            changed
        );
        assert_eq!(invalid.len(), changed.len());
        std::fs::write(&library, invalid).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&library)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
        let error = cache
            .prepare_module_generation(std::slice::from_ref(&root))
            .unwrap_err();
        assert!(
            matches!(error, VmError::Runtime(ref message) if message.contains(diagnostic)),
            "{error}"
        );
    }
}
