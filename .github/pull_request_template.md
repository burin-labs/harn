<!--
Title this pull request `[Area] Sentence case description`. The area tags are
listed in CONTRIBUTING.md ("Pull request titles and descriptions").
Release pull requests stay exactly `Release vX.Y.Z` — publish-release.yml
matches that subject. Bot titles are left as generated.

Replace everything below with about five sentences covering:
  1. what changed, in behavior a user, agent, or downstream repo sees
  2. why it changed — the defect or the goal
  3. the one risk or honest blind spot
  4. how you verified the claim, at the level of the claim

Leave out what the Files and Checks tabs already show. "All tests pass" says
nothing about whether the tests bind to the behavior you changed.

Worked example:

  `ToolSchema.compact` documented a contract that no renderer read, so a tool
  that declared it was still served its full description on every call. Every
  renderer now serves the shortened text: the native wire payload and the
  OpenAI function-schema projection share one `tool_summary` in Rust, and the
  text catalog renders compact tools as one-liners. The risk is that
  `tool_summary` in Rust and `__agent_tool_summary` in the stdlib are two
  implementations of one policy, held in parity by tests on each side rather
  than by a shared owner. A conformance fixture runs the same agent turn twice
  against registries that differ only in the flag and reads the served byte
  count off a real `provider_call_request`; it fails on the pre-fix binary with
  `full=8585 compact=8585`, which is the exact comparison that measured zero in
  the issue.

  Closes #7767

Gates, for reference:
- Demo gate and Changelog fragment auto-detect from your diff and pass on their
  own when they do not apply. Do NOT add `no-demo-needed` / `no-changelog-needed`
  preemptively; add one only if a gate actually fires and the entry is genuinely
  unnecessary. See .github/workflows/pr-gates.yml.
- Mechanism-contract gate: if this pull request adds or changes a termination,
  escalation, judge, guard, or routing mechanism, a contract under
  `conformance/tests/mechanisms/*.contract.harn` must be present and green. It
  must prove the mechanism fires on its trigger, emits its observable effect,
  and does NOT fire on the negative case. A green contract proves the contract
  only, not end-to-end quality. See conformance/tests/mechanisms/README.md.
- A Mermaid diagram in a ```mermaid fence helps when the data path is
  non-obvious.
-->
