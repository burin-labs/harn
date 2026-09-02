- The LLM outcome vocabularies are now part of the protocol contract.
  `category`, `kind`, and `reason` on the `harn.acp.prompt_error.v1` envelope
  have a named owner in every generated binding: `HarnLlmErrorCategory`,
  `HarnLlmErrorKind`, and `HarnLlmErrorReason` in Rust and Swift,
  `LLM_ERROR_CATEGORIES` / `LLM_ERROR_KINDS` / `LLM_ERROR_REASONS` in
  TypeScript, and the matching enums in Python and Go. A host no longer has to
  guess which failure strings Harn actually emits.
- `code` on the same envelope is documented as a provider passthrough with no
  closed set. It is opaque diagnostic text; branch on `reason` instead.
- The Rust binding gains open enums for those vocabularies and for the agent
  terminal class, kind, and owner. Each carries an `Unrecognized(String)`
  escape, so a host pinned to an older Harn round-trips a newer value verbatim
  instead of folding it into a neighbouring variant. The existing
  `AGENT_TERMINAL_*` string constants remain for one release.
