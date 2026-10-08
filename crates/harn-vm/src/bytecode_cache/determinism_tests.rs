//! Relocatable keys depend only on output-determining inputs.
//!
//! One tree is built under two roots, reached under two path spellings, with two
//! package generation ids. Every relocatable entry key and every module key
//! must match byte for byte, and a dependency edit must still change them.

use std::path::{Path, PathBuf};

use super::package_key_tests::publish_acme_generation;
use crate::bytecode_cache::CacheKey;
use crate::module_artifact::{ModuleCompilationContext, ModuleProvenance};
use crate::module_source::ModuleSource;

const ENTRY: &str = "import \"./lib/helper\"\nimport \"acme/capabilities\"\n\
                     fn main() { return len([helper(), exported_capability()]) }\n";
// Calling a builtin makes the entry's module key cover its imported names.
// Both dependencies carry an import that resolves nowhere, so the unresolved
// sentinel is reached from a relative module and from a package module.
const HELPER: &str = "import \"./missing/thing\"\npub fn helper() { return 1 }\n";
const CAPABILITY: &str =
    "import \"../missing_helper\"\npub fn exported_capability() { return 42 }\n";

/// Write the tree under `root` with the package published as `generation`.
fn build_tree(root: &Path, generation: &str, helper: &str) {
    publish_acme_generation(root, generation, CAPABILITY);
    std::fs::create_dir_all(root.join("lib")).unwrap();
    std::fs::write(root.join("entry.harn"), ENTRY).unwrap();
    std::fs::write(root.join("lib/helper.harn"), helper).unwrap();
}

fn module_key(path: &Path) -> (CacheKey, ModuleCompilationContext) {
    let source = std::fs::read_to_string(path).unwrap();
    let graph = harn_modules::build_with_source(path, &source);
    let context = ModuleCompilationContext::for_source_in_graph(&graph, path, &source).unwrap();
    let key = CacheKey::from_module_source(
        &ModuleSource::from_text(source.as_str()),
        &context,
        ModuleProvenance::User,
    );
    (key, context)
}

struct Keys {
    entry: CacheKey,
    modules: Vec<(&'static str, CacheKey)>,
}

/// Derive every key the way a packaged tree is stamped, from `spelling`, the
/// path the caller hands in (possibly through a symlink, never canonicalized).
fn keys(spelling: &Path, generation: &str) -> Keys {
    let package_file = harn_modules::package_snapshot::generation_root(spelling, generation)
        .join(harn_modules::package_snapshot::GENERATION_PACKAGES_DIR)
        .join("acme/runtime/capabilities.harn");
    let entry = CacheKey::from_relocatable_source(&spelling.join("entry.harn"), ENTRY);
    let (entry_module, entry_context) = module_key(&spelling.join("entry.harn"));
    assert_ne!(
        entry_context,
        ModuleCompilationContext::default(),
        "the entry's module key must cover a real imported interface"
    );
    let modules = vec![
        ("entry.harn", entry_module),
        (
            "lib/helper.harn",
            module_key(&spelling.join("lib/helper.harn")).0,
        ),
        (
            "@packages/acme/runtime/capabilities.harn",
            module_key(&package_file).0,
        ),
    ];
    Keys { entry, modules }
}

/// A second spelling of `root`: through a symlink, so the caller's path is not
/// the canonical one. Tempdirs on macOS already are; Linux needs the alias.
fn aliased(root: &Path, holder: &Path) -> PathBuf {
    let alias = holder.join("alias");
    #[cfg(unix)]
    std::os::unix::fs::symlink(root, &alias).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(root, &alias).unwrap();
    alias
}

#[test]
fn relocatable_keys_ignore_root_spelling_and_install_generation() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let holder = tempfile::tempdir().unwrap();
    let edited = tempfile::tempdir().unwrap();
    build_tree(first.path(), "generation-0001", HELPER);
    build_tree(second.path(), "generation-0002", HELPER);
    build_tree(
        edited.path(),
        "generation-0003",
        "import \"./missing/thing\"\npub fn helper() { return 2 }\n",
    );

    let first_keys = keys(&aliased(first.path(), holder.path()), "generation-0001");
    let second_keys = keys(second.path(), "generation-0002");
    assert_eq!(
        first_keys.entry, second_keys.entry,
        "the relocatable entry key must not see the root, its spelling, or the generation"
    );
    for ((label, left), (_, right)) in first_keys.modules.iter().zip(&second_keys.modules) {
        assert_eq!(
            left, right,
            "module key for {label} must match across trees"
        );
    }

    let edited_keys = keys(edited.path(), "generation-0003");
    assert_ne!(
        first_keys.entry, edited_keys.entry,
        "a dependency edit must change the relocatable entry key"
    );
    let changed = first_keys
        .modules
        .iter()
        .zip(&edited_keys.modules)
        .filter(|((_, left), (_, right))| left != right)
        .map(|((label, _), _)| *label)
        .collect::<Vec<_>>();
    assert_eq!(
        changed,
        ["lib/helper.harn"],
        "only the edited module's key changes"
    );
}
