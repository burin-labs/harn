Removed the `HARN-LLM-002` diagnostic code (`Code::DeprecatedLlmOption`,
"LLM option key is deprecated") and its catalog entry. Nothing in the workspace
ever emitted it — its only references were its own declaration and a
repair-template mapping — and a sweep of `burin-code` found no references
there either, so the code advertised a diagnostic Harn could not produce.

This is a breaking change to the published `harn-parser` crate: `Code` is a
public enum, so code that constructs or matches `Code::DeprecatedLlmOption`
no longer compiles. Removed pre-launch deliberately, while the compatibility
promise costs nothing. Reviving it is a revert.

Removed options are reported by `HARN-LNT-050` (`removed-llm-options`), which
is unaffected.
