//! Portable source names with an immutable, host-private file lookup.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use crate::{normalize_path, stdlib_module_name, ModuleGraph};

/// A host-private cached mapping, never part of the portable IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceDocumentRecord {
    pub document: String,
    pub path: PathBuf,
}

/// A source name is opaque. Only this graph-owned map can resolve it to a file.
#[derive(Clone, Debug)]
pub struct SourceDocuments {
    documents: BTreeMap<PathBuf, String>,
    physical: BTreeMap<String, PathBuf>,
}

impl SourceDocuments {
    /// Retain only real physical mappings in the compilation manifest.
    pub fn records(&self) -> Vec<SourceDocumentRecord> {
        self.physical
            .iter()
            .map(|(document, path)| SourceDocumentRecord {
                document: document.clone(),
                path: path.clone(),
            })
            .collect()
    }

    /// Restore a previously validated host manifest without resolving imports.
    /// The host must also bind this exact census to its validated source files.
    pub fn from_records(records: Vec<SourceDocumentRecord>) -> Result<Self, SourceDocumentError> {
        if records.is_empty() {
            return Err(SourceDocumentError::InvalidRecords);
        }
        let mut result = Self {
            documents: BTreeMap::new(),
            physical: BTreeMap::new(),
        };
        for record in records {
            if !record.path.is_absolute()
                || record
                    .path
                    .components()
                    .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
                || !valid_document(&record.document)
                || result.documents.contains_key(&record.path)
                || result.physical.contains_key(&record.document)
            {
                return Err(SourceDocumentError::InvalidRecords);
            }
            result.insert(record.path, record.document)?;
        }
        Ok(result)
    }

    /// Look up an exact module path from the graph; do not resolve new aliases.
    pub fn document_for(&self, path: &Path) -> Option<&str> {
        self.documents.get(path).map(String::as_str)
    }

    /// Resolve a known real source. Synthetic stdlib names grant no disk access.
    pub fn resolve(&self, document: &str) -> Option<&Path> {
        self.physical.get(document).map(PathBuf::as_path)
    }

    pub fn physical_paths(&self) -> impl Iterator<Item = &Path> {
        self.physical.values().map(PathBuf::as_path)
    }

    fn insert(&mut self, path: PathBuf, document: String) -> Result<(), SourceDocumentError> {
        if self
            .physical
            .get(&document)
            .is_some_and(|existing| existing != &path)
        {
            return Err(SourceDocumentError::DocumentCollision(document));
        }
        self.physical.insert(document.clone(), path.clone());
        self.documents.insert(path, document);
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceDocumentError {
    UnknownEntry,
    InvalidRecords,
    InvalidPath(PathBuf),
    UnavailablePackageRoot(String),
    UnmappedPackageScope(PathBuf),
    DocumentCollision(String),
}

impl std::fmt::Display for SourceDocumentError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownEntry => formatter.write_str("entry is not a real module in this graph"),
            Self::InvalidRecords => {
                formatter.write_str("source records are empty, malformed, or ambiguous")
            }
            Self::InvalidPath(path) => {
                write!(formatter, "source path cannot be named: {}", path.display())
            }
            Self::UnavailablePackageRoot(alias) => {
                write!(formatter, "package root is unavailable: {alias}")
            }
            Self::UnmappedPackageScope(path) => write!(
                formatter,
                "package source has no resolved import route: {}",
                path.display()
            ),
            Self::DocumentCollision(document) => write!(
                formatter,
                "multiple source files have document identity {document}"
            ),
        }
    }
}

impl std::error::Error for SourceDocumentError {}

impl ModuleGraph {
    /// Name the complete resolved graph without exposing machine-specific roots.
    ///
    /// Package aliases and roots come from the snapshots retained by the import
    /// resolver. Canonical path dependencies therefore keep their package name
    /// even when their actual directory lies outside the entry's project.
    pub fn source_documents(&self, entry: &Path) -> Result<SourceDocuments, SourceDocumentError> {
        let entry = normalize_path(entry);
        if !entry.is_absolute() || !self.modules.contains_key(&entry) {
            return Err(SourceDocumentError::UnknownEntry);
        }
        let entry_directory = entry.parent().ok_or(SourceDocumentError::UnknownEntry)?;
        // Several aliases can refer to the same canonical package. They share
        // one document namespace; the source graph already has one module for
        // that physical file and one defining scope for its imports.
        let mut packages = BTreeMap::<PathBuf, BTreeSet<String>>::new();
        for snapshot in &self._package_snapshots {
            for alias in snapshot.package_names() {
                let root = snapshot.packages_root().join(alias);
                let root = root
                    .canonicalize()
                    .map_err(|_| SourceDocumentError::UnavailablePackageRoot(alias.clone()))?;
                if !root.is_dir() {
                    return Err(SourceDocumentError::UnavailablePackageRoot(alias.clone()));
                }
                packages.entry(root).or_default().insert(alias.clone());
            }
        }
        // An entry executed from a package is still the local source root of
        // this compilation. Its own modules need no incoming package edge.
        packages.retain(|root, _| !entry.starts_with(root));

        let mut result = SourceDocuments {
            documents: BTreeMap::new(),
            physical: BTreeMap::new(),
        };
        let paths = self.module_paths();
        let mut pending = BTreeSet::new();
        for path in &paths {
            if let Some(module) = stdlib_module_name(path) {
                let document = format!("harn:synthetic/std/{}", encode_path(Path::new(module))?);
                result.documents.insert(path.clone(), document);
                continue;
            }
            if !path.is_absolute() {
                return Err(SourceDocumentError::InvalidPath(path.clone()));
            }
            if package_root(&packages, path).is_none() {
                let document = format!(
                    "harn:source/{}",
                    relative_document_path(entry_directory, path)?
                );
                result.insert(path.clone(), document.clone())?;
                pending.insert((document, path.clone()));
            }
        }

        // Use only edges already resolved by ModuleGraph. A package namespace
        // commits to its portable incoming scope, so the same alias in two
        // dependency scopes cannot collide. The ordered frontier picks one
        // deterministic route for aliases of the same canonical package and
        // visits cycles once. No cache directory or lockfile path enters it.
        let mut namespaces = BTreeMap::<PathBuf, String>::new();
        while let Some((importer_document, importer)) = pending.pop_first() {
            for import in self.imports_for_module(&importer) {
                let Some(path) = import.resolved_path else {
                    continue;
                };
                if result.documents.contains_key(&path) || !self.modules.contains_key(&path) {
                    continue;
                }
                let Some((root, aliases)) = package_root(&packages, &path) else {
                    continue;
                };
                let namespace = namespaces.entry(root.clone()).or_insert_with(|| {
                    let alias = aliases.first().expect("snapshot package alias");
                    let mut hash = Sha256::new();
                    hash.update(b"harn-source-package-route-v1\0");
                    hash.update(importer_document.as_bytes());
                    hash.update(b"\0");
                    hash.update(alias.as_bytes());
                    format!("harn:package/{alias}/{}/", hex::encode(hash.finalize()))
                });
                let relative = path.strip_prefix(root).expect("matched package root");
                let document = format!("{namespace}{}", encode_path(relative)?);
                result.insert(path.clone(), document.clone())?;
                pending.insert((document, path));
            }
        }
        if let Some(path) = paths
            .into_iter()
            .find(|path| !result.documents.contains_key(path))
        {
            return Err(SourceDocumentError::UnmappedPackageScope(path));
        }
        Ok(result)
    }
}

fn package_root<'a>(
    packages: &'a BTreeMap<PathBuf, BTreeSet<String>>,
    path: &Path,
) -> Option<(&'a PathBuf, &'a BTreeSet<String>)> {
    packages
        .iter()
        .filter(|(root, _)| path.starts_with(root))
        .max_by_key(|(root, _)| root.components().count())
}

fn valid_document(document: &str) -> bool {
    let path = if let Some(path) = document.strip_prefix("harn:source/") {
        path
    } else if let Some(package) = document.strip_prefix("harn:package/") {
        let mut parts = package.splitn(3, '/');
        let Some(alias) = parts.next() else {
            return false;
        };
        let Some(digest) = parts.next() else {
            return false;
        };
        let Some(path) = parts.next() else {
            return false;
        };
        if !crate::package_snapshot::is_valid_package_name(alias)
            || digest.len() != 64
            || !digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return false;
        }
        path
    } else {
        return false;
    };
    path.split('/').all(|segment| {
        if segment.is_empty() || matches!(segment, "." | "..") {
            return false;
        }
        let mut bytes = segment.bytes();
        while let Some(byte) = bytes.next() {
            if byte == b'%' {
                for _ in 0..2 {
                    if !bytes
                        .next()
                        .is_some_and(|hex| hex.is_ascii_digit() || (b'A'..=b'F').contains(&hex))
                    {
                        return false;
                    }
                }
            } else if !(byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~')) {
                return false;
            }
        }
        true
    })
}

fn relative_document_path(base: &Path, path: &Path) -> Result<String, SourceDocumentError> {
    let base_components = base.components().collect::<Vec<_>>();
    let path_components = path.components().collect::<Vec<_>>();
    // Different Windows volumes have no entry-relative spelling. Refuse instead
    // of embedding an absolute drive or assigning an unstable fallback name.
    if base_components.first() != path_components.first() {
        return Err(SourceDocumentError::InvalidPath(path.to_path_buf()));
    }
    let common = base_components
        .iter()
        .zip(&path_components)
        .take_while(|(a, b)| a == b)
        .count();
    let mut relative = PathBuf::new();
    for _ in common..base_components.len() {
        relative.push("..");
    }
    for component in &path_components[common..] {
        relative.push(component.as_os_str());
    }
    encode_path(&relative)
}

const SOURCE_SEGMENT: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');

fn encode_path(path: &Path) -> Result<String, SourceDocumentError> {
    let mut names = Vec::new();
    for component in path.components() {
        match component {
            // Keep parent components opaque so URI consumers cannot collapse
            // them into a different namespace. They never grant filesystem access.
            Component::ParentDir => names.push("%2E%2E".to_owned()),
            Component::Normal(name) => {
                let name = name
                    .to_str()
                    .ok_or_else(|| SourceDocumentError::InvalidPath(path.to_path_buf()))?;
                names.push(percent_encoding::utf8_percent_encode(name, SOURCE_SEGMENT).to_string());
            }
            Component::CurDir => {}
            Component::RootDir | Component::Prefix(_) => {
                return Err(SourceDocumentError::InvalidPath(path.to_path_buf()));
            }
        }
    }
    if names.is_empty() {
        return Err(SourceDocumentError::InvalidPath(path.to_path_buf()));
    }
    Ok(names.join("/"))
}

#[cfg(test)]
#[path = "source_documents_tests.rs"]
mod tests;
