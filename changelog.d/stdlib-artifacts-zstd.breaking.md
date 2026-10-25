- **`harn_vm::bytecode_cache::EmbeddedStdlibArtifact` stores its artifact
  compressed.** The `bytes` field is replaced by `zstd`, one zstd frame, and
  `len`, the artifact's decoded size. Only `harn-stdlib-artifacts`' build script
  writes these rows, and hosts that call `harn_stdlib_artifacts::register()`
  are unaffected.

  Migration: a hand-built table compresses each artifact and records its size:

  ```rust
  // before
  EmbeddedStdlibArtifact { file_name, bytes }
  // after
  EmbeddedStdlibArtifact { file_name, len: bytes.len(), zstd: compressed }
  ```
