//! Conservative package-scope collision checks for Go renames.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use harn_vm::process_sandbox::FsAccess;

use crate::ast::{api as ast_api, Language, SymbolKind};
use crate::host_call::{HostCallContext, Phase};

use super::refactor_core::{
    collect_identifier_spans, contained_path, read_source, IdentifierSpan, ShadowSite,
};

const MAX_ENTRIES: usize = 50_000;
const MAX_SOURCE_BYTES: usize = 64 * 1024 * 1024;

/// The exact package files read before planning. Compare again while holding
/// the host write lease so another builtin cannot insert a conflicting name
/// between this proof and the rename's writes.
pub(super) struct PackageProof {
    seed_path: String,
    sources: Vec<(String, String)>,
    members: BTreeSet<String>,
    constrained: BTreeSet<String>,
    workspace: Option<Vec<(String, String)>>,
}

impl PackageProof {
    pub(super) fn unchanged(&self, root: &Path, session_id: Option<&str>) -> bool {
        read_package(root, &self.seed_path, session_id, None)
            .is_ok_and(|current| current == self.sources)
            && self.workspace.as_ref().is_none_or(|expected| {
                read_workspace(root, session_id).is_ok_and(|current| &current == expected)
            })
    }

    pub(super) fn members(&self) -> impl Iterator<Item = &String> {
        self.members.iter()
    }

    pub(super) fn proves_binding(&self, path: &str) -> bool {
        self.members.contains(path)
            && !self.constrained.contains(&self.seed_path)
            && !self.constrained.contains(path)
    }

    /// The ordinary index intentionally omits some workspace files. Do not
    /// trust it as a completeness proof for a cross-file Go rename: an
    /// unindexed consumer could otherwise be left referring to the old name.
    pub(super) fn cover_workspace(
        &mut self,
        root: &Path,
        name: &str,
        session_id: Option<&str>,
        call: &HostCallContext,
    ) -> Result<(), String> {
        call.report(Phase::Resolving, "checking Go workspace sources");
        let sources =
            read_workspace_bounded(root, session_id, MAX_ENTRIES, MAX_SOURCE_BYTES, Some(call))?;
        for (path, source) in &sources {
            if call.interrupted().is_some() {
                return Err("Go workspace coverage was interrupted".into());
            }
            call.heartbeat(Phase::Resolving, "checking Go workspace bindings");
            if self.members.contains(path) || !source.contains(name) {
                continue;
            }
            let tree = ast_api::parse_tree(source, Language::Go).map_err(|err| err.to_string())?;
            if tree.root_node().has_error() {
                return Err(format!(
                    "Go source `{path}` cannot be parsed for binding proof"
                ));
            }
            let mut targets = Vec::new();
            let mut ignored = Vec::new();
            collect_identifier_spans(
                tree.root_node(),
                source.as_bytes(),
                name,
                "",
                &[
                    "identifier",
                    "type_identifier",
                    "field_identifier",
                    "package_identifier",
                ],
                path,
                &mut targets,
                &mut ignored,
            );
            if !targets.is_empty() {
                return Err(format!(
                    "Go source `{path}` outside the seed package may refer to `{name}`; use a binding-aware LSP rename"
                ));
            }
        }
        self.workspace = Some(sources);
        Ok(())
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
    call: Option<&HostCallContext>,
) -> Result<(Vec<ShadowSite>, PackageProof), String> {
    if let Some(call) = call {
        call.report(Phase::Resolving, "checking Go package sources");
    }
    let sources = read_package(root, seed_path, session_id, call)?;
    if call.is_some_and(|call| call.interrupted().is_some()) {
        return Err("Go package coverage was interrupted".into());
    }
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
    let mut members = BTreeSet::new();
    let mut constrained = BTreeSet::new();
    for (relative, source) in &sources {
        if let Some(call) = call {
            if call.interrupted().is_some() {
                return Err("Go package coverage was interrupted".into());
            }
            call.heartbeat(Phase::Resolving, "checking Go package bindings");
        }
        let (tree, symbols) =
            ast_api::parse_with_symbols(source, Language::Go).map_err(|err| err.to_string())?;
        if tree.root_node().has_error() {
            return Err(format!("Go package source `{relative}` does not parse"));
        }
        if package_name(&symbols)? != package {
            continue;
        }
        members.insert(relative.clone());
        let mut cursor = tree.root_node().walk();
        if tree.root_node().named_children(&mut cursor).any(|node| {
            node.kind() == "comment"
                && source.get(node.byte_range()).is_some_and(|text| {
                    text.starts_with("//go:build ") || text.starts_with("// +build ")
                })
        }) {
            constrained.insert(relative.clone());
        }
        if relative == seed_path {
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
            members,
            constrained,
            workspace: None,
        },
    ))
}

/// Bounded, fail-closed census independent of the index's pruning policy.
/// Files are read through the same workspace and staged-filesystem boundary as
/// rename itself. Keep the snapshot until commit, so a new unindexed consumer
/// cannot appear between planning and the host write lease.
fn read_workspace(root: &Path, session_id: Option<&str>) -> Result<Vec<(String, String)>, String> {
    read_workspace_bounded(root, session_id, MAX_ENTRIES, MAX_SOURCE_BYTES, None)
}

fn read_workspace_bounded(
    root: &Path,
    session_id: Option<&str>,
    max_entries: usize,
    max_source_bytes: usize,
    call: Option<&HostCallContext>,
) -> Result<Vec<(String, String)>, String> {
    let mut pending = vec![root.to_path_buf()];
    let mut paths = BTreeSet::<PathBuf>::new();
    let mut entries_seen = 0usize;
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).map_err(|err| err.to_string())? {
            if let Some(call) = call {
                if call.interrupted().is_some() {
                    return Err("Go workspace coverage was interrupted".into());
                }
                call.heartbeat(Phase::Resolving, "checking Go workspace sources");
            }
            entries_seen += 1;
            if entries_seen > max_entries {
                return Err("Go workspace census exceeded its entry limit".into());
            }
            let entry = entry.map_err(|err| err.to_string())?;
            let path = entry.path();
            let kind = entry.file_type().map_err(|err| err.to_string())?;
            if kind.is_dir() {
                if entry.file_name() != ".git" {
                    if super::walker::is_sensitive(&path) {
                        return Err(
                            "a sensitive workspace directory prevents Go coverage proof".into()
                        );
                    }
                    pending.push(path);
                }
            } else if kind.is_symlink() {
                // A non-sensitive link could hide a directory or a Go file.
                // Inspect only the target's type; never descend into a linked
                // directory or read a linked Go file to establish coverage.
                if path.extension().is_some_and(|extension| extension == "go") {
                    return Err("a workspace symbolic link prevents Go coverage proof".into());
                }
                if !super::walker::is_sensitive(&path)
                    && std::fs::metadata(&path)
                        .map_err(|err| err.to_string())?
                        .is_dir()
                {
                    return Err("a workspace symbolic link prevents Go coverage proof".into());
                }
            } else if kind.is_file() && path.extension().is_some_and(|extension| extension == "go")
            {
                if entry.metadata().map_err(|err| err.to_string())?.len()
                    > super::walker::MAX_FILE_BYTES
                {
                    return Err("a Go workspace source exceeds the inspection limit".into());
                }
                paths.insert(path);
            } else if path.extension().is_some_and(|extension| extension == "go") {
                return Err("a Go workspace source is not a regular file".into());
            }
        }
    }
    if let Some(session_id) = session_id {
        for path in crate::fs::staged_pending_paths(session_id).map_err(|err| err.to_string())? {
            entries_seen += 1;
            if entries_seen > max_entries {
                return Err("Go workspace census exceeded its entry limit".into());
            }
            if let Some(call) = call {
                if call.interrupted().is_some() {
                    return Err("Go workspace coverage was interrupted".into());
                }
                call.heartbeat(Phase::Resolving, "checking staged Go workspace sources");
            }
            if path.extension().is_some_and(|extension| extension == "go") {
                paths.insert(path);
            }
        }
    }
    let mut sources = Vec::new();
    let mut bytes = 0usize;
    for path in paths {
        if let Some(call) = call {
            if call.interrupted().is_some() {
                return Err("Go workspace coverage was interrupted".into());
            }
            call.heartbeat(Phase::Resolving, "reading Go workspace sources");
        }
        if super::walker::is_sensitive(&path) {
            return Err("a Go workspace source cannot be safely inspected".into());
        }
        let relative = path.strip_prefix(root).map_err(|err| err.to_string())?;
        let relative = relative
            .to_str()
            .ok_or("a Go workspace path is not UTF-8")?;
        let source = read_source(super::rename::BUILTIN, root, relative, session_id)
            .map_err(|err| err.to_string())?;
        if source.len() as u64 > super::walker::MAX_FILE_BYTES {
            return Err("a Go workspace source exceeds the inspection limit".into());
        }
        bytes = bytes.saturating_add(source.len());
        if bytes > max_source_bytes {
            return Err("Go workspace census exceeded its source-size limit".into());
        }
        sources.push((relative.to_string(), source));
    }
    Ok(sources)
}

fn read_package(
    root: &Path,
    seed_path: &str,
    session_id: Option<&str>,
    call: Option<&HostCallContext>,
) -> Result<Vec<(String, String)>, String> {
    read_package_bounded(
        root,
        seed_path,
        session_id,
        MAX_ENTRIES,
        MAX_SOURCE_BYTES,
        call,
    )
}

fn read_package_bounded(
    root: &Path,
    seed_path: &str,
    session_id: Option<&str>,
    max_entries: usize,
    max_source_bytes: usize,
    call: Option<&HostCallContext>,
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
    let mut entries_seen = 0usize;
    for entry in std::fs::read_dir(&directory).map_err(|err| err.to_string())? {
        if let Some(call) = call {
            if call.interrupted().is_some() {
                return Err("Go package coverage was interrupted".into());
            }
            call.heartbeat(Phase::Resolving, "checking Go package sources");
        }
        entries_seen += 1;
        if entries_seen > max_entries {
            return Err("Go package census exceeded its entry limit".into());
        }
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
            } else {
                return Err("a Go package source is not a regular file".into());
            }
        }
    }
    if let Some(session_id) = session_id {
        for path in crate::fs::staged_pending_paths(session_id).map_err(|err| err.to_string())? {
            if path.parent() == Some(directory.as_path())
                && path.extension().is_some_and(|extension| extension == "go")
            {
                if let Some(call) = call {
                    if call.interrupted().is_some() {
                        return Err("Go package coverage was interrupted".into());
                    }
                    call.heartbeat(Phase::Resolving, "checking staged Go package sources");
                }
                entries_seen += 1;
                if entries_seen > max_entries {
                    return Err("Go package census exceeded its entry limit".into());
                }
                paths.insert(path);
            }
        }
    }
    let mut sources = Vec::new();
    let mut bytes = 0usize;
    for path in paths {
        if let Some(call) = call {
            if call.interrupted().is_some() {
                return Err("Go package coverage was interrupted".into());
            }
            call.heartbeat(Phase::Resolving, "reading Go package sources");
        }
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
        bytes = bytes.saturating_add(source.len());
        if bytes > max_source_bytes {
            return Err("Go package census exceeded its source-size limit".into());
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
        let (shadows, proof) = inspect(root, "store.go", "DocumentLibrary", None, None).unwrap();
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

    #[test]
    fn workspace_proof_detects_new_consumer_before_commit() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir(root.join("lab")).unwrap();
        fs::write(
            root.join("lab/store.go"),
            "package lab\ntype DocumentStore struct{}\n",
        )
        .unwrap();
        let (_, mut proof) = inspect(root, "lab/store.go", "DocumentLibrary", None, None).unwrap();
        proof
            .cover_workspace(root, "DocumentStore", None, &HostCallContext::new(None))
            .unwrap();
        assert!(proof.unchanged(root, None));
        fs::create_dir(root.join("build")).unwrap();
        fs::write(
            root.join("build/client.go"),
            "package build\nvar value DocumentStore\n",
        )
        .unwrap();
        assert!(!proof.unchanged(root, None));
    }

    #[test]
    fn package_census_refuses_when_entry_or_byte_limits_are_exceeded() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("one.go"), "package lab\ntype One struct{}\n").unwrap();
        fs::write(root.join("two.go"), "package lab\ntype Two struct{}\n").unwrap();
        assert_eq!(
            read_package_bounded(root, "one.go", None, 2, 1000, None)
                .unwrap()
                .len(),
            2
        );
        assert!(read_package_bounded(root, "one.go", None, 1, 1000, None).is_err());
        assert!(read_package_bounded(root, "one.go", None, 2, 10, None).is_err());
        let expired = HostCallContext::new(None).with_budget(std::time::Duration::ZERO);
        assert!(read_package_bounded(root, "one.go", None, 2, 1000, Some(&expired)).is_err());
    }

    #[test]
    fn workspace_census_refuses_when_entry_or_byte_limits_are_exceeded() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("one.go"), "package lab\ntype One struct{}\n").unwrap();
        fs::write(root.join("two.go"), "package lab\ntype Two struct{}\n").unwrap();
        assert_eq!(
            read_workspace_bounded(root, None, 2, 1000, None)
                .unwrap()
                .len(),
            2
        );
        assert!(read_workspace_bounded(root, None, 1, 1000, None).is_err());
        assert!(read_workspace_bounded(root, None, 2, 10, None).is_err());
        let expired = HostCallContext::new(None).with_budget(std::time::Duration::ZERO);
        assert!(read_workspace_bounded(root, None, 2, 1000, Some(&expired)).is_err());
    }

    #[test]
    fn workspace_census_counts_staged_paths_toward_its_limit() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let session_id = format!(
            "go-coverage-{}",
            root.file_name().unwrap().to_string_lossy()
        );
        crate::fs::configure_session_root(&session_id, &root);
        crate::fs::set_mode(&session_id, crate::fs::FsMode::Staged, Some(&root)).unwrap();
        for name in ["one.go", "two.go"] {
            crate::fs::stage_write_or_none(
                super::super::rename::BUILTIN,
                &root.join(name),
                b"package lab\n",
                false,
                true,
                Some(&session_id),
            )
            .unwrap()
            .expect("source was staged");
        }
        // The staged-filesystem owner may create private directories. Measure
        // their disk contribution through the same census before testing the
        // additional staged entries.
        let disk_entries = (0..=100)
            .find(|limit| read_workspace_bounded(&root, None, *limit, 1000, None).is_ok())
            .expect("fixture fits the census");
        assert_eq!(
            read_workspace_bounded(&root, Some(&session_id), disk_entries + 2, 1000, None)
                .unwrap()
                .len(),
            2
        );
        assert!(
            read_workspace_bounded(&root, Some(&session_id), disk_entries + 1, 1000, None).is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn workspace_proof_rejects_linked_go_sources_but_allows_unrelated_file_links() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(
            root.join("store.go"),
            "package lab\ntype DocumentStore struct{}\n",
        )
        .unwrap();
        fs::write(root.join("guidance.md"), "guidance\n").unwrap();
        symlink(root.join("guidance.md"), root.join("CLAUDE.md")).unwrap();
        let (_, mut proof) = inspect(root, "store.go", "DocumentLibrary", None, None).unwrap();
        proof
            .cover_workspace(root, "DocumentStore", None, &HostCallContext::new(None))
            .unwrap();
        symlink(root.join("store.go"), root.join("linked.go")).unwrap();
        assert!(proof
            .cover_workspace(root, "DocumentStore", None, &HostCallContext::new(None))
            .is_err());
        assert!(!proof.unchanged(root, None));
        fs::remove_file(root.join("linked.go")).unwrap();
        symlink(root.join("store.go"), root.join("secret.go")).unwrap();
        assert!(proof
            .cover_workspace(root, "DocumentStore", None, &HostCallContext::new(None))
            .is_err());
    }

    #[cfg(unix)]
    #[test]
    fn workspace_census_refuses_a_nonregular_go_source_without_opening_it() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let dir = tempfile::tempdir().unwrap();
        let fifo = CString::new(dir.path().join("pending.go").as_os_str().as_bytes()).unwrap();
        // SAFETY: the CString is valid and points into this private test directory.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert!(read_workspace(dir.path(), None).is_err());
    }
}
