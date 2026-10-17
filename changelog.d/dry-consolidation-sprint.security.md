- **Filesystem snapshots stay inside their session folder.** Snapshot and
  session ids that are empty or made only of dots (`.`, `..`) now map to a
  hashed folder name, as staged-write ids already did. Before, a `..` snapshot
  id wrote its manifest and bodies into the shared parent directory. Snapshots
  previously stored under a `...` id are no longer found.
