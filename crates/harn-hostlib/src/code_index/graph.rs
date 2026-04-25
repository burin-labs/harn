//! Forward + reverse dependency graph over project files.
//!
//! Edges are "file A imports file B", built from the import strings the
//! `imports` module surfaces. Forward answers "what does A depend on?";
//! reverse answers "who depends on A?". Mirrors the Swift `DepGraph`,
//! including the `unresolved_imports` side-table for raw strings the
//! resolver could not map back to a known file.

use std::collections::{HashMap, HashSet};

use super::file_table::FileId;

/// Forward + reverse import graph plus the side-table of unresolved
/// import strings (raw text we couldn't map back to a known file).
#[derive(Debug, Default, Clone)]
pub struct DepGraph {
    forward: HashMap<FileId, HashSet<FileId>>,
    reverse: HashMap<FileId, HashSet<FileId>>,
    unresolved: HashMap<FileId, Vec<String>>,
}

impl DepGraph {
    /// Construct an empty graph.
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the outgoing edges for `file` with the resolved set.
    /// Re-keys reverse edges atomically so `imports`/`importers` stay in
    /// sync after every call.
    pub fn set_edges(&mut self, file: FileId, resolved: HashSet<FileId>, unresolved: Vec<String>) {
        if let Some(old) = self.forward.remove(&file) {
            for target in &old {
                if *target == file {
                    continue;
                }
                if let Some(set) = self.reverse.get_mut(target) {
                    set.remove(&file);
                    if set.is_empty() {
                        self.reverse.remove(target);
                    }
                }
            }
        }
        for target in &resolved {
            if *target == file {
                continue;
            }
            self.reverse.entry(*target).or_default().insert(file);
        }
        self.forward.insert(file, resolved);
        if unresolved.is_empty() {
            self.unresolved.remove(&file);
        } else {
            self.unresolved.insert(file, unresolved);
        }
    }

    /// Drop every edge touching `file` from both directions. Importers
    /// of `file` keep their forward entry but lose the dangling target.
    pub fn remove_file(&mut self, file: FileId) {
        if let Some(old) = self.forward.remove(&file) {
            for target in old {
                if target == file {
                    continue;
                }
                if let Some(set) = self.reverse.get_mut(&target) {
                    set.remove(&file);
                    if set.is_empty() {
                        self.reverse.remove(&target);
                    }
                }
            }
        }
        self.unresolved.remove(&file);
        // Anyone who imported us keeps their forward edge but it now
        // dangles. They'll re-resolve on their next reindex.
        if let Some(rev) = self.reverse.remove(&file) {
            for importer in rev {
                if let Some(forward) = self.forward.get_mut(&importer) {
                    forward.remove(&file);
                }
            }
        }
    }

    /// Forward edges: every file `file` imports.
    pub fn imports_of(&self, file: FileId) -> Vec<FileId> {
        self.forward
            .get(&file)
            .map(|set| set.iter().copied().collect())
            .unwrap_or_default()
    }

    /// Reverse edges: every file that imports `file`.
    pub fn importers_of(&self, file: FileId) -> Vec<FileId> {
        self.reverse
            .get(&file)
            .map(|set| set.iter().copied().collect())
            .unwrap_or_default()
    }

    /// Raw import strings the resolver couldn't map back to a known file.
    pub fn unresolved_imports(&self, file: FileId) -> &[String] {
        self.unresolved.get(&file).map(Vec::as_slice).unwrap_or(&[])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set_of(ids: &[FileId]) -> HashSet<FileId> {
        ids.iter().copied().collect()
    }

    #[test]
    fn set_edges_populates_both_sides() {
        let mut g = DepGraph::new();
        g.set_edges(1, set_of(&[2, 3]), vec![]);
        let mut imports = g.imports_of(1);
        imports.sort();
        assert_eq!(imports, vec![2, 3]);
        assert_eq!(g.importers_of(2), vec![1]);
        assert_eq!(g.importers_of(3), vec![1]);
    }

    #[test]
    fn re_setting_edges_drops_stale_reverse_edges() {
        let mut g = DepGraph::new();
        g.set_edges(1, set_of(&[2]), vec![]);
        g.set_edges(1, set_of(&[3]), vec![]);
        assert!(g.importers_of(2).is_empty());
        assert_eq!(g.importers_of(3), vec![1]);
    }

    #[test]
    fn remove_file_cleans_up_both_directions() {
        let mut g = DepGraph::new();
        g.set_edges(1, set_of(&[2]), vec!["unmatched".into()]);
        g.set_edges(3, set_of(&[1]), vec![]);
        g.remove_file(1);
        assert!(g.imports_of(1).is_empty());
        assert!(g.importers_of(2).is_empty());
        // 3 still imports... nothing now (its forward edge dangling-cleared).
        assert!(g.imports_of(3).is_empty());
    }

    #[test]
    fn unresolved_imports_round_trip() {
        let mut g = DepGraph::new();
        g.set_edges(1, HashSet::new(), vec!["weird::path".into()]);
        assert_eq!(g.unresolved_imports(1), &["weird::path".to_string()]);
        g.set_edges(1, HashSet::new(), vec![]);
        assert!(g.unresolved_imports(1).is_empty());
    }
}
