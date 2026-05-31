# Contributing preset hooks

This guide is for contributors adding a new rule (or a whole new stack
catalogue) to the shipped `preset_run_command` corpus. The shipped
"harn-canon" catalogues live under
[`crates/harn-stdlib/src/stdlib/stdlib_tool_hooks_catalogues.harn`](https://github.com/burin-labs/harn/blob/main/crates/harn-stdlib/src/stdlib/stdlib_tool_hooks_catalogues.harn);
the user-facing reference is [Preset tool hooks](../tool-hooks.md) and
recipes are in [Cookbook: tool hooks](../cookbooks/tool-hooks.md).

The bar for adding to harn-canon is intentionally higher than for a
custom rule in your own harness — these run by default for everyone
who opts into the stack, so a noisy rule punishes thousands of
sessions before anyone notices. Use this checklist.

## Decide: rule, catalogue, or harness-local?

| Scope | Where it lives | When to use |
|---|---|---|
| **harness-local** (one project) | `custom_rules` in your `preset_run_command(...)` call | Vendor-specific footgun; project-internal convention; experimenting before upstreaming. |
| **per-package** (one bundle) | A catalogue exported from your Harn package, registered into the host's `registry:` | Reusable corpus across multiple agents you ship together; vendor patch overlaying the shipped catalogues. |
| **harn-canon** (every user) | A rule in an existing `tool_hooks_catalogue_<stack>()` builder, or a new `tool_hooks_catalogue_<newstack>()` builder | Universally-true ecosystem advice that nearly every agent would benefit from. See the bar below. |

Most rules should start harness-local. Promote to per-package once two
or more harnesses copy the same rule. Promote to harn-canon only when
the rule satisfies the bar below.

## The harn-canon bar

A rule belongs in harn-canon when it's:

1. **Stack-canonical.** The advice is from upstream docs, an
   ecosystem-blessed style guide, or a widely cited post-mortem.
   Cite the source in `references`.
2. **Universally beneficial.** Triggering on a false-positive should
   be at worst harmless (an extra flag the user wanted anyway). Rules
   that block legitimate workflows belong in per-package overlays, not
   in harn-canon.
3. **Cheap to evaluate.** The predicate runs on every `run_command`
   dispatch. Regex anchors at the leading command (`^cmd\b`) keep the
   sweep linear. Callable predicates that allocate per-call are
   acceptable but should be O(command length).
4. **Stable under churn.** Don't add a rule that depends on
   bleeding-edge CLI flags; wait for the upstream tool to stabilize.
5. **Recoverable.** Prefer `severity: "warning"` with a rewrite over
   `severity: "error"` with a deny. The agent's transcript shows the
   rewrite plus the `tool_rewritten` reminder, so the loop self-corrects.
   Reserve denies for irreversible or policy-violating commands (the
   universal catalogue's `git push --force main` and root-adjacent
   `rm -rf` are the prototypes).

If a candidate fails any bar item, ship it as a custom rule in your
harness or as a per-package overlay first and let real-world usage
prove it out.

## Adding a rule to an existing catalogue

1. **Open the catalogue builder.** Find the
   `tool_hooks_catalogue_<stack>()` function in
   `crates/harn-stdlib/src/stdlib/stdlib_tool_hooks_catalogues.harn`.
2. **Write a `tool_rule(...)` value.** Follow the conventions in the
   module HarnDoc:
   - `id` shaped `<stack>.<tool>.<short>` (e.g.
     `rust.cargo.test_no_capture_default`).
   - `pattern` anchored at the leading command (`^cmd\b`) so siblings
     don't trip it. Use a callable predicate when you need
     "matches X but not when Y is present" — the Rust `regex` crate
     has no lookaround, and the shared `__cat_pred_prefix` helper in
     the catalogues file already encodes this pattern.
   - `applies_to` matches the catalogue's stack.
   - `severity` defaults to `warning`; reserve `error` for denies and
     `info` for "we noticed but the rewrite is purely a quality-of-life
     improvement" (e.g. `pytest -s`).
   - `explanation` is one sentence the agent can paraphrase back to
     the user. Avoid jargon, link to docs in `references` instead.
   - `references` should be at least one canonical doc link. Prefer
     upstream tool docs over blog posts; cite post-mortems only when
     they're widely known.
   - `rewrite`, if present, is `(command, _context) -> string | nil`.
     Return `nil` or the same string to skip the rewrite (rule still
     fires; the audit envelope flows). Use the shared
     `__cat_append_flag(command, flag)` helper for idempotent flag
     appends.
3. **Append it to the catalogue's rule list** in the
   `__cat_build(...)` call at the end of the builder.
4. **Add a conformance fixture.** Each catalogue has a paired test
   file under `conformance/tests/stdlib/tool_hooks/tool_hooks_catalogue_<stack>.harn`
   plus its `.expected`. Mirror the existing structure — one
   `tool_hooks_match` call per rule outcome (one hit, one miss). The
   `tool_hooks_catalogue_rust.harn` fixture is the canonical
   reference shape.
5. **Update docs.** Add the rule to the shipped catalogues table in
   [Preset tool hooks](../tool-hooks.md). For stack-specific recipes
   that the rule unblocks, add or update an entry in
   [Cookbook: tool hooks](../cookbooks/tool-hooks.md).
6. **CHANGELOG.** Add an Unreleased entry under
   `## Unreleased > ### Added` referencing the issue / PR and the
   new rule id.
7. **Gate locally:**

   ```bash
   make conformance       # tool_hooks_catalogue_<stack> cases pass
   make lint-md           # docs lint clean
   make check-docs-snippets   # harn,ignore fences only — others must type-check
   ./scripts/build_docs_site.sh  # mdBook builds clean and emits raw .md companions
   ```

## Adding a new stack catalogue

1. **Stack name.** Pick a single lowercase identifier (`go`, `ruby`,
   `terraform`, ...). The
   [`tool_hooks_seed_registry`](https://github.com/burin-labs/harn/blob/main/crates/harn-stdlib/src/stdlib/stdlib_tool_hooks_catalogues.harn)
   stack-to-builder mapping is hardcoded — unknown stacks are silently
   skipped, so callers opting into a future stack don't break, but
   you must add yours to `__cat_for_stack(stack)` before it fires.
2. **Catalogue builder.** Add a
   `pub fn tool_hooks_catalogue_<stack>() { ... }` next to the
   existing ones. Use `__cat_build("harn-canon/<stack>", "<stack>", [...])`
   so the registry's `tool_hooks_filter` scoping works and audit
   consumers see consistent ids.
3. **Seed wiring.** Add the
   `if stack == "<stack>" { return tool_hooks_catalogue_<stack>() }`
   arm to `__cat_for_stack` so `preset_run_command({stacks: ["<stack>"]})`
   auto-seeds the new catalogue.
4. **Conformance.** Add a `tool_hooks_catalogue_<stack>.harn` test
   plus `.expected`, modelled on
   `tool_hooks_catalogue_rust.harn`. Each rule needs at least one hit
   and one miss case.
5. **Docs.** Add a row to the shipped-catalogues table in
   [Preset tool hooks](../tool-hooks.md). Add at least one recipe in
   [Cookbook: tool hooks](../cookbooks/tool-hooks.md). Add the stack
   to any quickref tables under `docs/llm/harn-quickref.md` that
   enumerate supported stacks.
6. **CHANGELOG.** Add an Unreleased entry that lists the catalogue id,
   the rules it ships, and the issue / PR.

## Promotion: harness-local → per-package → harn-canon

The natural ladder is:

1. **Harness-local custom rule.** Lives in one project's
   `custom_rules: [...]`. Zero coordination cost; only fires for that
   harness.
2. **Per-package catalogue.** Once two or more harnesses copy the
   same rule, factor it into a Harn package that exports a
   `catalogue(...)` value, and have each harness register it into
   their preset's `registry`. The package can ship its own
   `.expected` fixtures and version semantics. This is the
   recommended home for vendor-specific or org-specific catalogues.
3. **harn-canon graduation.** Move the rule into
   `stdlib_tool_hooks_catalogues.harn` once the bar above is met
   and the rule has at least a quarter of cross-harness real-world
   usage without bug reports.

When you graduate a rule from per-package to harn-canon:

- Update the per-package catalogue to leave the rule out (callers
  using both will get it via the auto-seed, no duplicates).
- File a deprecation note in the per-package CHANGELOG pointing to
  the harn-canon catalogue.
- Update the rule's `id` to the `<stack>.<tool>.<short>` convention
  if it isn't already.

## Gotchas

- **No lookaround in `regex` strings.** The Rust `regex` crate doesn't
  support `(?=...)` / `(?!...)`. Use a callable predicate plus the
  shared `__cat_pred_prefix(prefix_re, suppress_markers)` helper for
  "matches X but not when Y is present" shapes.
- **Anchor on the leading command.** `^cmd\b` keeps the rule from
  tripping on `cmdsuffix` or `wrapper cmd`. The shared `__cat_*`
  helpers all assume this convention.
- **Be careful with prefix-extension overlap.** `*.js` matches
  `*.json` without an explicit terminator. The shared
  `__cat_pred_find_for_ext` helper handles this for `find -name`
  rules; if you roll your own, require `(?:["']|\\s|$)` after the
  extension.
- **Environment escape hatches go through `env_or`.** The universal
  deny rules read `HARN_TOOL_HOOKS_ALLOW_FORCE_PUSH` via `env_or` so
  test contexts can opt out without mutating the process environment.
  If your rule needs an escape hatch, follow that pattern and
  document the env var in the rule's `explanation`.
- **Idempotent rewrites.** `__cat_append_flag(command, flag)` checks
  for the flag's presence before appending so the rule fires once and
  stops. Your rewriter should be safe to apply multiple times to its
  own output — the catalogue-stack auto-seed path can stack with
  custom rules.
- **One sentence per `explanation`.** Audit consumers render it inline.
  Multi-sentence rationales belong in `references` (link to the docs).
- **Don't depend on `now()`, `random()`, or `shell()` in predicates.**
  Predicates run during the linear sweep on every dispatch and must
  be pure. Side effects belong in the mode callback.

## Reviewing a rule PR

Reviewers should check:

- [ ] Rule id follows `<stack>.<tool>.<short>` convention and is
      unique within the catalogue.
- [ ] Pattern is anchored (`^cmd\b` or `__cat_pred_prefix`).
- [ ] Severity matches behavior (`error` only for denies).
- [ ] `references` cite upstream docs.
- [ ] Rewrite is idempotent and uses `__cat_append_flag` or similar.
- [ ] Conformance fixture covers both a hit and a miss.
- [ ] Docs updated: shipped-catalogues table, cookbook recipe (when
      the rule unblocks a workflow), CHANGELOG Unreleased entry.
- [ ] `make conformance`, `make lint-md`, `make check-docs-snippets`,
      and `./scripts/build_docs_site.sh` all pass locally.

## Tracking

- Epic: [#1884 — Preset run-command hook library](https://github.com/burin-labs/harn/issues/1884).
- This guide: TH-08 ([#1901](https://github.com/burin-labs/harn/issues/1901)).
- Catalogue source: [`crates/harn-stdlib/src/stdlib/stdlib_tool_hooks_catalogues.harn`](https://github.com/burin-labs/harn/blob/main/crates/harn-stdlib/src/stdlib/stdlib_tool_hooks_catalogues.harn).
- Conformance suite: `conformance/tests/stdlib/tool_hooks/tool_hooks_*.harn`.
