//! Conservative package-scope collision checks for Go renames.

use std::collections::BTreeSet;
use std::path::Path;

use harn_vm::process_sandbox::FsAccess;

use crate::ast::{api as ast_api, Language, SymbolKind};

use super::refactor_core::{
    collect_identifier_spans, contained_path, read_source, IdentifierSpan, ShadowSite,
};

/// The exact package files read before planning. Compare again while holding
/// the host write lease so another builtin cannot insert a conflicting name
/// between this proof and the rename's writes.
pub(super) struct PackageProof {
    seed_path: String,
    sources: Vec<(String, String)>,
}

impl PackageProof {
    pub(super) fn unchanged(&self, root: &Path, session_id: Option<&str>) -> bool {
        read_package(root, &self.seed_path, session_id).is_ok_and(|current| current == self.sources)
    }
}

/// Check every sibling Go source, including staged additions and files that do
/// not mention the old name. A same-package identifier may become shadowed by
/// the new package declaration, so the fallback fails closed on any such site.
pub(super) fn inspect(
    root: &Path,
    seed_path: &str,
    new_name: &str,
    session_id: Option<&str>,
) -> Result<(Vec<ShadowSite>, PackageProof), String> {
    let sources = read_package(root, seed_path, session_id)?;
    let seed_source = sources
        .iter()
        .find(|(path, _)| path == seed_path)
        .map(|(_, source)| source)
        .ok_or("the Go seed is absent from its current package")?;
    let (seed_tree, seed_symbols) =
        ast_api::parse_with_symbols(seed_source, Language::Go).map_err(|err| err.to_string())?;
    if seed_tree.root_node().has_error() {
        return Err("the current Go seed does not parse".into());
    }
    let package = package_name(&seed_symbols)?;
    // Field and package identifiers have their own namespaces and cannot be
    // shadowed by a package-level declaration.
    let kinds = &["identifier", "type_identifier"];
    let mut shadows = Vec::new();
    for (relative, source) in &sources {
        if relative == seed_path {
            continue;
        }
        let (tree, symbols) =
            ast_api::parse_with_symbols(source, Language::Go).map_err(|err| err.to_string())?;
        if tree.root_node().has_error() {
            return Err(format!("Go package source `{relative}` does not parse"));
        }
        if package_name(&symbols)? != package {
            continue;
        }
        let mut ignored: Vec<IdentifierSpan> = Vec::new();
        collect_identifier_spans(
            tree.root_node(),
            source.as_bytes(),
            "",
            new_name,
            kinds,
            relative,
            &mut ignored,
            &mut shadows,
        );
    }
    Ok((
        shadows,
        PackageProof {
            seed_path: seed_path.to_string(),
            sources,
        },
    ))
}

fn read_package(
    root: &Path,
    seed_path: &str,
    session_id: Option<&str>,
) -> Result<Vec<(String, String)>, String> {
    let builtin = super::rename::BUILTIN;
    let seed = Path::new(seed_path);
    let relative_dir = seed.parent().ok_or("the Go seed has no parent directory")?;
    let relative_dir_text = relative_dir
        .to_str()
        .ok_or("the Go package path is not UTF-8")?;
    let directory = contained_path(builtin, root, relative_dir_text, FsAccess::Read)
        .map_err(|err| err.to_string())?;
    let mut paths = BTreeSet::new();
    for entry in std::fs::read_dir(&directory).map_err(|err| err.to_string())? {
        let entry = entry.map_err(|err| err.to_string())?;
        let path = entry.path();
        if path.extension().is_some_and(|extension| extension == "go") {
            let file_type = entry.file_type().map_err(|err| err.to_string())?;
            if file_type.is_symlink() {
                return Err("a Go package source is a symbolic link".into());
            }
            if file_type.is_file() {
                if super::walker::is_sensitive(&path)
                    || entry.metadata().map_err(|err| err.to_string())?.len()
                        > super::walker::MAX_FILE_BYTES
                {
                    return Err("a Go package source cannot be safely inspected".into());
                }
                paths.insert(path);
            }
        }
    }
    if let Some(session_id) = session_id {
        for path in crate::fs::staged_pending_paths(session_id).map_err(|err| err.to_string())? {
            if path.parent() == Some(directory.as_path())
                && path.extension().is_some_and(|extension| extension == "go")
            {
                paths.insert(path);
            }
        }
    }
    let mut sources = Vec::new();
    for path in paths {
        if super::walker::is_sensitive(&path) {
            return Err("a Go package source cannot be safely inspected".into());
        }
        let relative = path.strip_prefix(root).map_err(|err| err.to_string())?;
        let relative = relative
            .to_str()
            .ok_or("a Go package source path is not UTF-8")?;
        let source =
            read_source(builtin, root, relative, session_id).map_err(|err| err.to_string())?;
        if source.len() as u64 > super::walker::MAX_FILE_BYTES {
            return Err("a Go package source cannot be safely inspected".into());
        }
        sources.push((relative.to_string(), source));
    }
    Ok(sources)
}

fn package_name(symbols: &[crate::ast::Symbol]) -> Result<&str, String> {
    let mut packages = symbols
        .iter()
        .filter(|symbol| symbol.kind == SymbolKind::Module);
    let package = packages
        .next()
        .ok_or("the Go package declaration is missing")?;
    if packages.next().is_some() {
        return Err("the Go package declaration is ambiguous".into());
    }
    Ok(&package.name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn package_proof_detects_changed_added_and_removed_siblings() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(
            root.join("store.go"),
            "package lab\ntype DocumentStore struct{}\n",
        )
        .unwrap();
        let sibling_path = root.join("other.go");
        let original = "package lab\ntype Other struct{}\n";
        fs::write(&sibling_path, original).unwrap();
        let (shadows, proof) = inspect(root, "store.go", "DocumentLibrary", None).unwrap();
        assert!(shadows.is_empty());
        assert!(proof.unchanged(root, None));
        fs::write(
            &sibling_path,
            "package lab\ntype DocumentLibrary struct{}\n",
        )
        .unwrap();
        assert!(!proof.unchanged(root, None));
        fs::write(&sibling_path, original).unwrap();
        assert!(proof.unchanged(root, None));
        let added = root.join("new.go");
        fs::write(&added, "package lab\ntype DocumentLibrary struct{}\n").unwrap();
        assert!(!proof.unchanged(root, None));
        fs::remove_file(&added).unwrap();
        assert!(proof.unchanged(root, None));
        fs::remove_file(&sibling_path).unwrap();
        assert!(!proof.unchanged(root, None));
    }
}
