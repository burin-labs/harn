Reclaim an idle target's compiler incremental cache with
`scripts/prune_stale_targets.sh --prune-incremental-entry NAME` while retaining
linked binaries, dependencies and unrelated targets. The collector refuses
live or unmeasured ownership and reports interrupted partial reclamation.
