- The Harn bump runtime's base adoption now restores a refreshed file the
  refresh deleted and staged, instead of refusing on a dirty tree and never
  converging. Untracked files that predate the refresh are no longer counted as
  refresh output, so adoption never deletes them.
