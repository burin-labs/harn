- A host-declared sandbox root (a process read, write or Unix-socket root, or
  a capability read-only root) is resolved one component at a time, keeping
  each directory's on-disk spelling, and a root reached through a link that
  sits where a confined process can write is not granted. A write or socket
  root on or over the user's git, gh or ssh configuration is not granted
  either. Each refusal is reported once with the remedy.
