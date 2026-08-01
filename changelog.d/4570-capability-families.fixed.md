- **A mock harness again owns every effect it stands in for.** Exposing an
  effect as a typed `Harness` method also registered it as a capability
  implementation, and that implementation ran for real ahead of the mock. Mock
  interception now precedes capability and builtin dispatch, so a mocked
  `harness.project.scan(...)` returns its canned response instead of scanning
  the filesystem.
- **`harness.llm.call` and `harness.llm.completion` narrow `.data` from their
  `output` schema.** The narrowing only applied to the removed ambient
  `llm_call`, so the same request typed as `any` when made through the capability
  handle. Both forms now resolve through one owner and cannot disagree.
- **Profiles attribute LLM time to the calling step again.** Span
  classification matched removed ambient builtin names, so a call through
  `harness.llm.*` recorded no LLM span. Capability calls now resolve to the same
  registry entry the ambient global did, and profile and audit identically.
- **`harn fix` narrows an existing root grant instead of only prepending one.**
  When an attenuated helper wants `HarnessFs` and the caller still passes
  `harness`, the repair rewrites the argument to `harness.fs`, or to
  `{fs: harness.fs, tools: harness.tools}` for a two-capability record. Repairs
  now locate the argument in the parsed source, so a diagnostic that points at
  no call argument produces no edit rather than a guessed one.
- **`HARN-LNT-069` reports a two-capability helper accurately.** The rule now
  reads parameter defaults, which execute in the callable's scope and can use
  authority just like the body, and it stays quiet when a nested closure
  shadows the parameter it is reasoning about. Both cases previously produced
  advice that would have stranded a grant. The rule remains advisory: narrowing
  a signature is only safe when every call site moves with it, and a caller can
  live in a module the linter never sees.
- **One owner parses `${...}` holes.** The typechecker, the linter, and
  `harn fix` share `harn_parser::interpolation`, so all three see spans in the
  containing file's coordinates.
- **A builtin alias no longer registers as a second capability method.** Adding
  a legacy alias to a builtin that is also exposed as `harness.<cap>.<method>`
  made the manifest see one builtin as two.
