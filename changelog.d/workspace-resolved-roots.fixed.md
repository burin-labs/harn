- **A workspace opened through a symlink stays inside itself.** The command risk scan now treats a path as inside a
  workspace root when either its written or its resolved spelling sits under the root. A cwd reported by
  `git rev-parse --show-toplevel` or `pwd -P` (for example `/private/tmp/project` for a root declared as
  `/tmp/project`) no longer earns an `outside_workspace` label and an approval hold.
