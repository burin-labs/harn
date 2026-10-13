//! One owner for the graph's name lookup and its typed candidate projections.
//!
//! Like the graph store, the index is a shared base built once per graph
//! generation plus the buckets changed since, so a clone copies only what
//! edits touched and the first edit after a restore copies one bucket.

use std::collections::HashSet;
use std::sync::Arc;

use foldhash::HashMap as FastMap;

use super::file_table::FileId;
use super::symbol_graph::{Node, NodeId, NodeKind};

#[derive(Debug, Default, Clone)]
pub(super) struct NameIndex {
    base: Arc<FastMap<String, NameBucket>>,
    /// Buckets copied out of `base` on first write. An empty bucket hides a
    /// base bucket whose nodes are all gone.
    changed: FastMap<String, NameBucket>,
}

#[derive(Debug, Default, Clone)]
struct NameBucket {
    all: Vec<NodeId>,
    references: Vec<(NodeId, FileId)>,
    calls: Vec<NodeId>,
    functions: FastMap<FileId, Vec<NodeId>>,
    function_count: usize,
}

impl NameBucket {
    fn add(&mut self, node: &Node) {
        self.all.push(node.id);
        if node.kind.is_name_addressable() {
            self.references.push((node.id, node.file_id));
        }
        match node.kind {
            NodeKind::Function => {
                self.functions
                    .entry(node.file_id)
                    .or_default()
                    .push(node.id);
                self.function_count += 1;
            }
            NodeKind::CallSite => self.calls.push(node.id),
            _ => {}
        }
    }
}

impl NameIndex {
    /// Index one graph generation's nodes as the shared base.
    pub fn from_nodes(nodes: &[Node]) -> Self {
        let mut buckets: FastMap<String, NameBucket> = FastMap::default();
        for node in nodes {
            // Most names repeat (every call of `len`), so look up before
            // paying for the owned key a new bucket needs.
            match buckets.get_mut(node.name.as_str()) {
                Some(bucket) => bucket.add(node),
                None => {
                    let mut bucket = NameBucket::default();
                    bucket.add(node);
                    buckets.insert(node.name.clone(), bucket);
                }
            }
        }
        Self {
            base: Arc::new(buckets),
            changed: FastMap::default(),
        }
    }

    fn bucket(&self, name: &str) -> Option<&NameBucket> {
        self.changed.get(name).or_else(|| self.base.get(name))
    }

    fn bucket_mut(&mut self, name: &str) -> &mut NameBucket {
        if !self.changed.contains_key(name) {
            let copied = self.base.get(name).cloned().unwrap_or_default();
            self.changed.insert(name.to_owned(), copied);
        }
        self.changed.get_mut(name).expect("bucket was just copied")
    }

    pub fn insert(&mut self, node: &Node) {
        self.bucket_mut(&node.name).add(node);
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
            if self.bucket(name).is_none() {
                continue;
            }
            let bucket = self.bucket_mut(name);
            bucket.all.retain(|id| !removed.contains(id));
            bucket.references.retain(|(_, owner)| *owner != file);
            bucket.calls.retain(|id| !removed.contains(id));
            if let Some(functions) = bucket.functions.remove(&file) {
                bucket.function_count -= functions.len();
            }
            if bucket.all.is_empty() && !self.base.contains_key(name) {
                self.changed.remove(name);
            }
        }
    }

    pub fn named(&self, name: &str) -> &[NodeId] {
        self.bucket(name).map_or(&[], |bucket| &bucket.all)
    }

    pub fn references(&self, name: &str) -> &[(NodeId, FileId)] {
        self.bucket(name).map_or(&[], |bucket| &bucket.references)
    }

    pub fn call_sites(&self, name: &str) -> &[NodeId] {
        self.bucket(name).map_or(&[], |bucket| &bucket.calls)
    }

    /// Resolve declarations in order of confidence: same file, imported files,
    /// then exactly one workspace-wide declaration. Never guess between names
    /// that are ambiguous without import visibility.
    pub fn call_targets(&self, name: &str, file: FileId, visible: &HashSet<FileId>) -> Vec<NodeId> {
        let Some(bucket) = self.bucket(name) else {
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
