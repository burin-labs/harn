<!-- Written by a review-profile calibration session (anthropic:claude-opus-5-5).
     Edit freely; a rerun copies forward rules a person wrote. -->
# Review profile

The automated reviewer reads this file before every review of this repository. A `REVIEW.md` in a
subdirectory adds rules for changes under it.

## Review bar

- A semantic decision has one owner; CLI, TUI, IDE, headless, cloud, portable kernel, and shell/JS
  projections must be adapters or parity-tested projections, not parallel implementations. Flag a
  second owner only after confirming the existing owner is not already reused at this head.
  (AGENTS.md#Ownership; Ecosystem working agreement)
- Harn owns orchestration, transcript lifecycle, replay/eval, worker lineage, capability policy, and
  mutation-session audit metadata; hosts own presentation, approval UX, concrete file mutations, and
  undo/redo. Judge a finding against the owner of the behavior. (AGENTS.md#Ownership)
- Every output registered in `scripts/generated_artifacts.toml` is generated: a hand edit to one
  is a defect, and the fix edits its source and regenerates. Examples include `spec/HARN_SPEC.md`,
  `docs/src/language-spec.md`, the provider catalog projections, and the provider docs. A new
  artifact needs Makefile targets, a registry entry, `make all`, and CI wiring. (AGENTS.md#Source
  ownership and generated files; `scripts/generated_artifacts.toml`)
- Provider catalog changes edit only catalog_sources/ and capability_sources/ fragments, ship a row
  and capability rule on every route the model is served on, and give every catalog test a negative
  control. Capability claims need a live probe, not a model card. (AGENTS.md#Adding a model or
  provider to the catalog)
- Tests must not use real-time sleeps, wall-clock polling, SystemTime::now(), or short recv_timeout;
  use paused Tokio time, EventLog::subscribe(), or OrchestratorHarness. (AGENTS.md#Verification;
  docs/src/dev/testing.md)
- Evidence must match the claim: test counts alone are not proof, product claims exercise the
  canonical user path, and liveness claims cover progress, interruption, recovery, and terminal
  state. (AGENTS.md#Working agreement)
- Pre-v2 `{{name}}` missing-identifier passthrough in templates must be preserved; new template
  constructs fail with parse errors, and only render_template_result parses and evaluates templates.
  (AGENTS.md#Source ownership and generated files)
- Public stdlib functions need explicit return types (named closed records, Result<T,E>, or typed
  maps, not any or open dict), and builtins register through #[harn_builtin]. (AGENTS.md#Source
  ownership and generated files)
- Non-trivial PRs add one `changelog.d/<id>.<category>.md` fragment. Public CLI, builtin, or
  host-capability changes require user-facing docs and help. (AGENTS.md#Editing and changelog;
  Cross-surface changes)
- Syntax, parser, or keyword changes need conformance coverage plus tree-sitter tests. Prompt-syntax
  changes need template conformance fixtures and vocabulary regeneration. (AGENTS.md#Verification;
  Cross-surface changes)
- website/: no bare UI strings (use src/i18n/en.ts), CTAs must not overpromise (nothing runs Harn in
  the browser), and code snippets stay narrow. (website/AGENTS.md#Conventions)

## Known non-defects

- A finding built on an earlier revision (a missing guard, a permissive parser, a missing
  registration, docs not yet updated) still applies. Not a defect: Several withdrawn findings
  described code or docs that had already been fixed at the reviewed head. Re-read the current lines
  before you post. (PRs #9282, #9366 (host_request.rs unknown matcher fields, boundary requirement),
  #9416, #9426, #9496, #9631, #9659, #9748, #9791)
- A value captured only inside a conditional branch is missing on the other path. Not a defect: In
  approval_rules, EvaluationContext::new captures literal identity unconditionally, and the
  workspace-boundary branch only refreshes it. Trace the constructor before you claim a value is
  absent. (PR #9366 (approval_rules.rs:643))
- Command::env_clear() on a spawned child weakens a gate the parent test enforces. Not a defect:
  env_clear affects only the child's environment. A required-test check that runs in the parent is
  unaffected. (PR #9353 linux_bwrap_probe.rs)
- Native VM and portable kernel limits must be identical, or else it is a divergence defect. Not a
  defect: The portable kernel has its own 1 MiB value ceiling enforced by its resource validator.
  The native VM's separate budget does not set the portable contract. (PR #9691 harn-kernel
  arithmetic.rs)
- An aggregator wire ID (e.g. anthropic/claude-haiku-4.5) should resolve through a direct provider's
  canonical catalog route. Not a defect: Wire IDs belong to the aggregator's catalog row. Hosted
  projection resolves a canonical catalog ID first and then writes the wire model. (PR #9385
  managed_supply.rs; AGENTS.md provider catalog section)
- A renamed catalog key with a private wire id revives a retired route. Not a defect: A new key plus
  a private wire id is an open-world route. The retired route stays blocked by its full catalog
  selector. (PR #9437 retired_routes.rs)
- A canary or test that uses a private temporary store fails to prove protection of the shared
  store. Not a defect: Integrity canaries isolate their store on purpose so they never attack the
  live shared store. (PR #9489 kache_store_integrity_test.sh)
- url_build double-encodes existing percent escapes in file: paths. Not a defect: The pinned url
  crate 2.5.8 preserves existing percent escapes in its path setter. (PR #9684 url_parse.rs)
- Conformance --record/--replay can reach live providers. Not a defect: Serial and parallel
  conformance launchers both install the live-dispatch kill switch, and the VM refuses a missing
  Replay fixture before HTTP. (PR #9751 harn-cli test/mod.rs)
- A refactor pre-edit gate accepts malformed Python line continuations. Not a defect: The
  tree-sitter parse reports an ERROR first, and a golden pins that refusal. Check parser behavior
  before you assume a lexical check is the only gate. (PR #9478 refactor_core.rs)

## Authoritative commands

- `make all`: Full local gate.
- `make test`: Workspace tests through cargo-nextest.
- `HARN_TEST_ONE_NAME='module::tests::case' make test-one`: One exact Rust test passes. An undefined
  name is refused and zero matches fail loudly.
- `make check-drift`: Generated artifacts match their sources (source checks).
- `make check-drift-binary`: Binary-classified generated checks match after registry or
  executable-semantics changes.
- `make conformance`: The executable language/runtime specification passes.
- `make lint-harn && make fmt-harn`: .harn sources lint and format cleanly.
- `make check-docs-snippets`: Docs code blocks are valid.
- `make wasm-check`: harn-wasm generated bindings, authority imports, and the real worker path work.
- `npm run portal:lint && npm run portal:test && npm run portal:build`: The portal UI is correct.
- `(cd tree-sitter-harn && npm test)`: The grammar is correct.
- `(cd website && npm run typecheck && npm run test && npm run build)`: The website builds and
  SSR-prerenders every doc page.
- `./scripts/gh_check_state.sh --repo OWNER/NAME --sha <sha>`: Canonical CI verdict for a head.

## Risky paths

- `crates/harn-vm/src/orchestration/policy/approval_rules/`: Capability and approval policy: a
  defect broadens allow decisions.
- `crates/harn-vm/src/stdlib/sandbox/`: Process sandboxing (bwrap, Landlock, macOS sandbox-exec
  nesting).
- `crates/harn-cli (MCP/OAuth, A2A/ACP)`: Credentials and the external protocol surface.
- `crates/harn-vm/src/llm/catalog_sources/, capability_sources/`: Route and capability downgrades,
  and generated catalog drift.
- `.github/workflows/, scripts/lib/release_candidate_run.sh, docs/src/maintainer-release.md`:
  Release, merge queue, and CI gating. A red publishable-crates check on main blocks release.
- `Cargo.toml (crate include lists, build scripts, dependency bounds)`: Verify publishable crates
  runs after merge, so a failure blocks the next release.
- `crates/harn-vm/src/stdlib/template/`: The single template engine, and its backward-compatible
  passthrough.
