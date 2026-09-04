- **Windows agent commands can see the host's installed toolchains again
  (#7993).** A confined child on Windows runs under an AppContainer, which
  reads a file only when that file's permissions admit it. The sandbox granted
  read on the workspace and on toolchain directories under the user's home, so
  anything installed system-wide was invisible: `node`, and any other tool on
  `PATH` outside the home directory, failed with "'node' is not recognized as
  an internal or external command" even though the command, the search path,
  and the working directory were all correct. Directories on the launching
  process's `PATH` that hold a runnable command, plus the standard system
  prefixes and the hosted tool cache, are now part of the read set on every
  profile rather than only when an embedder opts into the developer-toolchains
  preset. Directories the host
  already opens to sandboxed programs are detected and left untouched, so the
  common case adds no work and changes no permissions, and the broad system
  prefixes are never rewritten at all. A read grant that fails now leaves that
  one directory unreadable and lets the command run, instead of failing the
  whole command; grants the child actually depends on still fail loudly.
  Writes are unchanged and still confined to the workspace.
