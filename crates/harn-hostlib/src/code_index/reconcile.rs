//! Incremental reconciliation with consistent, resumable snapshot boundaries.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::file_table::FileId;
use super::imports_go::GoModules;
use super::state::{mtime_ms_of, now_unix_ms, relative_path_under_root, RefreshOutcome};
use super::walker::walk_workspace;
use super::IndexState;
use crate::HarnReferenceResolver;

/// A partial walk must not promote the workspace HEAD: files not reached yet
/// still need content verification even when their stat metadata matches.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct ReconcileProgress {
    git_head: Option<String>,
    verified_files: HashSet<FileId>,
}

#[cfg(test)]
#[path = "reconcile_tests.rs"]
mod tests;

pub(super) struct CheckpointPolicy {
    /// Parsed files, rather than cheap checks of unchanged content.
    pub files: usize,
    pub interval: Duration,
}

impl Default for CheckpointPolicy {
    fn default() -> Self {
        Self {
            files: 512,
            interval: Duration::from_secs(30),
        }
    }
}

impl CheckpointPolicy {
    pub fn disabled() -> Self {
        Self {
            files: usize::MAX,
            interval: Duration::MAX,
        }
    }
}

impl IndexState {
    pub(super) fn refresh_with_checkpoints(
        &mut self,
        resolver: Option<&HarnReferenceResolver>,
        policy: CheckpointPolicy,
        mut checkpoint: impl FnMut(&Self),
    ) -> RefreshOutcome {
        let _phase = super::timing::Phase::start("reconcile.total");
        let root = self.root.clone();
        let live_head = super::git_head::read_git_head(&root);
        let mut outcome = RefreshOutcome {
            git_head_changed: self.git_head != live_head
                || self
                    .reconcile_progress
                    .as_ref()
                    .is_some_and(|p| p.git_head != live_head),
            ..RefreshOutcome::default()
        };
        if outcome.git_head_changed {
            if self
                .reconcile_progress
                .as_ref()
                .is_none_or(|p| p.git_head != live_head)
            {
                self.reconcile_progress = Some(ReconcileProgress {
                    git_head: live_head.clone(),
                    verified_files: HashSet::new(),
                });
            }
        } else {
            self.reconcile_progress = None;
        }
        let mut seen = HashSet::with_capacity(self.files.len());
        let mut graph_dirty = false;
        let mut deps_dirty = false;
        let mut harn_touched = false;
        let mut completed = 0;
        let mut last_checkpoint = Instant::now();
        let mut manifests: Vec<String> = Vec::new();
        walk_workspace(
            &root,
            |abs, meta| {
                outcome.files_scanned += 1;
                let Some(rel) = relative_path_under_root(&root, abs) else {
                    return;
                };
                let known = self.path_to_id.get(&rel).copied();
                if let Some(id) = known {
                    let verified = self
                        .reconcile_progress
                        .as_ref()
                        .is_none_or(|p| p.verified_files.contains(&id));
                    if verified
                        && self.files.get(&id).is_some_and(|file| {
                            file.size_bytes == meta.len() && file.mtime_ms == mtime_ms_of(meta)
                        })
                    {
                        outcome.files_unchanged += 1;
                        seen.insert(rel);
                        return;
                    }
                }
                match self.ingest(abs, Some(meta)) {
                    Some(ingested) => {
                        let id = ingested.id;
                        let changed = ingested.changed;
                        seen.insert(rel.clone());
                        if known.is_none() {
                            outcome.files_added += 1;
                            deps_dirty = true;
                        } else if changed {
                            self.rebuild_deps(id, &rel);
                        }
                        if changed {
                            outcome.files_reindexed += 1;
                            completed += 1;
                            harn_touched |=
                                self.files.get(&id).is_some_and(|f| f.language == "harn");
                            // Parse before evaluating elapsed time. Reading a batch
                            // is often cheap while graph repair takes minutes.
                            // Unvisited files keep their old metadata throughout.
                            self.rebuild_symbol_graph_for(id, &ingested.source);
                            graph_dirty = true;
                        } else {
                            outcome.files_touched_only += 1;
                        }
                        if let Some(progress) = &mut self.reconcile_progress {
                            progress.verified_files.insert(id);
                        }
                        if completed >= policy.files || last_checkpoint.elapsed() >= policy.interval
                        {
                            self.finish_reconcile_batch(&mut graph_dirty, &mut deps_dirty);
                            checkpoint(self);
                            completed = 0;
                            last_checkpoint = Instant::now();
                        }
                    }
                    None => {
                        super::timing::skipped(abs);
                        outcome.files_skipped += 1;
                    }
                }
            },
            |manifest| manifests.push(manifest),
        );

        // Delete only after a complete walk. A partial checkpoint retains the
        // old rows of files not yet visited, including their old metadata.
        let stale: Vec<_> = self
            .path_to_id
            .keys()
            .filter(|rel| !seen.contains(*rel))
            .cloned()
            .collect();
        for rel in stale {
            harn_touched |= self
                .path_to_id
                .get(&rel)
                .and_then(|id| self.files.get(id))
                .is_some_and(|f| f.language == "harn");
            self.remove_relative_path(&rel);
            outcome.files_removed += 1;
        }
        if outcome.files_removed > 0 {
            deps_dirty = true;
        }
        self.finish_reconcile_batch(&mut graph_dirty, &mut deps_dirty);
        // Like deletion, manifests apply only after a complete walk: a
        // checkpoint keeps the manifests its saved call graph was linked
        // under, so a partial snapshot never mixes two resolutions.
        outcome.files_reresolved =
            self.apply_module_manifests(GoModules::read(&root, manifests)) as u64;
        self.git_head = live_head;
        self.reconcile_progress = None;
        if harn_touched {
            self.relink_harn_references(resolver);
        }
        if !outcome.is_noop() {
            self.last_built_unix_ms = now_unix_ms();
        }
        outcome
    }

    fn finish_reconcile_batch(&mut self, graph_dirty: &mut bool, deps_dirty: &mut bool) {
        if *deps_dirty {
            // Resolve additions together against the complete batch path set,
            // rather than rebuilding every file's imports for each new path.
            self.rebuild_all_deps();
            *deps_dirty = false;
            *graph_dirty = true;
        }
        if !*graph_dirty {
            return;
        }
        self.link_symbol_imports();
        self.relink_dirty_calls();
        *graph_dirty = false;
    }
}
