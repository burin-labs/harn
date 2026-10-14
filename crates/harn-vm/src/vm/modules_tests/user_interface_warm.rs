use std::path::{Path, PathBuf};

use super::*;

/// Parses and graph walks done on this thread while `work` runs. The walk is
/// where `harn_modules` builds its graph, and `parse_module_source` is every
/// module parse this crate does, so together they are all the parsing an
/// import can cause.
fn parse_work_during(work: impl FnOnce()) -> (u64, u64) {
    let walks = || crate::bytecode_cache::WALKS_PERFORMED.with(std::cell::Cell::get);
    let parses = || crate::module_artifact::MODULE_PARSES.with(std::cell::Cell::get);
    let (walks_before, parses_before) = (walks(), parses());
    work();
    (walks() - walks_before, parses() - parses_before)
}

/// A tree whose root's lowering depends on an enum two imports away, so the
/// interface carries names that can be wrong.
struct Tree {
    _dir: tempfile::TempDir,
    root: PathBuf,
    mid: PathBuf,
    leaf: PathBuf,
}

const MID_BODY: &str = "import \"./leaf\"\npub fn mid() -> int { return 1 }\n";

impl Tree {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("temp tree");
        let root = dir.path().join("root.harn");
        let mid = dir.path().join("mid.harn");
        let leaf = dir.path().join("leaf.harn");
        std::fs::write(
            &root,
            r#"import { Color } from "./leaf"
import { mid } from "./mid"
pub fn describe(value: any) -> string {
  match value {
    Color.Ready(message) -> { return message }
    _ -> { return "other" }
  }
}
"#,
        )
        .unwrap();
        std::fs::write(&mid, MID_BODY).unwrap();
        std::fs::write(&leaf, "pub enum Color { Ready(message: string) Empty }\n").unwrap();
        let tree = Self {
            _dir: dir,
            root,
            mid,
            leaf,
        };
        tree.settle();
        tree
    }

    /// Age every file out of the racy window, so a manifest decides by stats
    /// the way it does for a tree nobody is editing.
    fn settle(&self) {
        for path in [&self.root, &self.mid, &self.leaf] {
            age(path);
        }
    }
}

fn age(path: &Path) {
    let long_ago =
        std::fs::metadata(path).unwrap().modified().unwrap() - std::time::Duration::from_hours(1);
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(long_ago))
        .unwrap();
}

/// Import `root` into a fresh VM, as a new process would: a fresh VM has an
/// empty prepared-module cache and interface memo, so only the disk can
/// answer.
fn import_fresh(runtime: &tokio::runtime::Runtime, root: &Path) -> Vec<String> {
    runtime.block_on(async {
        let mut vm = Vm::new();
        let exports = vm.load_module_exports(root).await.expect("root imports");
        exports.into_keys().collect()
    })
}

/// The interface a fresh process derives for `path`.
fn derived_interface(path: &Path) -> crate::module_artifact::ModuleCompilationContext {
    let source = std::fs::read_to_string(path).unwrap();
    crate::bytecode_cache::module_compilation_context_with_manifest(path, &source)
        .expect("interface derives")
        .0
}

struct CacheDir {
    _dir: tempfile::TempDir,
    previous: Option<std::ffi::OsString>,
}

impl CacheDir {
    fn install() -> Self {
        let dir = tempfile::tempdir().expect("temp cache dir");
        let previous = std::env::var_os(crate::bytecode_cache::CACHE_DIR_ENV);
        std::env::set_var(crate::bytecode_cache::CACHE_DIR_ENV, dir.path());
        Self {
            _dir: dir,
            previous,
        }
    }
}

impl Drop for CacheDir {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => std::env::set_var(crate::bytecode_cache::CACHE_DIR_ENV, value),
            None => std::env::remove_var(crate::bytecode_cache::CACHE_DIR_ENV),
        }
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime builds")
}

#[test]
fn a_warm_import_of_an_unchanged_graph_parses_nothing() {
    // harn#9403: a warm process loaded every module's bytecode from disk, but
    // first re-walked and re-parsed the whole import graph to rebuild the
    // interfaces those artifacts are keyed by.
    let _guard = cache_test_guard();
    let _cache = CacheDir::install();
    let tree = Tree::new();
    let runtime = runtime();

    // Cold arm: the positive control. Without it a counter that never moves
    // would pass the warm assertion vacuously.
    let (cold_walks, cold_parses) = parse_work_during(|| {
        import_fresh(&runtime, &tree.root);
    });
    assert!(
        cold_walks > 0 && cold_parses > 0,
        "a cold import must walk and parse (walks {cold_walks}, parses {cold_parses})"
    );

    let (warm_walks, warm_parses) = parse_work_during(|| {
        assert!(import_fresh(&runtime, &tree.root).contains(&"describe".to_string()));
    });
    assert_eq!(
        (warm_walks, warm_parses),
        (0, 0),
        "a warm import of an unchanged graph must not walk or parse any module"
    );
}

#[test]
fn editing_a_leaf_re_derives_its_importers_interface() {
    let _guard = cache_test_guard();
    let _cache = CacheDir::install();
    let tree = Tree::new();
    let runtime = runtime();
    import_fresh(&runtime, &tree.root);
    assert!(derived_interface(&tree.root)
        .enum_candidates()
        .contains(&"Color".to_string()));

    std::fs::write(
        &tree.leaf,
        "pub enum Shade { Ready(message: string) Empty }\n",
    )
    .unwrap();
    age(&tree.leaf);

    let (walks, _) = parse_work_during(|| {
        let context = derived_interface(&tree.root);
        assert!(
            !context.enum_candidates().contains(&"Color".to_string()),
            "a stored interface must not outlive the leaf it described: {context:?}"
        );
    });
    assert!(
        walks > 0,
        "an edited leaf must send the importer back to the walk"
    );
}

#[test]
fn changing_an_import_list_re_derives_the_interface() {
    let _guard = cache_test_guard();
    let _cache = CacheDir::install();
    let tree = Tree::new();
    import_fresh(&runtime(), &tree.root);
    let before = derived_interface(&tree.mid);
    assert!(!before.enum_candidates().contains(&"Tone".to_string()));

    // `mid` gains an import whose enum its own body now matches on. Nothing
    // the old manifest recorded changed except mid's bytes, which key the
    // record; the new dependency did not exist when it was written.
    let tone = tree.root.with_file_name("tone.harn");
    std::fs::write(&tone, "pub enum Tone { Loud Quiet }\n").unwrap();
    std::fs::write(
        &tree.mid,
        r#"import "./leaf"
import { Tone } from "./tone"
pub fn mid(value: any) -> int {
  match value {
    Tone.Loud -> { return 1 }
    _ -> { return 0 }
  }
}
"#,
    )
    .unwrap();
    age(&tree.mid);
    age(&tone);

    let after = derived_interface(&tree.mid);
    assert!(
        after.enum_candidates().contains(&"Tone".to_string()),
        "a changed import list must re-derive the interface: {after:?}"
    );
}

#[test]
fn an_interface_from_another_compiler_build_is_not_reused() {
    let _guard = cache_test_guard();
    let _cache = CacheDir::install();
    let tree = Tree::new();
    let source = std::fs::read_to_string(&tree.root).unwrap();
    derived_interface(&tree.root);
    let slot =
        crate::bytecode_cache::InterfaceSlot::open(&tree.root, &source).expect("the cache is on");
    assert!(
        slot.recall().is_some(),
        "the walk must have stored a record"
    );

    slot.restamp_as_build("a-different-compiler-build");
    assert!(
        slot.recall().is_none(),
        "a record another build of this version wrote must not be served"
    );
    let (walks, _) = parse_work_during(|| {
        derived_interface(&tree.root);
    });
    assert!(walks > 0, "a rejected record must fall back to the walk");
}

/// Publish package generation `generation` under `root` whose `acme` package
/// exports `body`, and point the project at it. Earlier generations stay on
/// disk untouched, as they do while another process still leases them.
fn publish_generation(root: &Path, generation: &str, body: &str) {
    use harn_modules::package_snapshot::{
        generation_root, package_current_path, package_lock_digest, package_publication_lock_path,
        PackageGenerationManifest, PackageGenerationPointer, GENERATION_LEASE_FILE,
        GENERATION_LOCK_FILE, GENERATION_MANIFEST_FILE, GENERATION_PACKAGES_DIR,
    };
    let generation_root = generation_root(root, generation);
    let package = generation_root.join(GENERATION_PACKAGES_DIR).join("acme");
    std::fs::create_dir_all(package.join("runtime")).unwrap();
    let lock = "version = 4\n\n[[package]]\nname = \"acme\"\n";
    std::fs::write(generation_root.join(GENERATION_LOCK_FILE), lock).unwrap();
    std::fs::write(generation_root.join(GENERATION_LEASE_FILE), []).unwrap();
    let manifest =
        PackageGenerationManifest::new(generation, package_lock_digest(lock.as_bytes())).unwrap();
    std::fs::write(
        generation_root.join(GENERATION_MANIFEST_FILE),
        toml::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();
    std::fs::write(
        package.join("harn.toml"),
        "[exports]\nshapes = \"runtime/shapes.harn\"\n",
    )
    .unwrap();
    let module = package.join("runtime/shapes.harn");
    std::fs::write(&module, body).unwrap();
    age(&module);
    std::fs::write(
        package_current_path(root),
        toml::to_string_pretty(&PackageGenerationPointer::new(generation).unwrap()).unwrap(),
    )
    .unwrap();
    let publication_lock = package_publication_lock_path(root);
    if !publication_lock.exists() {
        std::fs::File::create(publication_lock).unwrap();
    }
}

#[test]
fn a_reinstalled_package_generation_re_derives_the_interface() {
    // A reinstall repoints the project at a new generation while the old one's
    // files stay on disk unchanged. Every file the record observed still
    // re-checks clean by stats, so only re-asking where the package import
    // resolves can notice that it now names different code.
    let _guard = cache_test_guard();
    let _cache = CacheDir::install();
    let project = tempfile::tempdir().unwrap();
    let root = project.path();
    std::fs::create_dir_all(root.join(".git")).unwrap();
    publish_generation(
        root,
        "generation-first",
        "pub enum Color { Ready(message: string) Empty }\n",
    );
    let entry = root.join("entry.harn");
    std::fs::write(
        &entry,
        r#"import "acme/shapes"
pub fn describe(value: any) -> string {
  match value {
    Color.Ready(message) -> { return message }
    _ -> { return "other" }
  }
}
"#,
    )
    .unwrap();
    age(&entry);

    let first = derived_interface(&entry);
    assert!(
        first.enum_candidates().contains(&"Color".to_string()),
        "the package's enum must reach the interface: {first:?}"
    );
    let (walks, _) = parse_work_during(|| {
        derived_interface(&entry);
    });
    assert_eq!(
        walks, 0,
        "an unchanged package graph must be served from its record"
    );

    publish_generation(
        root,
        "generation-second",
        "pub enum Shade { Ready(message: string) Empty }\n",
    );
    let second = derived_interface(&entry);
    assert!(
        second.enum_candidates().contains(&"Shade".to_string())
            && !second.enum_candidates().contains(&"Color".to_string()),
        "a repointed package generation must re-derive the interface: {second:?}"
    );
}
