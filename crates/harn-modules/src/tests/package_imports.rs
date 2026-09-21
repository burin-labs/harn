use super::*;
use crate::package_snapshot::probe_counter;

#[test]
fn graph_classifies_only_reachable_nonlocal_imports_as_package_aliases() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_file(
        root,
        "relative.harn",
        "import \"transitive_dep/tools\"\npub fn local() { 1 }\n",
    );
    let entry = write_file(
        root,
        "entry.harn",
        "import \"std/runtime\"\nimport \"./relative\"\nimport \"./missing\"\nimport \"direct_dep/api\"\n",
    );

    let graph = build(std::slice::from_ref(&entry));

    assert_eq!(
        graph.package_import_aliases(),
        ["direct_dep".to_string(), "transitive_dep".to_string()]
    );
    let imports = graph.package_imports();
    assert!(imports.iter().any(|import| {
        import.alias == "direct_dep" && import.importer.file_name().unwrap() == "entry.harn"
    }));
    assert!(imports.iter().any(|import| {
        import.alias == "transitive_dep" && import.importer.file_name().unwrap() == "relative.harn"
    }));
}

#[test]
fn package_export_map_resolves_declared_module() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let packages_root = package_fixture(root);
    let packages = packages_root.join("acme/runtime");
    fs::create_dir_all(&packages).unwrap();
    fs::write(
        packages_root.join("acme/harn.toml"),
        "[exports]\ncapabilities = \"runtime/capabilities.harn\"\n",
    )
    .unwrap();
    fs::write(
        packages.join("capabilities.harn"),
        "pub fn exported_capability() { 1 }\n",
    )
    .unwrap();
    let entry = write_file(
        root,
        "entry.harn",
        "import \"acme/capabilities\"\nexported_capability()\n",
    );

    let graph = build(std::slice::from_ref(&entry));
    assert_eq!(
        graph.package_import_aliases(),
        ["acme".to_string()],
        "classification must retain the raw alias after a snapshot resolves it"
    );
    let imported = graph
        .imported_names_for_file(&entry)
        .expect("package export should resolve");
    assert!(imported.contains("exported_capability"));
}

/// Many files under one project root cost one acquire, not one per file.
///
/// The walk is a handful of stats; the acquire canonicalizes, takes two
/// shared flocks, parses two TOML files and re-reads plus SHA256s the
/// lockfile. Deduping roots AFTER acquiring — as this did — paid the
/// expensive half once per file and discarded all but one result. Every
/// real graph build resolves many files under a single root, so the waste
/// was the common case, not the edge case.
#[test]
fn many_files_under_one_root_acquire_a_single_snapshot() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    package_fixture(root);
    let files: Vec<PathBuf> = (0..8)
        .map(|i| write_file(root, &format!("entry{i}.harn"), ""))
        .collect();

    let (snapshots, walks, acquires) =
        probe_counter::count_walks_and_acquires(|| acquire_package_snapshots(&files));

    assert_eq!(snapshots.len(), 1, "one root must yield one snapshot");
    assert_eq!(walks, 8, "each file still needs its own cheap root walk");
    assert_eq!(
        acquires, 1,
        "the expensive acquire ran once per file instead of once per root"
    );
}

/// Distinct roots must still each get their own snapshot — otherwise the
/// dedup above could 'pass' by never acquiring at all.
#[test]
fn distinct_roots_each_acquire_their_own_snapshot() {
    let tmp = tempfile::tempdir().unwrap();
    let first = tmp.path().join("first");
    let second = tmp.path().join("second");
    for root in [&first, &second] {
        fs::create_dir_all(root).unwrap();
        fs::write(root.join(".git"), "").unwrap();
        package_fixture(root);
    }
    let files = vec![
        write_file(&first, "a.harn", ""),
        write_file(&first, "b.harn", ""),
        write_file(&second, "c.harn", ""),
    ];

    let (snapshots, _, acquires) =
        probe_counter::count_walks_and_acquires(|| acquire_package_snapshots(&files));

    assert_eq!(
        snapshots.len(),
        2,
        "each distinct root must yield a snapshot"
    );
    assert_eq!(
        acquires, 2,
        "one acquire per distinct root, no more, no fewer"
    );
}

/// Only a package import can be answered by a package, so only a package
/// import may pay to find one.
///
/// harn#4657 hoisted `PackageSnapshot::acquire_nearest` above the stdlib and
/// relative-path checks, so every `std/...` and every sibling import walked
/// its ancestors stat-ing for a package pointer, then opened, flocked and
/// parsed it — and discarded the snapshot unused. Those are the two
/// overwhelmingly common import shapes. It cost ~5x per-test module setup,
/// 1.8x on a downstream CI critical path, and it was invisible for three
/// releases because the wasted work changes nothing except wall time
/// (harn#4815).
#[test]
fn stdlib_and_relative_imports_never_probe_for_a_package() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    // A real installed package, so a probe would find something and the
    // test cannot pass merely because there is nothing to look for.
    package_fixture(root);
    write_file(root, "sibling.harn", "pub fn helper() { 1 }\n");
    let entry = write_file(root, "entry.harn", "");

    let (resolved, probes) =
        probe_counter::count_probes(|| resolve_import_path(&entry, "std/testing"));
    assert!(resolved.is_some(), "std/testing must still resolve");
    assert_eq!(
        probes, 0,
        "a std/ import probed the filesystem for a package"
    );

    let (resolved, probes) =
        probe_counter::count_probes(|| resolve_import_path(&entry, "./sibling"));
    assert!(resolved.is_some(), "a relative sibling must still resolve");
    assert_eq!(
        probes, 0,
        "a relative import probed the filesystem for a package"
    );

    let (resolved, probes) =
        probe_counter::count_probes(|| resolve_import_path(&entry, "./missing"));
    assert!(
        resolved.is_none(),
        "a missing relative import must not resolve"
    );
    assert_eq!(
        probes, 0,
        "a missing relative import fell through to package resolution"
    );
}

/// The counter above only means something if a real package import still
/// probes — otherwise the assertions would hold even with resolution
/// removed entirely.
#[test]
fn a_package_import_still_acquires_a_snapshot() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let packages_root = package_fixture(root);
    fs::create_dir_all(packages_root.join("acme")).unwrap();
    fs::write(
        packages_root.join("acme/capabilities.harn"),
        "pub fn exported_capability() { 1 }\n",
    )
    .unwrap();
    let entry = write_file(root, "entry.harn", "");

    let (resolved, probes) =
        probe_counter::count_probes(|| resolve_import_path(&entry, "acme/capabilities"));
    assert!(resolved.is_some(), "a package import must still resolve");
    assert_eq!(
        probes, 1,
        "a package import must acquire exactly one snapshot"
    );
}

/// A `std/` import that names no real module resolves to nothing and must
/// not fall through to package resolution — otherwise a package could
/// shadow the standard library namespace.
#[test]
fn an_unknown_stdlib_module_does_not_fall_through_to_packages() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let packages_root = package_fixture(root);
    fs::create_dir_all(packages_root.join("std")).unwrap();
    fs::write(
        packages_root.join("std/not_a_real_module.harn"),
        "pub fn impostor() { 1 }\n",
    )
    .unwrap();
    let entry = write_file(root, "entry.harn", "");

    let (resolved, probes) =
        probe_counter::count_probes(|| resolve_import_path(&entry, "std/not_a_real_module"));
    assert!(
        resolved.is_none(),
        "a package resolved a std/ import and shadowed the stdlib namespace"
    );
    assert_eq!(probes, 0, "an unknown std/ import probed for a package");
}

#[test]
fn package_direct_import_cannot_escape_packages_root() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    fs::create_dir_all(package_fixture(root).join("acme")).unwrap();
    fs::write(root.join("secret.harn"), "pub fn leaked() { 1 }\n").unwrap();
    let entry = write_file(root, "entry.harn", "");

    let resolved = resolve_import_path(&entry, "acme/../../secret");
    assert!(resolved.is_none(), "package import escaped package root");
}

#[test]
fn package_export_map_cannot_escape_package_root() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let packages_root = package_fixture(root);
    fs::create_dir_all(packages_root.join("acme")).unwrap();
    fs::write(root.join("secret.harn"), "pub fn leaked() { 1 }\n").unwrap();
    fs::write(
        packages_root.join("acme/harn.toml"),
        "[exports]\nleak = \"../../secret.harn\"\n",
    )
    .unwrap();
    let entry = write_file(root, "entry.harn", "");

    let resolved = resolve_import_path(&entry, "acme/leak");
    assert!(resolved.is_none(), "package export escaped package root");
}

#[test]
fn package_export_map_allows_symlinked_path_dependencies() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let source = root.join("source-package");
    fs::create_dir_all(source.join("runtime")).unwrap();
    fs::write(
        source.join("harn.toml"),
        "[exports]\ncapabilities = \"runtime/capabilities.harn\"\n",
    )
    .unwrap();
    fs::write(
        source.join("runtime/capabilities.harn"),
        "pub fn exported_capability() { 1 }\n",
    )
    .unwrap();
    let packages_root = package_fixture(root);
    #[cfg(unix)]
    std::os::unix::fs::symlink(&source, packages_root.join("acme")).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(&source, packages_root.join("acme")).unwrap();
    let entry = write_file(root, "entry.harn", "");

    let resolved = resolve_import_path(&entry, "acme/capabilities")
        .expect("symlinked package export should resolve");
    assert!(resolved.ends_with("runtime/capabilities.harn"));
}

#[test]
fn explicit_snapshot_resolves_from_a_path_dependency_outside_the_project() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("consumer");
    let source = tmp.path().join("source-package");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(source.join("runtime")).unwrap();
    fs::write(
        source.join("runtime/capabilities.harn"),
        "pub fn exported_capability() { 1 }\n",
    )
    .unwrap();
    let packages_root = package_fixture(&root);
    #[cfg(unix)]
    std::os::unix::fs::symlink(&source, packages_root.join("acme")).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(&source, packages_root.join("acme")).unwrap();
    let importing_file = write_file(&source, "entry.harn", "");
    let snapshot = PackageSnapshot::acquire(&root).unwrap().unwrap();

    let resolved =
        resolve_import_path_with_snapshot(&importing_file, "acme/runtime/capabilities", &snapshot)
            .expect("the caller-owned snapshot should resolve its path dependency");

    assert!(resolved.ends_with("runtime/capabilities.harn"));
}

#[test]
fn package_imports_resolve_from_nested_package_module() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    fs::create_dir_all(root.join(".git")).unwrap();
    let packages_root = package_fixture(root);
    fs::create_dir_all(packages_root.join("acme")).unwrap();
    fs::create_dir_all(packages_root.join("shared")).unwrap();
    fs::write(
        packages_root.join("shared/lib.harn"),
        "pub fn shared_helper() { 1 }\n",
    )
    .unwrap();
    fs::write(
        packages_root.join("acme/lib.harn"),
        "import \"shared\"\npub fn use_shared() { shared_helper() }\n",
    )
    .unwrap();
    let entry = write_file(root, "entry.harn", "import \"acme\"\nuse_shared()\n");

    let graph = build(std::slice::from_ref(&entry));
    let imported = graph
        .imported_names_for_file(&entry)
        .expect("nested package import should resolve");
    assert!(imported.contains("use_shared"));
    let acme_path = packages_root.join("acme/lib.harn");
    let acme_imports = graph
        .imported_names_for_file(&acme_path)
        .expect("package module imports should resolve");
    assert!(acme_imports.contains("shared_helper"));
}

#[test]
fn unknown_stdlib_import_is_unresolved() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let entry = write_file(root, "entry.harn", "import \"std/does_not_exist\"\n");

    let graph = build(std::slice::from_ref(&entry));
    assert!(
        graph.imported_names_for_file(&entry).is_none(),
        "unknown std module should fail resolution and disable strict check"
    );
}

/// Build one installed project root whose package set is `installed`.
///
/// Mirrors what an install actually produces: a path dependency is a SYMLINK
/// to its own source tree, so a module reached through one canonicalizes
/// outside the consumer's project root. A registry or git dependency is a copy
/// and canonicalizes inside it. That difference is the whole defect.
fn installed_project(
    parent: &Path,
    name: &str,
    manifest: &str,
    sources: &[(&str, &str)],
    installed: &[(&str, &Path)],
) -> PathBuf {
    let root = parent.join(name);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("harn.toml"), manifest).unwrap();
    for (relative, contents) in sources {
        fs::write(root.join(relative), contents).unwrap();
    }
    let packages_root = package_fixture(&root);
    for (alias, target) in installed {
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, packages_root.join(alias)).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(target, packages_root.join(alias)).unwrap();
    }
    root
}

/// A consumer's graph must resolve the imports its dependency makes of ITS own
/// dependency. Resolution used to consult only the snapshots acquired for the
/// files the caller asked about, and it keeps a snapshot only for a file
/// underneath that snapshot's project root. The middle module lives outside the
/// consumer's root, so every acquired snapshot was filtered away and its own
/// package import resolved to nothing — while the same file checked directly
/// resolved fine. A census that refuses an unresolved import then refused the
/// whole consumer.
#[test]
fn a_dependencys_own_package_import_resolves_through_the_consumers_graph() {
    let tmp = tempfile::tempdir().unwrap();
    let leaf = installed_project(
        tmp.path(),
        "leaf",
        "[package]\nname = \"leaf\"\n[exports]\ndefault = \"src/leaf.harn\"\n",
        &[("src/leaf.harn", "pub fn leaf_value() { 3 }\n")],
        &[],
    );
    let middle = installed_project(
        tmp.path(),
        "middle",
        "[package]\nname = \"middle\"\n[exports]\nmiddle = \"src/middle.harn\"\n",
        &[(
            "src/middle.harn",
            "import { leaf_value } from \"leaf/default\"\npub fn middle_value() { leaf_value() }\n",
        )],
        &[("leaf", leaf.as_path())],
    );
    let consumer = installed_project(
        tmp.path(),
        "consumer",
        "[package]\nname = \"consumer\"\n",
        &[(
            "src/lib.harn",
            "import { middle_value } from \"middle/middle\"\npub fn top() { middle_value() }\n",
        )],
        // An install flattens the transitive dependency into the consumer too.
        // Its presence is what makes the failure confusing rather than obvious:
        // the leaf IS installed here, and the middle module still could not see
        // it, because the middle module is not under this project root.
        &[("middle", middle.as_path()), ("leaf", leaf.as_path())],
    );

    let entry = consumer.join("src/lib.harn");
    let graph = build(std::slice::from_ref(&entry));

    let middle_module = middle.join("src/middle.harn");
    let leaf_import = graph
        .imports_for_module(&middle_module)
        .into_iter()
        .find(|import| import.raw_path == "leaf/default")
        .expect("the dependency's own package import must appear in the graph");
    assert!(
        leaf_import.resolved_path.is_some(),
        "a dependency reached through a path install must still resolve its own \
         package imports; unresolved here is what refused the census"
    );
    assert!(graph
        .imported_names_for_file(&middle_module)
        .expect("the middle module must type-check its own import")
        .contains("leaf_value"));

    // Control: the consumer's own import was never the broken one, so an
    // assertion that only read the entry file would pass on the defect.
    let middle_import = graph
        .imports_for_module(&entry)
        .into_iter()
        .find(|import| import.raw_path == "middle/middle")
        .expect("the consumer's direct import must appear");
    assert!(middle_import.resolved_path.is_some());
}

/// Control for the fallback's blast radius: it must not invent a resolution
/// for an import no project provides. Absence has to stay absence, or the
/// census would go from refusing a real module to accepting a missing one.
#[test]
fn a_dependencys_import_of_an_uninstalled_package_stays_unresolved() {
    let tmp = tempfile::tempdir().unwrap();
    let middle = installed_project(
        tmp.path(),
        "middle",
        "[package]\nname = \"middle\"\n[exports]\nmiddle = \"src/middle.harn\"\n",
        &[(
            "src/middle.harn",
            "import { absent } from \"never-installed/default\"\npub fn middle_value() { absent() }\n",
        )],
        &[],
    );
    let consumer = installed_project(
        tmp.path(),
        "consumer",
        "[package]\nname = \"consumer\"\n",
        &[(
            "src/lib.harn",
            "import { middle_value } from \"middle/middle\"\npub fn top() { middle_value() }\n",
        )],
        &[("middle", middle.as_path())],
    );

    let entry = consumer.join("src/lib.harn");
    let graph = build(std::slice::from_ref(&entry));
    let absent = graph
        .imports_for_module(&middle.join("src/middle.harn"))
        .into_iter()
        .find(|import| import.raw_path == "never-installed/default")
        .expect("the unresolvable import must still be recorded");
    assert_eq!(
        absent.resolved_path, None,
        "no project provides this package, so the fallback must report nothing"
    );
}
