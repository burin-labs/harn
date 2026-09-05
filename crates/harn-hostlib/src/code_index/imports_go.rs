//! Go package resolution.
//!
//! A Go import names a package, and a package is a directory: every
//! `.go` file in it is part of the same package and sees the others with
//! no import statement. Mapping an import path onto a directory needs
//! the module path each `go.mod` declares, which is the one piece of
//! this that has to be read from a file rather than derived from the
//! workspace layout.

/// The `module` lines of every `go.mod` in the workspace, longest module
/// path first so a nested module wins over the one containing it.
#[derive(Debug, Default, Clone)]
pub(crate) struct GoModules {
    /// `(declared module path, workspace-relative directory of its go.mod)`.
    modules: Vec<(String, String)>,
}

impl GoModules {
    /// Build from `(workspace-relative go.mod path, its contents)` pairs.
    pub(crate) fn from_manifests<'a, I>(manifests: I) -> Self
    where
        I: IntoIterator<Item = (&'a str, &'a str)>,
    {
        let mut modules: Vec<(String, String)> = Vec::new();
        for (path, contents) in manifests {
            let Some(module_path) = declared_module_path(contents) else {
                continue;
            };
            let dir = path
                .rsplit_once('/')
                .map(|(head, _)| head.to_string())
                .unwrap_or_default();
            modules.push((module_path, dir));
        }
        // A nested module declares a longer path than its parent, and an
        // import matching both belongs to the nested one.
        modules.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));
        GoModules { modules }
    }

    /// The workspace-relative directory an import path names, or `None`
    /// when the import belongs to no module declared in this workspace
    /// (the standard library and external dependencies).
    pub(crate) fn directory_for(&self, import_path: &str) -> Option<String> {
        for (module_path, dir) in &self.modules {
            if import_path == module_path {
                return Some(dir.clone());
            }
            if let Some(rest) = import_path.strip_prefix(module_path) {
                if let Some(rest) = rest.strip_prefix('/') {
                    return Some(if dir.is_empty() {
                        rest.to_string()
                    } else {
                        format!("{dir}/{rest}")
                    });
                }
            }
        }
        None
    }

    /// Whether any module was declared. An empty index means every Go
    /// import resolves to nothing for want of a manifest, which is a
    /// different answer from an import that names an external package.
    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.modules.is_empty()
    }
}

/// The path on `go.mod`'s `module` line.
fn declared_module_path(contents: &str) -> Option<String> {
    for line in contents.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("module ") {
            let path = rest.trim().trim_matches('"');
            if !path.is_empty() {
                return Some(path.to_string());
            }
        }
    }
    None
}

/// Extract the import paths from Go source.
///
/// Go writes almost every import inside a parenthesised block, so the
/// line-oriented keyword matcher used for other languages sees only the
/// `import (` opener and never a single path. Both the block form and
/// the single-line form are handled here, along with the alias, blank
/// and dot spellings (`m "x"`, `_ "x"`, `. "x"`).
pub(crate) fn extract_go_imports(source: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut in_block = false;
    for line in source.lines() {
        let trimmed = line.trim();
        if in_block {
            if trimmed.starts_with(')') {
                in_block = false;
                continue;
            }
            if let Some(path) = quoted_path(trimmed) {
                out.push(path);
            }
            continue;
        }
        if trimmed == "import (" || trimmed.starts_with("import (") {
            in_block = true;
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("import ") {
            if let Some(path) = quoted_path(rest) {
                out.push(path);
            }
        }
    }
    out
}

/// The contents of the first double-quoted run in `text`, ignoring any
/// alias token before it and any comment after it.
fn quoted_path(text: &str) -> Option<String> {
    let (_, rest) = text.split_once('"')?;
    let (path, _) = rest.split_once('"')?;
    if path.is_empty() {
        return None;
    }
    Some(path.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = "module github.com/acme/tool\n\ngo 1.26.1\n";

    #[test]
    fn block_imports_are_extracted_one_path_per_entry() {
        let source = "package runtime\n\nimport (\n\t\"fmt\"\n\n\t\"github.com/acme/tool/internal/store\"\n)\n\nfunc F() {}\n";
        assert_eq!(
            extract_go_imports(source),
            vec![
                "fmt".to_string(),
                "github.com/acme/tool/internal/store".to_string()
            ]
        );
    }

    #[test]
    fn alias_blank_and_dot_imports_all_yield_their_path() {
        let source = "import (\n\tm \"github.com/acme/tool/a\"\n\t_ \"github.com/acme/tool/b\"\n\t. \"github.com/acme/tool/c\"\n)\n";
        assert_eq!(
            extract_go_imports(source),
            vec![
                "github.com/acme/tool/a".to_string(),
                "github.com/acme/tool/b".to_string(),
                "github.com/acme/tool/c".to_string()
            ]
        );
    }

    #[test]
    fn single_line_import_is_extracted_too() {
        assert_eq!(extract_go_imports("import \"fmt\"\n"), vec!["fmt"]);
    }

    #[test]
    fn code_after_the_block_is_not_mistaken_for_an_import() {
        let source = "import (\n\t\"fmt\"\n)\n\nvar s = \"github.com/acme/tool/nope\"\n";
        assert_eq!(extract_go_imports(source), vec!["fmt"]);
    }

    #[test]
    fn import_path_maps_onto_the_declaring_modules_directory() {
        let mods = GoModules::from_manifests([("go.mod", MANIFEST)]);
        assert_eq!(
            mods.directory_for("github.com/acme/tool/internal/store")
                .as_deref(),
            Some("internal/store")
        );
        assert_eq!(
            mods.directory_for("github.com/acme/tool").as_deref(),
            Some("")
        );
    }

    #[test]
    fn a_module_in_a_subdirectory_anchors_on_that_subdirectory() {
        let mods = GoModules::from_manifests([("sdks/go/go.mod", MANIFEST)]);
        assert_eq!(
            mods.directory_for("github.com/acme/tool/client").as_deref(),
            Some("sdks/go/client")
        );
    }

    #[test]
    fn a_nested_module_wins_over_the_one_containing_it() {
        let mods = GoModules::from_manifests([
            ("go.mod", "module github.com/acme/tool\n"),
            ("plugin/go.mod", "module github.com/acme/tool/plugin\n"),
        ]);
        // Both prefixes match; the longer declaration owns the path.
        assert_eq!(
            mods.directory_for("github.com/acme/tool/plugin/x")
                .as_deref(),
            Some("plugin/x")
        );
        assert_eq!(
            mods.directory_for("github.com/acme/tool/internal")
                .as_deref(),
            Some("internal")
        );
    }

    #[test]
    fn an_external_or_stdlib_import_names_no_workspace_directory() {
        let mods = GoModules::from_manifests([("go.mod", MANIFEST)]);
        assert_eq!(mods.directory_for("fmt"), None);
        assert_eq!(mods.directory_for("github.com/other/dep"), None);
        // A prefix that is not a path boundary must not match.
        assert_eq!(mods.directory_for("github.com/acme/toolkit/x"), None);
    }

    #[test]
    fn a_workspace_with_no_manifest_declares_no_modules() {
        let mods = GoModules::from_manifests([("go.mod", "go 1.26.1\n")]);
        assert!(mods.is_empty());
        assert_eq!(mods.directory_for("github.com/acme/tool/x"), None);
    }
}
