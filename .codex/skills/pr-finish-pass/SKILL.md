---
name: pr-finish-pass
description: Final-polish pass on the current PR or feature branch before it lands. Rebases on origin/main, force-pushes, drives CI to green, then reviews the diff for correctness/perf/security/reliability bugs, flaky test patterns banned by `make lint-test-patterns`, sloppy comments a senior engineer would not write, cross-crate drift (typechecker / formatter / portal / DAP / LSP / tree-sitter), DRY/polish, Rust orchestration logic that should be Harn script composing host capabilities, prompt prose that belongs in `.harn.prompt` files, long files that should be split, and interpreter perf wins that bundle naturally with the change. Triggers when the user says "finish pass", "polish this PR", "wrap up the PR", or otherwise asks for a final review-and-fix sweep before merge.
---

# pr-finish-pass

Final-polish sweep on whatever work is currently in flight — usually a
named PR, but it works equally well on an unpushed feature branch or a
session's accumulated diff. The goal is to get the change to a state a
senior reviewer would land without comments.

The sweep is **not** a generic code review. It targets the recurring
issues this repo accumulates between "the feature works" and "the PR is
landable."

## When to use

- A PR is open and you want it merged today.
- A feature branch is about to become a PR.
- A session produced a diff worth landing and you want it polished
  before opening the PR.

If there is no current branch worth polishing — say so and stop.

## Phase 0 — identify the work

Determine the scope before touching anything:

```bash
git status --short
git rev-parse --abbrev-ref HEAD                    # current branch
git log --oneline origin/main..HEAD                # commits to ship
gh pr view --json number,state,headRefName,mergeStateStatus 2>/dev/null
```

If `gh pr view` returns a PR, you are in **PR mode**. Otherwise you are
in **branch mode** — the rebase / CI phases are skipped and you go
straight to the review sweep.

Read the full diff before doing anything else:

```bash
git diff origin/main...HEAD
```

For PRs in PR mode, also read `gh pr view --json title,body,comments`
and skim review comments — sometimes the easiest finding is one a
reviewer already left.

## Phase 1 — rebase on origin/main (PR mode only)

```bash
git fetch origin main
git rebase origin/main
```

Resolve conflicts inline. Common conflict rules in this repo:

- `CHANGELOG.md`: keep both sides; preserve the top heading.
- `docs/theme/harn-keywords.js`: regenerate with `make gen-highlight`.
- `docs/src/language-spec.md`: regenerate via the pre-commit hook (edit
  `spec/HARN_SPEC.md` instead).
- `Cargo.lock`: take theirs and re-run `cargo check --workspace`.
- Any file with both sides reformatted: take one side then `cargo fmt`
  / `make fmt-harn` / portal lint as appropriate.

Then force-push with lease:

```bash
git push --force-with-lease
```

Never `--force` without `--with-lease` — co-authors may have pushed.

## Phase 2 — drive CI to green (PR mode only)

```bash
gh pr checks
gh run list --branch "$(git branch --show-current)" --limit 5
```

For each failing check, fetch the actual log and read it:

```bash
gh run view <run-id> --log-failed
```

Address failures comprehensively — do not paper over them with retries
or `continue-on-error`. Common categories:

- **Format / clippy / lint failures:** run `make fmt`, `make lint`,
  `make lint-harn`, `make fmt-harn` and commit the fixes.
- **Test failures:** reproduce locally with the narrowest possible
  command (`cargo nextest run -p <crate> <test_name>`) before fixing.
  Read the test, read the code under test, and fix the root cause.
- **Conformance failures:** `cargo run --bin harn -- test conformance
  --filter <name>` reproduces a single case. If a `.expected` file
  needs updating because user-visible behavior intentionally changed,
  do that explicitly — don't blindly accept new output.
- **Portal / VS Code / tree-sitter failures:** run the corresponding
  `npm run portal:lint` / `npm run portal:build` / `npm test` locally.
- **`make check-language-spec` / `make check-highlight` /
  `make check-trigger-quickref`:** these are drift checks. Re-run the
  generators and commit the regenerated outputs.
- **`make lint-test-patterns`:** see the flaky-test guidance in
  Phase 3.
- **`make check-docs-snippets`:** a `.harn` snippet under `docs/src/`
  no longer parses or type-checks. Fix the snippet, not the parser.

Push fixes incrementally; let CI re-run between iterations rather than
batching every guess at once.

If a failure is a known-flaky test unrelated to the diff, re-run the
specific job (`gh run rerun <run-id> --failed`). Do not silence it.

## Phase 3 — the review sweep

This is the substance of the skill. Walk the diff and look for each of
the following classes of finding. Fix every material one inline; note
non-material findings in your end-of-turn summary so the user can
decide whether to address them.

### 3.1 correctness, performance, security, privacy, reliability

- Off-by-ones, unhandled `Option`/`Result`, panics on hostile input,
  integer overflow on user-controlled arithmetic.
- Unbounded allocations driven by external input (request body sizes,
  iterator chains over network responses, recursive structures with no
  depth cap).
- Concurrency: shared mutable state without synchronization, deadlocks
  from nested locks, cancellation safety on `select!` arms.
- Secrets in logs, request/response bodies, error messages, transcripts.
  This crate writes a lot of structured logs — check whether anything
  user-controlled or token-shaped can hit them.
- File operations relative to ambient cwd in code paths that can be
  invoked from agents — prefer worktree-backed execution.
- Provider/network code paths: timeouts set, retries bounded, backoff
  applied, partial reads handled.

### 3.2 flaky test patterns

The deflake epic ([#1057]) removed wall-clock polling from the fast
suite. `make lint-test-patterns` enforces the bans, but it only catches
the literal patterns. Look for the spirit too:

[#1057]: https://github.com/burin-labs/harn/issues/1057

| Pattern | Replacement |
|---|---|
| `std::thread::sleep` | `tokio::time::pause()` + `advance()` |
| `tokio::time::sleep` outside `start_paused = true` | `start_paused` runtime + `advance()` |
| `while … Instant::now()` polling loops | `EventLog::subscribe()` + `tokio::time::timeout` |
| `SystemTime::now()` in tests | `MockClock` / injected timestamp |
| `recv_timeout(Duration::from_millis(50))` | `tokio::time::timeout` on event channel |
| Sleep-then-assert ("wait long enough that the thing happened") | Subscribe to the event the thing emits and wait for it |
| Hand-rolled `tokio::spawn` + sleep + flag check | `OrchestratorHarness` / `MockProcess` |

Also flag:

- Tests whose only synchronization is "spawn task, sleep N ms, assert
  flag." Even if the lint passes (e.g. it's in an allowlisted file),
  this is a deflake liability.
- Wall-clock literals (`Duration::from_millis(50)`) without a named
  constant or comment justifying the magnitude.
- E2E tests in `*_e2e.rs` files that share a process-wide global —
  parallel execution will race them.

Reference: `docs/src/dev/testing.md` has the canonical patterns.

### 3.3 sloppy comments

Strip every comment that a senior engineer would not have written:

- **Historical narration:** "previously this used X, now it uses Y",
  "this used to live in foo.rs", "removed the old fallback path".
  Git blame is the historical record.
- **Restating obvious code:** `// increment counter`, `// loop over
  items`, `// return the result`. If the comment paraphrases the line
  below it, delete it.
- **References to private / cross-org repos by name:** this repo is
  open-source. Replace `burin-code`, `burin-labs/...`, or specific
  internal product names with neutral phrasing — "a Harn integrator",
  "a Harn host", "a Harn Cloud workspace", "a downstream Harn
  consumer." External integrators read this code.
- **Task-bound or PR-bound comments:** "added for ticket #1234", "see
  PR #4567", "needed by the foo flow". Move the rationale to the
  commit message / PR description and delete the comment.
- **Multi-paragraph docstrings on small functions.** Module-level
  docstrings explaining architecture are fine; a 12-line docstring
  on a one-line helper is noise.

When in doubt: delete it. The bar is "would removing this comment
confuse a future reader." If the answer is no, it goes.

### 3.4 cross-crate drift

Changes that look local often silently leave another crate stale. Walk
the checklist for every public-surface change:

- **Lexer / parser change** → tree-sitter grammar
  (`tree-sitter-harn/`), VS Code grammar (`editors/vscode/`),
  conformance tests, `spec/HARN_SPEC.md`, highlight keywords.
- **New / changed builtin** → stdlib registration is authoritative,
  but check `harn-lint` builtin awareness, `harn-fmt` formatting,
  `harn-lsp` completion, docs (`docs/src/`), the relevant
  `docs/llm/harn-quickref*.md`, conformance coverage.
- **Type-checker change** → conformance, lint awareness, LSP hover/
  diagnostics, portal display of run records.
- **Runtime / VM change** → portal record schema, transcripts, replay/
  eval, DAP variable inspection, ACP/A2A surface if exposed,
  `CHANGELOG.md`.
- **Provider / LLM-call change** → `harn-quickref.md` `llm_call`
  options table, conformance, transcripts.
- **Prompt-template change** → `crates/harn-vm/src/stdlib/template.rs`
  is the one parser; do not add a second. Plus
  `docs/src/prompt-templating.md`, the prompt section of
  `harn-quickref.md`, and `conformance/tests/template_*`. New
  keywords/filters/sections go in
  `crates/harn-vm/src/stdlib/template/vocabulary.rs`; regenerate the
  VS Code grammar with `make gen-prompt-grammar` rather than editing it.
- **CLI surface change** → help text, README, docs, `harn-quickref.md`.

If the diff touches behavior the portal renders, run the portal
locally and check the affected view actually still works — unit tests
miss UI drift.

### 3.5 DRY / polish

- Two near-identical functions where one parameterized helper would
  do. (Three near-identical functions: definitely.)
- Inline string constants used in multiple places; promote to a
  `const`.
- Manual `match` on an enum that could be a method on the enum.
- `unwrap()` / `expect()` in non-test code where a typed error is
  appropriate.
- Redundant clones, redundant `.to_string()`, redundant `.into_iter()
  .collect::<Vec<_>>()` round-trips.

But: do not invent abstractions for hypothetical second uses. Three
similar lines is not a refactor opportunity.

### 3.6 Rust → Harn opportunities

If the diff added Rust code that looks like agent orchestration
plumbing — sequencing LLM calls, retrying with different prompts,
collecting structured outputs, branching on tool-use results — ask:
could this be a `.harn` script composing primitive host capabilities?

Heuristics:

- Code that calls `llm_call`-equivalent APIs in a loop with conditional
  retries → likely a Harn script with `parallel settle` /
  `schema_retries`.
- Code that fans out work over a list and gathers results → likely
  `parallel each`.
- Code that picks a prompt based on input shape → likely a Harn
  function.

If the answer is yes, prefer the Harn script form. The Rust side
should be the primitive host capability, not the orchestration. This
is the trust boundary the repo is built on.

If a refactor is too large for the current PR, file an issue with the
specific surface to migrate and link it from the PR description.

### 3.7 prompt prose that belongs in `.harn.prompt` files

Long prompt strings embedded in Rust (or in `.harn` scripts as inline
literals) should usually live in `.harn.prompt` files and be rendered
via `template.render` / `render_prompt(...)`. Reasons:

- Harness authors can override them without forking the crate.
- Templates support partial application, includes, and filters.
- Diff hygiene improves — prompt edits don't churn unrelated source.

Look for:

- `r#"..."#` literals over ~10 lines containing instructions to a
  model.
- `format!("...")` calls assembling instructions from runtime data.
- Magic-string default prompts hardcoded next to logic.

Move them to `.harn.prompt` files with a clear name, expose an
override hook, and replace the inline string with a `render` call.

### 3.8 overly long files

Files past ~800 lines that span multiple concerns are a refactor
target. Split candidates:

- A module with three independent submodules separated by big banner
  comments → three `mod foo` files.
- A single struct with 40+ methods grouped by concern (parsing /
  formatting / executing) → split into impl blocks in separate files,
  or break the struct by concern.
- Tests inline with > 500 lines of `#[test]` → move to a `tests.rs`
  sibling.

Don't split for the sake of splitting; split when the file no longer
fits in a single mental model.

### 3.9 interpreter perf wins

If the diff touches `crates/harn-vm` execution paths, look for
patterns that bundle naturally:

- Per-frame allocations that could move to the frame pool.
- `clone()` of values that could be borrows.
- `HashMap` lookups in hot paths that could be small-vector linear
  scans (or vice versa for large ones).
- Unnecessary `Box`/`Rc`/`Arc` indirection on values that are
  effectively `Copy`-sized.
- Redundant string interning / re-interning.

Bundle these only when they touch code already in the diff. Don't open
a "while I'm here" perf rabbit hole that doubles the PR scope.

## Phase 4 — re-validate

After making fixes:

```bash
make fmt        # or cargo fmt
make lint-harn fmt-harn
cargo nextest run -p <crates touched by the diff>
```

For wider changes:

```bash
make test
cargo run --bin harn -- test conformance --filter <relevant>
```

Then push (`git push --force-with-lease` if you rewrote history
during the sweep, plain `git push` otherwise) and let CI run.

## Phase 5 — land

Once CI is green and the diff looks landable:

```bash
gh pr merge --auto       # let the merge queue land it
```

Do not pass `--squash` / `--merge` / `--rebase` — branch protection
chooses the strategy.

## Anti-patterns

- **Don't disable a failing test** to make CI green. Fix the test or
  the code.
- **Don't add `#[allow(...)]` to silence clippy.** Fix the underlying
  pattern.
- **Don't `--force` without `--with-lease`.** You can't see your
  co-author's pushes.
- **Don't `--no-verify` to skip hooks.** The hooks gate things CI also
  gates; you'll just fail later.
- **Don't bundle "while I'm here" refactors** the user did not ask
  for. Note them in your summary, file follow-ups, move on.
- **Don't hand-edit generated files** (`docs/src/language-spec.md`,
  `docs/theme/harn-keywords.js`). Edit the source and regenerate.

## Source-of-truth references

- `AGENTS.md` / `CLAUDE.md` — repo conventions and command surface
- `docs/src/dev/testing.md` — deflake patterns and bans
- `docs/llm/harn-quickref.md` — Harn scripting reference
- `spec/HARN_SPEC.md` — language spec source of truth
- `crates/harn-vm/src/stdlib/template.rs` — sole prompt-template engine
