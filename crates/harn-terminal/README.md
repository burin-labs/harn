# harn-terminal

Cross-platform pseudo-terminal sessions with typed VT screen capture.

The crate is deliberately independent of the Harn VM and product code. It
owns the reusable PTY lifecycle, typed key encoding, resize handling,
revision-fenced idle waits, cursor/cell/style capture, bounded diagnostics, and
child cleanup. Policy and schema validation belong to the embedding host; the
`harn-hostlib` adapter supplies those boundaries for Harn pipelines.

This is infrastructure for explicitly trusted terminal harnesses. It does not
weaken or replace a host's process sandbox.
