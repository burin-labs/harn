Run a final-polish pass on the current PR or feature branch before it lands.

The goal is the state a senior reviewer would land without comments. The sweep
is targeted at the recurring issues this repo accumulates between "the feature
works" and "the PR is landable" — not a generic code review.

## Phase 0 — identify the work

```bash
git status --short
git rev-parse --abbrev-ref HEAD
git log --oneline origin/main..HEAD
gh pr view --json number,state,headRefName,mergeStateStatus 2>/dev/null
```

If `gh pr view` returns a PR, you are in **PR mode**. Otherwise **branch mode** —
skip rebase/CI phases and go straight to the review sweep.

Always read the full diff before touching anything: `git diff origin/main...HEAD`.
In PR mode, also read existing review comments — sometimes the easiest finding
is one a reviewer already left.

## Phase 1 — rebase on origin/main (PR mode only)

```bash
git fetch origin main
git rebase origin/main
```

Conflict rules in this repo:

- `CHANGELOG.md`: keep both sides; preserve the top heading.
- `docs/theme/harn-keywords.js`: regenerate with `make gen-highlight`.
- `docs/src/language-spec.md`: regenerate via the pre-commit hook (edit
  `spec/HARN_SPEC.md` instead).
- `Cargo.lock`: take theirs and re-run `cargo check --workspace`.
- Any file where both sides reformatted: take one side then run the
  appropriate formatter (`cargo fmt`, `make fmt-harn`, portal lint).

Then `git push --force-with-lease`. Never bare `--force`.

## Phase 2 — drive CI to green (PR mode only)

```bash
gh pr checks
gh run list --branch "$(git branch --show-current)" --limit 5
gh run view <run-id> --log-failed   # for each failure
```

Address every failure comprehensively — no `continue-on-error` papering, no
silenced asserts. Common categories:

- **Format / clippy / lint:** `make fmt`, `make lint`, `make lint-harn`,
  `make fmt-harn`. Commit the fixes.
- **Test failure:** reproduce locally with the narrowest command
  (`cargo nextest run -p <crate> <test>`), then fix the root cause.
- **Conformance:** `cargo run --bin harn -- test conformance --filter <name>`.
  If `.expected` needs an update for an intentional behavior change, do it
  explicitly.
- **Portal / VS Code / tree-sitter:** run the relevant
  `npm run portal:lint|build|test` / `(cd tree-sitter-harn && npm test)`.
- **Drift checks** (`check-language-spec`, `check-highlight`,
  `check-trigger-quickref`): re-run the generators and commit the outputs.
- **`make lint-test-patterns`:** see Phase 3.2.
- **`check-docs-snippets`:** a `.harn` snippet in docs no longer parses; fix
  the snippet, not the parser.

If a failure is a known-flaky unrelated test, `gh run rerun --failed`. Don't
silence it.

## Phase 3 — the review sweep

For every category below, fix material findings inline. Note non-material ones
in your summary so the user can decide.

### 3.1 correctness / perf / security / privacy / reliability

- Off-by-ones, unhandled `Option`/`Result`, panics on hostile input, integer
  overflow on user-controlled arithmetic.
- Unbounded allocations driven by external input (request bodies, network
  iterators, recursive structures with no depth cap).
- Concurrency: shared mutable state without sync, deadlock from nested locks,
  cancellation safety on `select!` arms.
- Secrets in logs, transcripts, error messages, request/response bodies. This
  repo writes structured logs liberally — confirm nothing user-controlled or
  token-shaped lands in them.
- File operations against ambient cwd in agent-invocable paths — prefer
  worktree-backed execution.
- Provider/network code: timeouts set, retries bounded, backoff applied,
  partial reads handled.

### 3.2 flaky test patterns

The deflake epic (#1057) banned wall-clock polling from the fast suite.
`make lint-test-patterns` enforces literal patterns; you also need to catch
the spirit:

| Pattern | Replacement |
|---|---|
| `std::thread::sleep` | `tokio::time::pause()` + `advance()` |
| `tokio::time::sleep` outside `start_paused = true` | `start_paused` runtime + `advance()` |
| `while … Instant::now()` polling loops | `EventLog::subscribe()` + `tokio::time::timeout` |
| `SystemTime::now()` in tests | `MockClock` / injected timestamp |
| `recv_timeout(Duration::from_millis(50))` | `tokio::time::timeout` on event channel |
| Sleep-then-assert ("wait long enough that the thing happened") | Subscribe to the event the thing emits |
| Hand-rolled `tokio::spawn` + sleep + flag check | `OrchestratorHarness` / `MockProcess` |

Also flag: tests synchronized only by spawn-sleep-assert (even in allowlisted
files); wall-clock literals without a named constant; E2E tests sharing
process-wide globals under parallel execution. Reference: `docs/src/dev/testing.md`.

### 3.3 sloppy comments — strip aggressively

- **Historical narration:** "previously this used X", "this used to live in
  foo.rs", "removed the old fallback path." Git blame is the historical record.
- **Restating obvious code:** `// increment counter`, `// loop over items`,
  `// return result`. If the comment paraphrases the line below, delete it.
- **References to private/cross-org repos by name:** this codebase is
  open-source. Replace `burin-code`, internal product names, etc. with neutral
  phrasing — "a Harn integrator", "a Harn host", "a Harn Cloud workspace", "a
  downstream Harn consumer." External integrators read this code.
- **Task/PR-bound comments:** "added for #1234", "see PR #4567", "needed by the
  foo flow." Move to the commit / PR description; delete the comment.
- **Multi-paragraph docstrings on small functions.** Module-level architectural
  docstrings are fine; 12-line docstrings on one-line helpers are noise.

When in doubt: delete it. The bar is "would removing this confuse a future
reader." If no, it goes.

### 3.4 cross-crate drift

A change that looks local often leaves another crate stale.

- **Lexer/parser change** → tree-sitter grammar (`tree-sitter-harn/`), VS Code
  grammar (`editors/vscode/`), conformance tests, `spec/HARN_SPEC.md`,
  highlight keywords.
- **New/changed builtin** → stdlib registration is authoritative, but check
  `harn-lint` builtin awareness, `harn-fmt` formatting, `harn-lsp` completion,
  docs (`docs/src/`), `docs/llm/harn-quickref*.md`, conformance coverage.
- **Type-checker change** → conformance, lint awareness, LSP hover/diagnostics,
  portal display of run records.
- **Runtime / VM change** → portal record schema, transcripts, replay/eval, DAP
  variable inspection, ACP/A2A surface if exposed, `CHANGELOG.md`.
- **Provider / LLM-call change** → `harn-quickref.md` `llm_call` options table,
  conformance, transcripts.
- **Prompt-template change** → `crates/harn-vm/src/stdlib/template.rs` is the
  one parser; do not add a second. Also `docs/src/prompt-templating.md`, the
  prompt section of `harn-quickref.md`, and `conformance/tests/template_*`.
  New keywords/filters/sections go in
  `crates/harn-vm/src/stdlib/template/vocabulary.rs`; regenerate the VS Code
  grammar with `make gen-prompt-grammar` rather than editing it.
- **CLI surface change** → help text, README, docs, `harn-quickref.md`.

If the diff touches behavior the portal renders, run the portal locally
(`npm run portal:dev`) and click through — unit tests miss UI drift.

### 3.5 DRY / polish

- Two near-identical functions where one parameterized helper fits. (Three:
  definitely.)
- Inline string constants used in multiple places — promote to `const`.
- Manual `match` on an enum that could be a method.
- `unwrap()` / `expect()` in non-test code where a typed error is appropriate.
- Redundant clones / `.to_string()` / `.into_iter().collect::<Vec<_>>()`
  round-trips.

But: do not invent abstractions for hypothetical second uses. Three similar
lines is not a refactor opportunity.

### 3.6 Rust → Harn opportunities

If the diff adds Rust code that looks like agent orchestration plumbing —
sequencing LLM calls, retrying with different prompts, collecting structured
outputs, branching on tool-use results — ask whether it should be a `.harn`
script composing primitive host capabilities.

Heuristics:

- `llm_call`-equivalent in a loop with conditional retries → Harn script
  with `parallel settle` / `schema_retries`.
- Fan out work over a list, gather results → `parallel each`.
- Pick a prompt based on input shape → Harn function.

When the answer is yes, prefer the Harn form. The Rust side should be the
primitive host capability, not the orchestration. This is the trust boundary
the repo is built on. If the refactor is too big for the current PR, file an
issue and link it.

### 3.7 prompt prose → `.harn.prompt`

Long prompt strings embedded in Rust (or as inline `.harn` literals) should
usually live in `.harn.prompt` files rendered via `template.render` /
`render_prompt(...)`. Why: harness authors can override without forking;
templates support includes, partials, filters; diff hygiene improves.

Look for:

- `r#"..."#` over ~10 lines containing model instructions.
- `format!("...")` assembling instructions from runtime data.
- Magic-string default prompts hardcoded next to logic.

Move them to `.harn.prompt`, expose an override hook, and replace the inline
string with a `render` call.

### 3.8 overly long files

Files past ~800 lines spanning multiple concerns are a split candidate:

- Module with three independent submodules separated by banner comments →
  three `mod foo` files.
- Struct with 40+ methods grouped by concern (parsing / formatting /
  executing) → split impl blocks across files, or break the struct by concern.
- Inline tests > 500 lines → move to `tests.rs` sibling.

Don't split for the sake of splitting; split when the file no longer fits in
a single mental model.

### 3.9 interpreter perf wins

If the diff touches `crates/harn-vm` execution paths, look for patterns that
bundle naturally:

- Per-frame allocations that could move to the frame pool.
- `clone()` on values that could be borrows.
- Hot-path `HashMap` lookups that could be small-vector linear scans (or
  vice versa for large maps).
- Unnecessary `Box`/`Rc`/`Arc` on values that are effectively `Copy`-sized.
- Redundant string interning / re-interning.

Bundle only when the perf path overlaps the diff. Don't open a "while I'm
here" rabbit hole that doubles the PR scope.

## Phase 4 — re-validate

```bash
make fmt
make lint-harn fmt-harn
cargo nextest run -p <crates touched by the diff>
```

For wider changes:

```bash
make test
cargo run --bin harn -- test conformance --filter <relevant>
```

Then push (`--force-with-lease` if you rewrote history, plain push otherwise)
and let CI re-run.

## Phase 5 — land

```bash
gh pr merge --auto
```

Don't pass `--squash` / `--merge` / `--rebase` — branch protection picks the
strategy.

## Anti-patterns

- Disabling a failing test to make CI green — fix the test or the code.
- `#[allow(...)]` to silence clippy — fix the pattern.
- `--force` without `--with-lease` — you can't see co-authors' pushes.
- `--no-verify` to skip hooks — they gate the same things CI does.
- "While I'm here" refactors the user did not ask for — note them, file
  follow-ups, move on.
- Hand-editing generated files (`docs/src/language-spec.md`,
  `docs/theme/harn-keywords.js`) — edit the source and regenerate.

## References

- `AGENTS.md` / `CLAUDE.md` — repo conventions and command surface
- `docs/src/dev/testing.md` — deflake patterns (#1057) and bans
- `docs/llm/harn-quickref.md` — Harn scripting reference
- `spec/HARN_SPEC.md` — language spec source of truth
- `crates/harn-vm/src/stdlib/template.rs` — sole prompt-template engine
