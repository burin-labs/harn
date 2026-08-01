`harn lint` and `harn fix` now repair the globals that were renamed rather than
moved onto a handle. `regex_replace_all` and `task_current` were recorded as
exact behavior-preserving renames, but only the legacy compatibility bridge read
that record, so strict source reported a bare "not defined" and left the call
for a human. They now report `HARN-LNT-001` naming the new spelling, and
`harn fix` rewrites them.

A definition in the same file still wins: a script that declares its own
`fn regex_replace_all` keeps its meaning. An import of the old spelling is still
rewritten, since that is the case the rule exists for.
