# pr-finish-pass

Final-polish sweep on the current PR or feature branch before it lands. See
the canonical playbook at
[`.codex/skills/pr-finish-pass/SKILL.md`](../skills/pr-finish-pass/SKILL.md).

## TL;DR

Three phases:

1. **Sync** — if a PR is open, rebase on `origin/main`, resolve conflicts,
   `git push --force-with-lease`. Drive every CI failure to green by fixing
   the root cause; never paper over with retries or `--no-verify`.
2. **Sweep** — read the full diff (`git diff origin/main...HEAD`) and walk
   the nine review categories below. Fix every material finding inline.
3. **Land** — re-validate locally, push, `gh pr merge --auto`.

## Review categories

For each, fix material findings inline and note non-material ones in the
end-of-turn summary.

1. **Correctness / perf / security / privacy / reliability bugs.** Off-by-ones,
   unhandled errors, panics on hostile input, unbounded allocations from
   external input, concurrency hazards, secrets in logs/transcripts, ambient
   cwd in agent-invocable paths, missing timeouts/retries/backoff.

2. **Flaky test patterns.** Catches what `make lint-test-patterns` doesn't.
   Banned: `std::thread::sleep`, `tokio::time::sleep` outside `start_paused`,
   `Instant::now()` polling, `SystemTime::now()` in tests, `recv_timeout`
   with short literal, sleep-then-assert, hand-rolled spawn+sleep+flag.
   Approved: `tokio::time::pause()` + `advance()`, `EventLog::subscribe()` +
   `tokio::time::timeout`, `OrchestratorHarness`, `MockProcess`, `MockClock`.
   Reference deflake epic #1057 and `docs/src/dev/testing.md`.

3. **Sloppy comments — strip aggressively.** Historical narration
   ("previously this used X"), restating obvious code, references to private
   or cross-org repos by name (use neutral phrasing — "a Harn integrator",
   "a Harn host", "a Harn Cloud workspace", "a downstream Harn consumer"),
   task/PR-bound comments ("added for #1234"), multi-paragraph docstrings on
   small functions. The bar: would removing it confuse a future reader?
   If no, delete.

4. **Cross-crate drift.** Lexer/parser → tree-sitter, VS Code grammar,
   conformance, `spec/HARN_SPEC.md`, highlight keywords. Builtin → lint/fmt/
   LSP awareness, docs, `harn-quickref*.md`, conformance. Type-checker →
   conformance, lint, LSP, portal records. Runtime/VM → portal schema,
   transcripts, replay/eval, DAP, ACP/A2A, CHANGELOG. Provider → quickref
   `llm_call` table, conformance, transcripts. Prompt-template →
   `crates/harn-vm/src/stdlib/template.rs` (the one parser),
   `docs/src/prompt-templating.md`, quickref, VS Code prompt grammar,
   `conformance/tests/template_*`. CLI → help text, README, docs, quickref.
   Run the portal locally if its surface is affected.

5. **DRY / polish.** Two-or-more near-identical functions → parameterized
   helper. Inline string constants → `const`. `unwrap()`/`expect()` in
   non-test code → typed error. Redundant clones / `.to_string()` /
   `.into_iter().collect()` round-trips. But: don't invent abstractions for
   hypothetical second uses.

6. **Rust → Harn opportunities.** Agent-orchestration plumbing (sequencing
   `llm_call`s, retrying with prompts, branching on tool-use results,
   fanning out and gathering) belongs in a `.harn` script composing
   primitive host capabilities, not in Rust. The Rust side is the host
   capability; the orchestration is Harn. This is the trust boundary. If
   the refactor is too big for the current PR, file a follow-up issue and
   link it.

7. **Prompt prose → `.harn.prompt` files.** Long prompt strings embedded in
   Rust or as inline `.harn` literals (`r#"..."#` over ~10 lines, `format!`
   building instructions, magic-string defaults next to logic) should live in
   `.harn.prompt` files rendered via `template.render` / `render_prompt(...)`,
   with a configurable override path so harness authors can swap them
   without forking.

8. **Overly long files.** Files past ~800 lines spanning multiple concerns
   are split candidates. Three independent submodules → three `mod foo`
   files. Struct with 40+ methods grouped by concern → split impl blocks.
   Inline tests > 500 lines → `tests.rs` sibling. Don't split for sake of
   splitting; split when the file no longer fits in a single mental model.

9. **Interpreter perf wins** that bundle naturally with the diff. If
   `crates/harn-vm` execution paths are touched: per-frame allocations
   that could use the frame pool, `clone()` that could be a borrow, hot-path
   `HashMap` lookups that could be small-vector linear scans (or vice
   versa), unnecessary `Box`/`Rc`/`Arc` on `Copy`-sized values, redundant
   string interning. Only bundle when the perf path overlaps the diff.

## Conflict rules in this repo

- `CHANGELOG.md`: keep both sides, preserve top heading.
- `docs/theme/harn-keywords.js`: regenerate with `make gen-highlight`.
- `docs/src/language-spec.md`: regenerate via pre-commit hook (edit
  `spec/HARN_SPEC.md` instead).
- `Cargo.lock`: take theirs, re-run `cargo check --workspace`.
- Both-sides reformatted file: take one side, run the formatter.

## Re-validation

```bash
make fmt
make lint-harn fmt-harn
cargo nextest run -p <crates touched>
```

Wider:

```bash
make test
cargo run --bin harn -- test conformance --filter <relevant>
```

Then `git push --force-with-lease` (if history was rewritten) and let CI
run before `gh pr merge --auto`.

## Anti-patterns

- Disabling a failing test or `#[allow(...)]`-ing clippy to force green.
- `git push --force` without `--with-lease`.
- `git commit --no-verify` to skip hooks.
- "While I'm here" refactors the user didn't ask for.
- Hand-editing generated files instead of regenerating from source.
