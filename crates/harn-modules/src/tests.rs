use super::*;
use std::fs;

fn write_file(dir: &Path, name: &str, contents: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, contents).unwrap();
    path
}

fn package_fixture(root: &Path) -> PathBuf {
    use crate::package_snapshot::{
        generation_root, package_current_path, package_publication_lock_path,
        PackageGenerationManifest, PackageGenerationPointer, GENERATION_LEASE_FILE,
        GENERATION_LOCK_FILE, GENERATION_MANIFEST_FILE, GENERATION_PACKAGES_DIR,
    };

    let generation = "generation-test";
    let generation_root = generation_root(root, generation);
    let packages_root = generation_root.join(GENERATION_PACKAGES_DIR);
    fs::create_dir_all(&packages_root).unwrap();
    fs::write(generation_root.join(GENERATION_LOCK_FILE), "version = 4\n").unwrap();
    fs::write(generation_root.join(GENERATION_LEASE_FILE), []).unwrap();
    let manifest = PackageGenerationManifest::new(
        generation,
        crate::package_snapshot::package_lock_digest(b"version = 4\n"),
    )
    .unwrap();
    fs::write(
        generation_root.join(GENERATION_MANIFEST_FILE),
        toml::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();
    let pointer = PackageGenerationPointer::new(generation).unwrap();
    fs::write(
        package_current_path(root),
        toml::to_string_pretty(&pointer).unwrap(),
    )
    .unwrap();
    fs::File::create(package_publication_lock_path(root)).unwrap();
    packages_root
}

#[test]
fn wave_parallel_build_matches_serial_semantics() {
    // Seed enough files to cross MIN_PARALLEL_WAVE so `load_wave` takes
    // the threaded path, and verify the graph resolves exactly as the
    // serial walk always did: every seed sees the shared module's export
    // and the shared module knows all its importers.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_file(root, "shared.harn", "pub fn shared_fn() { 1 }\n");
    let seeds: Vec<PathBuf> = (0..12)
        .map(|i| {
            write_file(
                root,
                &format!("mod{i}.harn"),
                &format!(
                    "import {{ shared_fn }} from \"./shared\"\npub fn f{i}() {{ shared_fn() }}\n"
                ),
            )
        })
        .collect();

    let graph = build(&seeds);
    for seed in &seeds {
        let names = graph
            .imported_names_for_file(seed)
            .expect("seed imports should resolve");
        assert!(names.contains("shared_fn"));
    }
    let importers = graph.importers_of(&root.join("shared.harn"));
    assert_eq!(importers.len(), seeds.len());
}

#[test]
fn pub_const_and_let_are_exported() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_file(
        root,
        "consts.harn",
        "pub const MAX = 3\npub let SEED = 7\nconst PRIVATE = 9\n",
    );
    let consumer = write_file(
        root,
        "use.harn",
        "import { MAX, SEED } from \"./consts\"\nMAX\n",
    );

    let graph = build(std::slice::from_ref(&consumer));
    let names = graph
        .imported_names_for_file(&consumer)
        .expect("imports resolve");
    assert!(names.contains("MAX"), "pub const should be importable");
    assert!(names.contains("SEED"), "pub let should be importable");
    // A private const stays out of the export surface.
    let consts_exports = graph.exports_for_module(&root.join("consts.harn"));
    assert!(consts_exports.contains(&"MAX".to_string()));
    assert!(consts_exports.contains(&"SEED".to_string()));
    assert!(!consts_exports.contains(&"PRIVATE".to_string()));
}

#[test]
fn import_compile_failures_point_at_broken_module() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    // A syntax error makes the whole library fail to parse.
    write_file(
        root,
        "lib.harn",
        "pub fn ok() { 1 }\npub fn broken( {\n  2\n}\n",
    );
    let consumer = write_file(
        root,
        "main.harn",
        "import { ok } from \"./lib\"\npipeline test(task) { ok() }\n",
    );

    let graph = build(std::slice::from_ref(&consumer));
    let failures = graph.import_compile_failures(&consumer);
    assert_eq!(failures.len(), 1, "the broken import should be reported");
    assert_eq!(failures[0].import_raw_path, "./lib");
    assert!(
        failures[0]
            .module_path
            .to_string_lossy()
            .ends_with("lib.harn"),
        "failure must name the imported module, not the consumer"
    );

    // The consumer's undefined-name check falls back to conservative
    // `None` rather than flagging `ok` as undefined at its call site.
    assert!(
        graph.imported_names_for_file(&consumer).is_none(),
        "a broken import target should suppress the call-site undefined check"
    );
    assert_eq!(graph.selective_import_issues(&consumer), Vec::new());
}

#[test]
fn importers_of_finds_direct_dependents() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let leaf = write_file(root, "leaf.harn", "pub fn leaf() { 1 }\n");
    write_file(root, "a.harn", "import \"./leaf\"\nleaf()\n");
    write_file(root, "b.harn", "import { leaf } from \"./leaf\"\nleaf()\n");
    let entry = write_file(root, "entry.harn", "import \"./a\"\nimport \"./b\"\n");

    let graph = build(std::slice::from_ref(&entry));
    let importers = graph.importers_of(&leaf);
    let names: Vec<String> = importers
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert!(names.contains(&"a.harn".to_string()));
    assert!(names.contains(&"b.harn".to_string()));
    assert!(!names.contains(&"entry.harn".to_string()));
}

#[test]
fn recursive_build_loads_transitively_imported_modules() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_file(root, "leaf.harn", "pub fn leaf_fn() { 1 }\n");
    write_file(
        root,
        "mid.harn",
        "import \"./leaf\"\npub fn mid_fn() { leaf_fn() }\n",
    );
    let entry = write_file(root, "entry.harn", "import \"./mid\"\nmid_fn()\n");

    let graph = build(std::slice::from_ref(&entry));
    let imported = graph
        .imported_names_for_file(&entry)
        .expect("entry imports should resolve");
    // Wildcard import of mid exposes mid_fn (pub) but not leaf_fn.
    assert!(imported.contains("mid_fn"));
    assert!(!imported.contains("leaf_fn"));

    // The transitively loaded module is known to the graph even though
    // the seed only included entry.harn.
    let leaf_path = root.join("leaf.harn");
    assert!(graph.definition_of(&leaf_path, "leaf_fn").is_some());
}

#[test]
fn imported_names_returns_none_when_import_unresolved() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let entry = write_file(root, "entry.harn", "import \"./does_not_exist\"\n");

    let graph = build(std::slice::from_ref(&entry));
    assert!(graph.imported_names_for_file(&entry).is_none());
}

#[test]
fn selective_imports_contribute_only_requested_names() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_file(root, "util.harn", "pub fn a() { 1 }\npub fn b() { 2 }\n");
    let entry = write_file(root, "entry.harn", "import { a } from \"./util\"\n");

    let graph = build(std::slice::from_ref(&entry));
    let imported = graph
        .imported_names_for_file(&entry)
        .expect("entry imports should resolve");
    assert!(imported.contains("a"));
    assert!(!imported.contains("b"));
}

#[test]
fn private_selective_import_is_classified_when_module_has_pub() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_file(root, "lib.harn", "pub fn api() { 1 }\nfn helper() { 2 }\n");
    let entry = write_file(root, "entry.harn", "import { helper } from \"./lib\"\n");

    let graph = build(std::slice::from_ref(&entry));
    let issues = graph.selective_import_issues(&entry);
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].name, "helper");
    assert_eq!(issues[0].module, "./lib");
    assert_eq!(issues[0].kind, SelectiveImportIssueKind::Private);
    assert_eq!(
        issues[0].message(),
        "imported symbol `helper` is not exported by `./lib` — it is defined there but not `pub`"
    );
    assert_eq!(
        issues[0].help(),
        "mark `helper` as `pub` in `./lib` to export it"
    );

    // Importing the `pub` name is fine.
    let entry_ok = write_file(root, "entry_ok.harn", "import { api } from \"./lib\"\n");
    let graph_ok = build(std::slice::from_ref(&entry_ok));
    assert_eq!(graph_ok.selective_import_issues(&entry_ok), Vec::new());
}

#[test]
fn selective_import_from_zero_pub_module_is_flagged() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    // A module with no `pub` markers exports nothing — Harn has no
    // "public-by-default" fallback — so selectively importing any of its
    // functions is flagged just like importing a private name.
    write_file(root, "util.harn", "fn a() { 1 }\nfn b() { 2 }\n");
    let entry = write_file(root, "entry.harn", "import { a } from \"./util\"\n");

    let graph = build(std::slice::from_ref(&entry));
    let issues = graph.selective_import_issues(&entry);
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].name, "a");
    assert_eq!(issues[0].module, "./util");
    assert_eq!(issues[0].kind, SelectiveImportIssueKind::Private);
}

#[test]
fn absent_selective_type_import_is_classified_before_runtime() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_file(root, "types.harn", "pub type CurrentReceipt = {ok: bool}\n");
    let entry = write_file(
        root,
        "entry.harn",
        "import { StaleReceipt } from \"./types\"\npub fn grade(value: StaleReceipt) -> bool { true }\n",
    );

    let graph = build(std::slice::from_ref(&entry));
    let issues = graph.selective_import_issues(&entry);
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].name, "StaleReceipt");
    assert_eq!(issues[0].module, "./types");
    assert_eq!(issues[0].kind, SelectiveImportIssueKind::Missing);
    assert_eq!(
        issues[0].message(),
        "imported symbol `StaleReceipt` does not exist in `./types`"
    );
    assert_eq!(
        issues[0].help(),
        "update the import to a symbol exported by `./types`"
    );
}

#[test]
fn unused_absent_selective_value_import_is_classified() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_file(root, "values.harn", "pub const CurrentValue = 1\n");
    let entry = write_file(
        root,
        "entry.harn",
        "import { StaleValue } from \"./values\"\npub fn run() -> int { 1 }\n",
    );

    let graph = build(std::slice::from_ref(&entry));
    let issues = graph.selective_import_issues(&entry);
    assert_eq!(
        issues
            .iter()
            .map(|issue| (
                issue.name.as_str(),
                issue.module.as_str(),
                issue.kind,
                issue.message(),
                issue.help(),
            ))
            .collect::<Vec<_>>(),
        vec![(
            "StaleValue",
            "./values",
            SelectiveImportIssueKind::Missing,
            "imported symbol `StaleValue` does not exist in `./values`".to_string(),
            "update the import to a symbol exported by `./values`".to_string(),
        )]
    );
}

#[test]
fn source_override_drives_root_import_analysis() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_file(root, "types.harn", "pub type Receipt = {ok: bool}\n");
    let entry = write_file(root, "entry.harn", "import { Receipt } from \"./types\"\n");

    let graph = build_with_source(&entry, "import { StaleReceipt } from \"./types\"\n");
    let issues = graph.selective_import_issues(&entry);
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].name, "StaleReceipt");
    assert_eq!(issues[0].kind, SelectiveImportIssueKind::Missing);
}

#[test]
fn stdlib_imports_resolve_to_embedded_sources() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let entry = write_file(root, "entry.harn", "import \"std/math\"\nclamp(5, 0, 10)\n");

    let graph = build(std::slice::from_ref(&entry));
    let imported = graph
        .imported_names_for_file(&entry)
        .expect("std/math should resolve");
    // `clamp` is defined in stdlib_math.harn as `pub fn clamp(...)`.
    assert!(imported.contains("clamp"));
}

#[test]
fn stdlib_builtin_reexports_participate_in_static_resolution() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = write_file(
        tmp.path(),
        "entry.harn",
        "import { assert_eq } from \"std/testing\"\nassert_eq(1, 1)\n",
    );

    let graph = build(std::slice::from_ref(&entry));
    assert_eq!(graph.selective_import_issues(&entry), Vec::new());
    assert_eq!(
        graph.imported_names_for_file(&entry),
        Some(HashSet::from(["assert_eq".to_string()])),
    );
}

#[test]
fn stdlib_internal_imports_resolve_without_leaking_to_callers() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let entry = write_file(
        root,
        "entry.harn",
        "import { process_run } from \"std/runtime\"\nprocess_run([\"echo\", \"ok\"])\n",
    );

    let graph = build(std::slice::from_ref(&entry));
    let entry_imports = graph
        .imported_names_for_file(&entry)
        .expect("std/runtime should resolve");
    assert!(entry_imports.contains("process_run"));
    assert!(
        !entry_imports.contains("filter_nil"),
        "private std/runtime dependency leaked to caller"
    );

    let runtime_path = stdlib::stdlib_virtual_path("runtime");
    let runtime_imports = graph
        .imported_names_for_file(&runtime_path)
        .expect("std/runtime internal imports should resolve");
    assert!(runtime_imports.contains("filter_nil"));
}

#[test]
fn runtime_stdlib_import_surface_resolves_to_embedded_sources() {
    let tmp = tempfile::tempdir().unwrap();
    let entry_path = write_file(tmp.path(), "entry.harn", "");

    for source in harn_stdlib::STDLIB_SOURCES {
        let import_path = format!("std/{}", source.module);
        assert!(
            resolve_import_path(&entry_path, &import_path).is_some(),
            "{import_path} should resolve in the module graph"
        );
    }
}

#[test]
fn stdlib_imports_expose_type_declarations() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let entry = write_file(
        root,
        "entry.harn",
        "import \"std/triggers\"\nlet provider = \"github\"\n",
    );

    let graph = build(std::slice::from_ref(&entry));
    let decls = graph
        .imported_type_declarations_for_file(&entry)
        .expect("std/triggers type declarations should resolve");
    let names: HashSet<String> = decls
        .iter()
        .filter_map(type_decl_name)
        .map(ToString::to_string)
        .collect();
    assert!(names.contains("TriggerEvent"));
    assert!(names.contains("ProviderPayload"));
    assert!(names.contains("SignatureStatus"));
}

#[test]
fn stdlib_imports_expose_callable_declarations() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let entry = write_file(
        root,
        "entry.harn",
        "import { select_from } from \"std/tui\"\nlet item = \"alpha\"\n",
    );

    let graph = build(std::slice::from_ref(&entry));
    let decls = graph
        .imported_callable_declarations_for_file(&entry)
        .expect("std/tui callable declarations should resolve");
    let names: HashSet<String> = decls
        .iter()
        .filter_map(callable_decl_name)
        .map(ToString::to_string)
        .collect();
    assert!(names.contains("select_from"));
}

#[test]
fn stdlib_llm_catalog_exposes_routing_routes() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let entry = write_file(
        root,
        "entry.harn",
        "import { routing_routes } from \"std/llm/catalog\"\nrouting_routes()\n",
    );

    let graph = build(std::slice::from_ref(&entry));
    let imported = graph
        .imported_names_for_file(&entry)
        .expect("std/llm/catalog should resolve");
    assert!(imported.contains("routing_routes"));
    let decls = graph
        .imported_callable_declarations_for_file(&entry)
        .expect("std/llm/catalog callable declarations should resolve");
    let names: HashSet<String> = decls
        .iter()
        .filter_map(callable_decl_name)
        .map(ToString::to_string)
        .collect();
    assert!(names.contains("routing_routes"));
}

#[path = "tests/package_imports.rs"]
mod package_imports;

#[test]
fn import_cycles_do_not_loop_forever() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_file(root, "a.harn", "import \"./b\"\npub fn a_fn() { 1 }\n");
    write_file(root, "b.harn", "import \"./a\"\npub fn b_fn() { 1 }\n");
    let entry = root.join("a.harn");

    // Just ensuring this terminates and yields sensible names.
    let graph = build(std::slice::from_ref(&entry));
    let imported = graph
        .imported_names_for_file(&entry)
        .expect("cyclic imports still resolve to known exports");
    assert!(imported.contains("b_fn"));
}

#[test]
fn pub_import_selective_re_exports_named_symbols() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_file(
        root,
        "src.harn",
        "pub fn alpha() { 1 }\npub fn beta() { 2 }\n",
    );
    let facade = write_file(root, "facade.harn", "pub import { alpha } from \"./src\"\n");
    let entry = write_file(root, "entry.harn", "import \"./facade\"\nalpha()\n");

    let graph = build(std::slice::from_ref(&entry));
    assert_eq!(graph.selective_import_issues(&facade), Vec::new());
    let imported = graph
        .imported_names_for_file(&entry)
        .expect("entry should resolve");
    assert!(imported.contains("alpha"), "selective re-export missing");
    assert!(
        !imported.contains("beta"),
        "non-listed name leaked through facade"
    );

    let facade_path = root.join("facade.harn");
    let def = graph
        .definition_of(&facade_path, "alpha")
        .expect("definition_of should chase re-export");
    assert!(def.file.ends_with("src.harn"));
}

#[test]
fn pub_import_wildcard_re_exports_full_surface() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_file(
        root,
        "src.harn",
        "pub fn alpha() { 1 }\npub fn beta() { 2 }\n",
    );
    write_file(root, "facade.harn", "pub import \"./src\"\n");
    let entry = write_file(root, "entry.harn", "import \"./facade\"\nalpha()\n");

    let graph = build(std::slice::from_ref(&entry));
    let imported = graph
        .imported_names_for_file(&entry)
        .expect("entry should resolve");
    assert!(imported.contains("alpha"));
    assert!(imported.contains("beta"));
}

#[test]
fn pub_import_chain_resolves_definition_to_origin() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_file(root, "inner.harn", "pub fn deep() { 1 }\n");
    let middle = write_file(
        root,
        "middle.harn",
        "pub import { deep } from \"./inner\"\n",
    );
    let outer = write_file(
        root,
        "outer.harn",
        "pub import { deep } from \"./middle\"\n",
    );
    let entry = write_file(
        root,
        "entry.harn",
        "import { deep } from \"./outer\"\ndeep()\n",
    );

    let graph = build(std::slice::from_ref(&entry));
    assert_eq!(graph.selective_import_issues(&middle), Vec::new());
    assert_eq!(graph.selective_import_issues(&outer), Vec::new());
    assert_eq!(graph.selective_import_issues(&entry), Vec::new());
    let def = graph
        .definition_of(&entry, "deep")
        .expect("definition_of should follow chain");
    assert!(def.file.ends_with("inner.harn"));

    let imported = graph
        .imported_names_for_file(&entry)
        .expect("entry should resolve");
    assert!(imported.contains("deep"));
}

#[test]
fn duplicate_pub_import_reports_re_export_conflict() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_file(root, "a.harn", "pub fn shared() { 1 }\n");
    write_file(root, "b.harn", "pub fn shared() { 2 }\n");
    let facade = write_file(
        root,
        "facade.harn",
        "pub import { shared } from \"./a\"\npub import { shared } from \"./b\"\n",
    );

    let graph = build(std::slice::from_ref(&facade));
    let conflicts = graph.re_export_conflicts(&facade);
    assert_eq!(
        conflicts.len(),
        1,
        "expected exactly one re-export conflict, got {conflicts:?}"
    );
    assert_eq!(conflicts[0].name, "shared");
    assert_eq!(conflicts[0].sources.len(), 2);
}

#[test]
fn cross_directory_cycle_does_not_explode_module_count() {
    // Regression: two files in sibling directories that import each
    // other produced a fresh path spelling on every round-trip
    // (`../runtime/../context/../runtime/...`), and `build()`'s
    // `seen` set deduped on the raw spelling rather than the
    // canonical path. The walk only terminated when `PATH_MAX` was
    // hit — 1024 on macOS, 4096 on Linux — so Linux re-parsed the
    // same pair thousands of times until it ran out of memory.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let context = root.join("context");
    let runtime = root.join("runtime");
    fs::create_dir_all(&context).unwrap();
    fs::create_dir_all(&runtime).unwrap();
    write_file(
        &context,
        "a.harn",
        "import \"../runtime/b\"\npub fn a_fn() { 1 }\n",
    );
    write_file(
        &runtime,
        "b.harn",
        "import \"../context/a\"\npub fn b_fn() { 1 }\n",
    );
    let entry = context.join("a.harn");

    let graph = build(std::slice::from_ref(&entry));
    // The graph should contain exactly the two real files, keyed by
    // their canonical paths. Pre-fix this was thousands of entries.
    assert_eq!(
        graph.modules.len(),
        2,
        "cross-directory cycle loaded {} modules, expected 2",
        graph.modules.len()
    );
    let imported = graph
        .imported_names_for_file(&entry)
        .expect("cyclic imports still resolve to known exports");
    assert!(imported.contains("b_fn"));
}
