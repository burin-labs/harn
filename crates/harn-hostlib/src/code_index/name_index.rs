//! One owner for the graph's name lookup and its typed candidate projections.

use std::collections::HashSet;

use foldhash::HashMap as FastMap;

use super::file_table::FileId;
use super::symbol_graph::{Node, NodeId, NodeKind};

#[derive(Debug, Default, Clone)]
pub(super) struct NameIndex {
    buckets: FastMap<String, NameBucket>,
}

#[derive(Debug, Default, Clone)]
struct NameBucket {
    all: Vec<NodeId>,
    references: Vec<(NodeId, FileId)>,
    calls: Vec<NodeId>,
    functions: FastMap<FileId, Vec<NodeId>>,
    function_count: usize,
}

impl NameIndex {
    pub fn insert(&mut self, node: &Node) {
        // Most names repeat (every call of `len`), so look up before paying
        // for the owned key a new bucket needs.
        if !self.buckets.contains_key(node.name.as_str()) {
            self.buckets
                .insert(node.name.clone(), NameBucket::default());
        }
        let bucket = self
            .buckets
            .get_mut(node.name.as_str())
            .expect("bucket exists");
        bucket.all.push(node.id);
        if node.kind.is_name_addressable() {
            bucket.references.push((node.id, node.file_id));
        }
        match node.kind {
            NodeKind::Function => {
                bucket
                    .functions
                    .entry(node.file_id)
                    .or_default()
                    .push(node.id);
                bucket.function_count += 1;
            }
            NodeKind::CallSite => bucket.calls.push(node.id),
            _ => {}
        }
    }

    /// Remove a file in one pass per affected name, rather than rescanning a
    /// common call name's bucket for every call expression in that file.
    pub fn remove_file<'a>(&mut self, file: FileId, nodes: impl Iterator<Item = &'a Node>) {
        let mut removed = HashSet::new();
        let mut names = HashSet::new();
        for node in nodes {
            removed.insert(node.id);
            names.insert(node.name.as_str());
        }
        for name in names {
            let Some(bucket) = self.buckets.get_mut(name) else {
                continue;
            };
            bucket.all.retain(|id| !removed.contains(id));
            bucket.references.retain(|(_, owner)| *owner != file);
            bucket.calls.retain(|id| !removed.contains(id));
            if let Some(functions) = bucket.functions.remove(&file) {
                bucket.function_count -= functions.len();
            }
            if bucket.all.is_empty() {
                self.buckets.remove(name);
            }
        }
    }

    pub fn named(&self, name: &str) -> &[NodeId] {
        self.buckets.get(name).map_or(&[], |bucket| &bucket.all)
    }

    pub fn references(&self, name: &str) -> &[(NodeId, FileId)] {
        self.buckets
            .get(name)
            .map_or(&[], |bucket| &bucket.references)
    }

    pub fn call_sites(&self, name: &str) -> &[NodeId] {
        self.buckets.get(name).map_or(&[], |bucket| &bucket.calls)
    }

    /// Resolve declarations in order of confidence: same file, imported files,
    /// then exactly one workspace-wide declaration. Never guess between names
    /// that are ambiguous without import visibility.
    pub fn call_targets(&self, name: &str, file: FileId, visible: &HashSet<FileId>) -> Vec<NodeId> {
        let Some(bucket) = self.buckets.get(name) else {
            return Vec::new();
        };
        if let Some(local) = bucket.functions.get(&file) {
            return local.clone();
        }
        let mut imported = Vec::new();
        if visible.len() < bucket.functions.len() {
            for file in visible {
                if let Some(functions) = bucket.functions.get(file) {
                    imported.extend_from_slice(functions);
                }
            }
        } else {
            for (file, functions) in &bucket.functions {
                if visible.contains(file) {
                    imported.extend_from_slice(functions);
                }
            }
        }
        if !imported.is_empty() {
            imported.sort_unstable();
            return imported;
        }
        if bucket.function_count == 1 {
            return bucket.functions.values().next().unwrap().clone();
        }
        Vec::new()
    }
}
