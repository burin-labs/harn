//! Swift module resolution.
//!
//! Swift is the language whose imports name a build target rather than a
//! file, and whose files see each other inside a target with no import
//! statement at all. Both facts come from the same place, so both live
//! here: [`ModuleIndex`] answers "which files are in this module" and
//! "which module is this file in", and the resolver in
//! [`super::imports`] asks it rather than walking paths itself.

use std::collections::HashMap;

use super::file_table::FileId;

/// Module membership for languages where a file's module is implied by
/// where it sits rather than written at the top of the file.
///
/// Swift is the case this exists for. `import Foo` names a build target,
/// so it resolves to every file in that target, and the files of one
/// target already see each other with no import statement. Both are the
/// same underlying fact, and both are O(files) to store here rather than
/// O(files squared) if same-target visibility were written out as one
/// dependency edge per pair.
#[derive(Debug, Default, Clone)]
pub(crate) struct ModuleIndex {
    /// Module root (workspace-relative, e.g. `Sources/Core`) to its files.
    by_root: HashMap<String, Vec<FileId>>,
    /// Module name (`Core`) to the roots declaring it. Two roots can
    /// share a name, so this is a list and lookups union them.
    by_name: HashMap<String, Vec<String>>,
    /// File to the root of the module it belongs to.
    home: HashMap<FileId, String>,
}

impl ModuleIndex {
    /// Rebuild from the workspace path table. O(paths); the caller
    /// refreshes it whenever the path set changes, never per file.
    pub(crate) fn build(path_to_id: &HashMap<String, FileId>) -> Self {
        let mut index = ModuleIndex::default();
        for (path, id) in path_to_id {
            let Some(root) = swift_module_root(path) else {
                continue;
            };
            index.by_root.entry(root.clone()).or_default().push(*id);
            index.home.insert(*id, root);
        }
        for (root, files) in &mut index.by_root {
            files.sort_unstable();
            let name = root.rsplit('/').next().unwrap_or(root).to_string();
            index.by_name.entry(name).or_default().push(root.clone());
        }
        for roots in index.by_name.values_mut() {
            roots.sort();
        }
        index
    }

    /// The module root(s) named `name`. This, not the file list, is
    /// what the dep graph stores: a module of N files named by M
    /// importers costs M roots here and N times M only when expanded.
    pub(crate) fn roots_named(&self, name: &str) -> &[String] {
        self.by_name.get(name).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Expand module roots back into their files.
    pub(crate) fn files_in_roots(&self, roots: &[String]) -> Vec<FileId> {
        let mut out: Vec<FileId> = roots
            .iter()
            .filter_map(|root| self.by_root.get(root))
            .flatten()
            .copied()
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Every file in the module(s) named `name`.
    pub(crate) fn files_named(&self, name: &str) -> Vec<FileId> {
        self.files_in_roots(self.roots_named(name))
    }

    /// The root of the module `file` belongs to, if any.
    pub(crate) fn home_of(&self, file: FileId) -> Option<&str> {
        self.home.get(&file).map(String::as_str)
    }

    /// Every other file in `file`'s own module. Empty when the file
    /// belongs to no module the index understands.
    pub(crate) fn siblings_of(&self, file: FileId) -> Vec<FileId> {
        let Some(root) = self.home.get(&file) else {
            return Vec::new();
        };
        self.by_root
            .get(root)
            .map(|files| files.iter().copied().filter(|id| *id != file).collect())
            .unwrap_or_default()
    }
}

/// The target a Swift import statement names.
///
/// Swift spells this several ways and they all name the same target:
/// `import Core`, `@testable import Core`, the submodule form
/// `import Core.Net`, and the declaration form `import struct Core.Box`.
/// The target is the first dotted segment after the keywords.
pub(super) fn swift_module_name(raw: &str) -> Option<String> {
    let rest = raw.strip_prefix("@testable ").unwrap_or(raw).trim_start();
    let rest = rest.strip_prefix("import ")?.trim();
    // `import struct Core.Box` imports one declaration out of `Core`.
    const DECLARATION_KINDS: &[&str] = &[
        "struct",
        "class",
        "enum",
        "protocol",
        "typealias",
        "func",
        "let",
        "var",
        "actor",
    ];
    let mut token = rest.split_whitespace().next()?;
    if DECLARATION_KINDS.contains(&token) {
        token = rest.split_whitespace().nth(1)?;
    }
    let name = token.split('.').next()?.trim_end_matches(';');
    if name.is_empty() {
        return None;
    }
    Some(name.to_string())
}

/// The module root of a Swift file under the SwiftPM layout: the
/// directory immediately below the nearest `Sources` or `Tests`
/// ancestor. `Sources/Core/Net/Client.swift` belongs to `Sources/Core`.
///
/// Returns `None` for a Swift file that is not inside a target
/// directory, which is `Package.swift` itself and loose scripts. A file
/// sitting directly in `Sources/` also returns `None`: the layout does
/// not say which target owns it.
fn swift_module_root(path: &str) -> Option<String> {
    if !path.ends_with(".swift") {
        return None;
    }
    let segments: Vec<&str> = path.split('/').collect();
    if segments.len() < 3 {
        return None;
    }
    let last_container = segments.len() - 2;
    let anchor = segments[..=last_container]
        .iter()
        .rposition(|seg| *seg == "Sources" || *seg == "Tests")?;
    if anchor + 1 > last_container {
        return None;
    }
    Some(segments[..=anchor + 1].join("/"))
}

#[cfg(test)]
pub(in crate::code_index) mod tests {
    use super::*;

    pub(in crate::code_index) fn swift_workspace() -> HashMap<String, FileId> {
        let mut paths: HashMap<String, FileId> = HashMap::new();
        paths.insert("Sources/Core/Client.swift".to_string(), 1);
        paths.insert("Sources/Core/Net/Session.swift".to_string(), 2);
        paths.insert("Sources/App/Main.swift".to_string(), 3);
        paths.insert("Tests/CoreTests/ClientTests.swift".to_string(), 4);
        paths.insert("Package.swift".to_string(), 5);
        paths.insert("Sources/Loose.swift".to_string(), 6);
        paths
    }

    #[test]
    fn swift_target_membership_needs_no_import_statement() {
        let paths = swift_workspace();
        let modules = ModuleIndex::build(&paths);
        // `Client.swift` writes no import naming `Session.swift`, and
        // Swift does not require one: same target, mutually visible.
        assert_eq!(modules.siblings_of(1), vec![2]);
        assert_eq!(modules.siblings_of(2), vec![1]);
        // Membership excludes the file itself, so a lone file in a
        // target sees nobody.
        assert!(modules.siblings_of(3).is_empty());
    }

    #[test]
    fn swift_targets_do_not_leak_across_sources_and_tests() {
        let paths = swift_workspace();
        let modules = ModuleIndex::build(&paths);
        // A test target is its own module: importing it from a source
        // target must not pull in the source target's files.
        assert_eq!(modules.files_named("CoreTests"), vec![4]);
        assert!(!modules.siblings_of(4).contains(&1));
    }

    #[test]
    fn swift_files_outside_a_target_directory_join_no_module() {
        let paths = swift_workspace();
        let modules = ModuleIndex::build(&paths);
        // `Package.swift` is the manifest, not target source.
        assert!(modules.siblings_of(5).is_empty());
        // A file sitting directly in `Sources/` has no target directory
        // naming it, so guessing one would invent membership.
        assert!(modules.siblings_of(6).is_empty());
        assert!(modules.files_named("Sources").is_empty());
    }

    #[test]
    fn swift_module_root_reads_the_nearest_target_anchor() {
        assert_eq!(
            swift_module_root("Sources/Core/Net/Session.swift").as_deref(),
            Some("Sources/Core")
        );
        // A vendored package nests a second `Sources`; the nearest one
        // wins, so the inner package's targets stay separate.
        assert_eq!(
            swift_module_root("vendor/Dep/Sources/DepCore/A.swift").as_deref(),
            Some("vendor/Dep/Sources/DepCore")
        );
        assert_eq!(swift_module_root("Package.swift"), None);
        assert_eq!(swift_module_root("Sources/Loose.swift"), None);
        assert_eq!(swift_module_root("Sources/Core/Client.rs"), None);
    }

    #[test]
    fn swift_import_spellings_all_name_the_same_target() {
        assert_eq!(swift_module_name("import Core").as_deref(), Some("Core"));
        assert_eq!(
            swift_module_name("@testable import Core").as_deref(),
            Some("Core")
        );
        assert_eq!(
            swift_module_name("import Core.Net").as_deref(),
            Some("Core")
        );
        assert_eq!(
            swift_module_name("import struct Core.Box").as_deref(),
            Some("Core")
        );
        assert_eq!(swift_module_name("import"), None);
        assert_eq!(swift_module_name("class Foo {"), None);
    }
}
