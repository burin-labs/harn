Run tapes now keep a host command's request on its `process:` tool-call record, so a reader can
say what ran. Before this, only a digest of the arguments was recorded. An `env` map is kept as its
sorted variable names, never its values, and the rest of the request passes through the shared
redaction policy. Branch serving still matches by digest and never reads the request, so a
redacted request does not stop a tape from branching.

Migration: Rust code that constructs `TapeRecordKind::ToolCall` adds `request: None`, and an
exhaustive pattern on it adds `request: _` or `..`.
