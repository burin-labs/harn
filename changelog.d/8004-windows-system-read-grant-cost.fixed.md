- **Windows agent commands no longer stall on every spawn, and now actually
  reach the toolchain they were looking for (#8004).** Opening a system
  toolchain directory to a confined child means rewriting that directory's
  permissions, which on a Node install of roughly 2,400 files takes about a
  second. The sandbox was doing that work for a directory named after a single
  command run and then undoing it again when the command finished, so the cost
  came back on every spawn; it also worked through the search path in order
  under a fixed budget, and on a build machine that budget was spent entirely
  on build output directories before the real toolchain was ever considered.
  Commands timed out, and the toolchain stayed invisible anyway.

  Two things changed. A directory is only considered when it actually contains
  something Windows can run by name, so build output directories, which hold
  object files and headers, no longer displace the directories that answer a
  command. And when a directory does need opening, it is opened to sandboxed
  programs generally, which is the same permission the rest of
  `C:\Program Files` already carries, rather than to one command run. That
  makes the work happen at most once on a machine instead of twice per command:
  afterwards the sandbox's own check, which costs milliseconds, sees the
  directory is already readable and does nothing.

  Reads only. Nothing here grants a sandboxed command permission to write
  anywhere new, and writes remain confined to the workspace.
