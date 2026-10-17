//! Storage for the typed symbol graph: one immutable generation plus a delta.
//!
//! A restore builds a [`GraphBase`] in a few linear passes: nodes sorted by
//! id, an id-to-slot table, and forward and reverse adjacency in compressed
//! sparse row form. Nothing writes to it afterwards. Edits go to a
//! [`GraphDelta`] holding added nodes, tombstones for removed base nodes, and
//! copy-on-write replacements for the adjacency and file lists they change.
//! Readers consult the delta before the base, so a clone shares the base and
//! copies only the delta, and [`GraphStore::compact`] folds a grown delta
//! into a fresh base.
//!
//! Node ids only grow, so every added node's id is above every base id. A
//! lookup below the base's id range never needs the delta's node map.

use std::sync::Arc;

use foldhash::{HashMap as FastMap, HashSet as FastSet};

use super::file_table::FileId;
use super::parallel::{joined, scope};
use super::symbol_graph::{Edge, Node, NodeId};

/// Slot-table entry for an id the base does not hold.
const NO_SLOT: u32 = u32::MAX;

/// The graph's nodes and edges. See the module documentation.
#[derive(Debug, Default, Clone)]
pub(super) struct GraphStore {
    base: Arc<GraphBase>,
    delta: GraphDelta,
}

#[derive(Debug, Default)]
struct GraphBase {
    /// Sorted by id.
    nodes: Vec<Node>,
    /// Indexed by id; the node's position in `nodes`, or [`NO_SLOT`].
    slots: Vec<u32>,
    out: Csr,
    inc: Csr,
    /// Each file's node ids in ascending order.
    by_file: FastMap<FileId, Vec<NodeId>>,
}

/// Edges grouped by node slot: slot `s` owns `edges[offsets[s]..offsets[s + 1]]`.
#[derive(Debug, Default)]
struct Csr {
    offsets: Vec<u32>,
    edges: Vec<Edge>,
}

#[derive(Debug, Default, Clone)]
struct GraphDelta {
    added: FastMap<NodeId, Node>,
    /// Base nodes that are no longer part of the graph.
    removed: FastSet<NodeId>,
    /// Complete replacement edge lists, for any node whose list changed.
    out: FastMap<NodeId, Vec<Edge>>,
    inc: FastMap<NodeId, Vec<Edge>>,
    /// Complete replacement node lists; empty for a file whose nodes are gone.
    by_file: FastMap<FileId, Vec<NodeId>>,
}

impl Csr {
    fn get(&self, slot: usize) -> &[Edge] {
        match (self.offsets.get(slot), self.offsets.get(slot + 1)) {
            (Some(&start), Some(&end)) => &self.edges[start as usize..end as usize],
            _ => &[],
        }
    }

    /// Offsets for grouping `edges` by `key`, which must be below `slots`.
    fn offsets(slots: usize, edges: &[Edge], key: impl Fn(&Edge) -> usize) -> Vec<u32> {
        let mut offsets = vec![0u32; slots + 1];
        for edge in edges {
            offsets[key(edge) + 1] += 1;
        }
        for slot in 0..slots {
            offsets[slot + 1] += offsets[slot];
        }
        offsets
    }

    /// Group by `key` with a stable counting sort, keeping `edges` as is
    /// when it is already grouped.
    fn group(slots: usize, edges: Vec<Edge>, key: impl Fn(&Edge) -> usize) -> Self {
        let offsets = Self::offsets(slots, &edges, &key);
        if edges.windows(2).all(|pair| key(&pair[0]) <= key(&pair[1])) {
            return Self { offsets, edges };
        }
        Self::scatter(offsets, &edges, key)
    }

    /// Group a copy of `edges` by `key`; the reverse direction of a list
    /// grouped by source.
    fn grouped_copy(slots: usize, edges: &[Edge], key: impl Fn(&Edge) -> usize) -> Self {
        let offsets = Self::offsets(slots, edges, &key);
        Self::scatter(offsets, edges, key)
    }

    fn scatter(offsets: Vec<u32>, edges: &[Edge], key: impl Fn(&Edge) -> usize) -> Self {
        let Some(&first) = edges.first() else {
            return Self {
                offsets,
                edges: Vec::new(),
            };
        };
        let mut cursor = offsets.clone();
        let mut grouped = vec![first; edges.len()];
        for edge in edges {
            let slot = key(edge);
            grouped[cursor[slot] as usize] = *edge;
            cursor[slot] += 1;
        }
        Self {
            offsets,
            edges: grouped,
        }
    }
}

impl GraphBase {
    fn slot(&self, id: NodeId) -> Option<usize> {
        self.slots
            .get(id as usize)
            .copied()
            .filter(|slot| *slot != NO_SLOT)
            .map(|slot| slot as usize)
    }
}

impl GraphStore {
    /// Build a base generation from snapshot facts. `derive` runs on its own
    /// thread over the validated nodes while adjacency is built, for indexes
    /// owned elsewhere (the name index).
    pub fn from_parts<R: Send>(
        next_id: NodeId,
        mut nodes: Vec<Node>,
        edges: Vec<Edge>,
        derive: impl FnOnce(&[Node]) -> R + Send,
    ) -> Result<(Self, R), &'static str> {
        if !nodes.is_sorted_by_key(|node| node.id) {
            nodes.sort_unstable_by_key(|node| node.id);
        }
        let mut slots = vec![NO_SLOT; nodes.last().map_or(0, |node| node.id as usize + 1)];
        for (slot, node) in nodes.iter().enumerate() {
            if node.id == 0 || node.id >= next_id || slots[node.id as usize] != NO_SLOT {
                return Err("symbol graph has an invalid or duplicate node id");
            }
            slots[node.id as usize] = slot as u32;
        }
        if u32::try_from(edges.len()).is_err() {
            return Err("symbol graph has more edges than its offsets can address");
        }
        let slot_of = |id: NodeId| slots.get(id as usize).copied().unwrap_or(NO_SLOT);
        if edges
            .iter()
            .any(|edge| slot_of(edge.from) == NO_SLOT || slot_of(edge.to) == NO_SLOT)
        {
            return Err("symbol graph edge names an absent node");
        }
        let count = nodes.len();
        let by_source = |edge: &Edge| slot_of(edge.from) as usize;
        let by_target = |edge: &Edge| slot_of(edge.to) as usize;
        let (derived, inc, by_file) = scope(|scope| {
            let derived = scope.spawn(|| derive(&nodes));
            let inc = scope.spawn(|| Csr::grouped_copy(count, &edges, by_target));
            let mut by_file: FastMap<FileId, Vec<NodeId>> = FastMap::default();
            for node in &nodes {
                by_file.entry(node.file_id).or_default().push(node.id);
            }
            (joined(derived), joined(inc), by_file)
        });
        let out = Csr::group(count, edges, by_source);
        let base = GraphBase {
            nodes,
            slots,
            out,
            inc,
            by_file,
        };
        Ok((
            Self {
                base: Arc::new(base),
                delta: GraphDelta::default(),
            },
            derived,
        ))
    }

    /// Fold the delta into a fresh base. Nodes stay in id order and each
    /// node's outgoing edges keep their order; incoming lists are rebuilt in
    /// source order, exactly as a restore of the same graph would see them.
    pub fn compact<R: Send>(
        &mut self,
        next_id: NodeId,
        derive: impl FnOnce(&[Node]) -> R + Send,
    ) -> R {
        let ids = self.ids_sorted();
        let mut edges = Vec::with_capacity(self.edge_count());
        for id in &ids {
            edges.extend_from_slice(self.outgoing(*id));
        }
        let store = std::mem::take(self);
        let mut added = store.delta.added;
        let removed = store.delta.removed;
        let base_nodes = match Arc::try_unwrap(store.base) {
            Ok(base) => base.nodes,
            Err(shared) => shared.nodes.clone(),
        };
        let mut nodes: Vec<Node> = base_nodes
            .into_iter()
            .filter(|node| !removed.contains(&node.id))
            .collect();
        let mut tail: Vec<Node> = added.drain().map(|(_, node)| node).collect();
        tail.sort_unstable_by_key(|node| node.id);
        nodes.extend(tail);
        let (store, derived) = Self::from_parts(next_id, nodes, edges, derive)
            .expect("a live graph satisfies its own invariants");
        *self = store;
        derived
    }

    /// Entries the delta holds; a measure of how far reads have drifted
    /// from the compact base.
    pub fn delta_weight(&self) -> usize {
        self.delta.added.len()
            + self.delta.removed.len()
            + self.delta.out.len()
            + self.delta.inc.len()
    }

    pub fn base_len(&self) -> usize {
        self.base.nodes.len()
    }

    fn in_base_range(&self, id: NodeId) -> bool {
        (id as usize) < self.base.slots.len()
    }

    /// The live base slot of `id`, if the base holds it and it is not removed.
    fn base_slot(&self, id: NodeId) -> Option<usize> {
        if !self.delta.removed.is_empty() && self.delta.removed.contains(&id) {
            return None;
        }
        self.base.slot(id)
    }

    pub fn node(&self, id: NodeId) -> Option<&Node> {
        if self.in_base_range(id) {
            self.base_slot(id).map(|slot| &self.base.nodes[slot])
        } else {
            self.delta.added.get(&id)
        }
    }

    pub fn contains(&self, id: NodeId) -> bool {
        self.node(id).is_some()
    }

    pub fn len(&self) -> usize {
        self.base.nodes.len() - self.delta.removed.len() + self.delta.added.len()
    }

    /// Every live node; order unspecified.
    pub fn iter(&self) -> impl Iterator<Item = &Node> {
        let removed = &self.delta.removed;
        self.base
            .nodes
            .iter()
            .filter(move |node| removed.is_empty() || !removed.contains(&node.id))
            .chain(self.delta.added.values())
    }

    /// Every live node id, ascending.
    pub fn ids_sorted(&self) -> Vec<NodeId> {
        let mut added: Vec<NodeId> = self.delta.added.keys().copied().collect();
        added.sort_unstable();
        let removed = &self.delta.removed;
        let mut ids: Vec<NodeId> = Vec::with_capacity(self.len());
        ids.extend(
            self.base
                .nodes
                .iter()
                .map(|node| node.id)
                .filter(|id| removed.is_empty() || !removed.contains(id)),
        );
        ids.extend(added);
        ids
    }

    pub fn outgoing(&self, id: NodeId) -> &[Edge] {
        if let Some(edges) = self.delta.out.get(&id) {
            return edges;
        }
        self.base_slot(id)
            .map_or(&[], |slot| self.base.out.get(slot))
    }

    pub fn incoming(&self, id: NodeId) -> &[Edge] {
        if let Some(edges) = self.delta.inc.get(&id) {
            return edges;
        }
        self.base_slot(id)
            .map_or(&[], |slot| self.base.inc.get(slot))
    }

    pub fn edge_count(&self) -> usize {
        self.iter().map(|node| self.outgoing(node.id).len()).sum()
    }

    pub fn file_nodes(&self, file: FileId) -> &[NodeId] {
        self.delta
            .by_file
            .get(&file)
            .or_else(|| self.base.by_file.get(&file))
            .map_or(&[], Vec::as_slice)
    }

    /// Files owning at least one live node, ascending.
    pub fn file_ids(&self) -> Vec<FileId> {
        let mut files: Vec<FileId> = self
            .base
            .by_file
            .keys()
            .filter(|file| !self.delta.by_file.contains_key(file))
            .chain(
                self.delta
                    .by_file
                    .iter()
                    .filter(|(_, ids)| !ids.is_empty())
                    .map(|(file, _)| file),
            )
            .copied()
            .collect();
        files.sort_unstable();
        files
    }

    /// Add a node whose id the caller has just allocated above every
    /// existing id.
    pub fn insert(&mut self, node: Node) {
        debug_assert!(!self.in_base_range(node.id), "node ids only grow");
        self.file_nodes_mut(node.file_id).push(node.id);
        self.delta.added.insert(node.id, node);
    }

    pub fn add_edge(&mut self, edge: Edge) {
        self.outgoing_mut(edge.from).push(edge);
        self.incoming_mut(edge.to).push(edge);
    }

    /// Take `file`'s node list, leaving the file with no nodes.
    pub fn take_file_nodes(&mut self, file: FileId) -> Option<Vec<NodeId>> {
        let ids = match self.delta.by_file.remove(&file) {
            Some(ids) => ids,
            None => self.base.by_file.get(&file)?.clone(),
        };
        if self.base.by_file.contains_key(&file) {
            self.delta.by_file.insert(file, Vec::new());
        }
        (!ids.is_empty()).then_some(ids)
    }

    /// Remove `id` and every edge touching it. Returns whether it existed.
    pub fn remove(&mut self, id: NodeId) -> bool {
        let (outs, ins) = if self.in_base_range(id) {
            let Some(slot) = self.base_slot(id) else {
                return false;
            };
            let outs = self
                .delta
                .out
                .remove(&id)
                .unwrap_or_else(|| self.base.out.get(slot).to_vec());
            let ins = self
                .delta
                .inc
                .remove(&id)
                .unwrap_or_else(|| self.base.inc.get(slot).to_vec());
            self.delta.removed.insert(id);
            (outs, ins)
        } else {
            if self.delta.added.remove(&id).is_none() {
                return false;
            }
            (
                self.delta.out.remove(&id).unwrap_or_default(),
                self.delta.inc.remove(&id).unwrap_or_default(),
            )
        };
        for edge in outs {
            if self.contains(edge.to) {
                self.incoming_mut(edge.to).retain(|other| other.from != id);
            }
        }
        for edge in ins {
            if self.contains(edge.from) {
                self.outgoing_mut(edge.from).retain(|other| other.to != id);
            }
        }
        true
    }

    /// `id`'s outgoing list, copied out of the base on first write.
    pub fn outgoing_mut(&mut self, id: NodeId) -> &mut Vec<Edge> {
        let slot = self.base_slot(id);
        let base = &self.base;
        self.delta
            .out
            .entry(id)
            .or_insert_with(|| slot.map_or_else(Vec::new, |slot| base.out.get(slot).to_vec()))
    }

    /// `id`'s incoming list, copied out of the base on first write.
    pub fn incoming_mut(&mut self, id: NodeId) -> &mut Vec<Edge> {
        let slot = self.base_slot(id);
        let base = &self.base;
        self.delta
            .inc
            .entry(id)
            .or_insert_with(|| slot.map_or_else(Vec::new, |slot| base.inc.get(slot).to_vec()))
    }

    fn file_nodes_mut(&mut self, file: FileId) -> &mut Vec<NodeId> {
        let base = &self.base;
        self.delta
            .by_file
            .entry(file)
            .or_insert_with(|| base.by_file.get(&file).cloned().unwrap_or_default())
    }
}
