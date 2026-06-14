- **`read_range` reads a raw path again when the code index is unbuilt.** The
  read-only secondary-roots work (#3352) routed `read_range` through a resolver
  that returned no path when the primary index slot was `None` (never rebuilt),
  so reads erred with "path must stay within the indexed workspace root". This
  broke callers that read a file before any rebuild — `agent_run` scanning a
  process-output temp file to surface buried test-failure lines, and eval/verify
  reads over arbitrary shell output. Restored the pre-#3352 fallback: with an
  unbuilt primary index, resolve the raw path so the read still succeeds (a
  genuinely missing path still fails with "file not found").
