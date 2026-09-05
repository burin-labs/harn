//! What a Swift import statement names.
//!
//! A Swift import names a build target, and a target is the directory
//! immediately below the nearest `Sources` or `Tests` ancestor.
//! [`super::module_index`] owns the membership table; this file owns the
//! two rules that feed it.

/// The target a Swift import statement names.
///
/// Swift spells this several ways and they all name the same target:
/// `import Core`, `@testable import Core`, the submodule form
/// `import Core.Net`, and the declaration form `import struct Core.Box`.
/// The target is the first dotted segment after the keywords.
pub(crate) fn swift_module_name(raw: &str) -> Option<String> {
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
pub(super) fn swift_module_root(path: &str) -> Option<String> {
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
    use std::collections::HashMap;

    use super::super::file_table::FileId;
    use super::super::module_index::ModuleIndex;
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
