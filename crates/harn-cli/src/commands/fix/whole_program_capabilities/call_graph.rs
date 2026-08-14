//! Invocation edges and fixed-point authority propagation.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use harn_builtin_meta::CapabilityId;

use super::{canonical, CanonicalPathCache, ProgramCallable, ProgramFile};

#[derive(Debug, Clone, Copy)]
pub(super) struct ProgramEdge {
    pub(super) caller: usize,
    pub(super) call_idx: usize,
    pub(super) callee: usize,
    pub(super) propagates_authority: bool,
}

pub(super) fn resolve_edges(
    files: &[ProgramFile],
    callables: &[ProgramCallable],
    module_graph: &harn_modules::ModuleGraph,
) -> Vec<ProgramEdge> {
    let by_file_name = callables
        .iter()
        .enumerate()
        .map(|(idx, callable)| {
            (
                (
                    files[callable.file_idx].path.clone(),
                    callable.info.name.clone(),
                ),
                idx,
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut canonical_paths = CanonicalPathCache::new(canonical);
    let mut edges = Vec::new();
    for (caller_idx, caller) in callables.iter().enumerate() {
        let caller_path = &files[caller.file_idx].path;
        for (call_idx, call) in caller.info.calls.iter().enumerate() {
            let target = module_graph
                .definition_of(caller_path, &call.callee)
                .and_then(|definition| {
                    by_file_name
                        .get(&(canonical_paths.get(&definition.file), definition.name))
                        .copied()
                });
            if let Some(callee) = target {
                edges.push(ProgramEdge {
                    caller: caller_idx,
                    call_idx,
                    callee,
                    propagates_authority: call.authority_scope.is_none(),
                });
            }
        }
    }
    edges
}

pub(super) fn propagate_requirements(
    edges: &[ProgramEdge],
    requirements: &mut [BTreeSet<CapabilityId>],
    root_requirements: &mut [bool],
) {
    let mut callers_by_callee = vec![BTreeSet::new(); requirements.len()];
    for edge in edges {
        if edge.propagates_authority {
            callers_by_callee[edge.callee].insert(edge.caller);
        }
    }

    let mut queued = requirements
        .iter()
        .zip(root_requirements.iter())
        .map(|(requirement, root_required)| !requirement.is_empty() || *root_required)
        .collect::<Vec<_>>();
    let mut pending = queued
        .iter()
        .enumerate()
        .filter_map(|(idx, queued)| queued.then_some(idx))
        .collect::<VecDeque<_>>();

    while let Some(callee) = pending.pop_front() {
        queued[callee] = false;
        let propagated = requirements[callee].clone();
        let root_propagated = root_requirements[callee];
        for &caller in &callers_by_callee[callee] {
            let before = requirements[caller].len();
            let root_before = root_requirements[caller];
            requirements[caller].extend(propagated.iter().copied());
            root_requirements[caller] |= root_propagated;
            if (requirements[caller].len() > before || root_requirements[caller] != root_before)
                && !queued[caller]
            {
                queued[caller] = true;
                pending.push_back(caller);
            }
        }
    }
}
