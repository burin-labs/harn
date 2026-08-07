- Keep the Linux `workspace-tests` Actions cache resident under the 10 GiB
  repo budget by protecting it (with `package-audit`) during prune and reserving
  headroom before the post-merge refresh save, so Windows/macOS nightlies can no
  longer LRU-evict the merge-gate warm graph (#5003).
