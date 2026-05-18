---
name: harn-diagnostics
short: Diagnostics, explain output, repair plans, and fix-safety classes.
description: Use for Harn diagnostics, error codes, explain output, repair metadata, and fix application safety.
when_to_use: Use when working on typechecker diagnostics, lint diagnostics, harn explain, or repair flows.
---

# Harn diagnostics

Use this skill when changing errors, warnings, explain output, repair metadata, or fix application behavior.

Pair it with [[harn-language]] for syntax and type contracts and [[harn-agent]] for autonomy gating.

## Start here

- Parser diagnostics live primarily in `crates/harn-parser/`.
- Typechecker diagnostics live in `crates/harn-parser/src/typechecker/`.
- Lint diagnostics live in `crates/harn-lint/`.
- CLI rendering and `harn explain` live under `crates/harn-cli/src/commands/`.
- Repair planning and apply flows live under `crates/harn-cli/src/commands/fix.rs`.
- Conformance `.error` fixtures are the executable spec for intentional failures.
- LSP diagnostics should derive from the same core metadata as CLI diagnostics.
- Do not fork diagnostic meaning between CLI, LSP, and docs.

## Diagnostic categories

- `TYP` covers type mismatch and typechecker failures.
- `PAR` covers parser syntax failures.
- `NAM` covers naming and resolution failures.
- `CAP` covers capability and trust-boundary failures.
- `LLM` covers LLM provider and model-call failures.
- `ORC` covers orchestration and workflow failures.
- `STD` covers stdlib and builtin failures.
- `PRM` covers prompt-template failures.
- `MOD` covers module-system and import failures.
- `LNT` covers lint findings.
- `FMT` covers formatter findings.
- `IMP` covers import and package integration failures.
- `OWN` covers ownership and lifetime-style constraints.
- `RCV` covers recovery and resume failures.
- `MAT` covers match and exhaustiveness failures.
- Codes use the stable `HARN-<CAT>-<NNN>` format.

## Message rules

- Keep messages stable, specific, and actionable.
- Point at the smallest useful span.
- Do not blame the user; describe the invalid construct.
- Mention the expected shape before the found shape when useful.
- Include a short help string only when it reduces guesswork.
- Avoid diagnostics that require reading compiler internals.
- Prefer one primary error over many cascading errors.
- Preserve existing code identifiers unless the old code was wrong.
- Keep warnings clearly distinguishable from blocking errors.
- Update explanations when changing a diagnostic's meaning.

## Structured metadata

- Preserve `code`.
- Preserve `severity`.
- Preserve source spans.
- Preserve labels and notes.
- Preserve related spans.
- Preserve machine-applicable `FixEdit` values.
- Preserve repair ids and summaries.
- Preserve repair safety classes.
- Preserve structured diagnostic details used by LSP code actions.
- Avoid parsing human-readable messages in downstream tools.

## Repair safety

- Format-only edits can be auto-applied when they are byte-local and deterministic.
- Syntax-local repairs can be auto-applied only when they cannot change intent.
- Type or semantic repairs should usually be propose-only.
- Cross-file edits need higher review.
- Capability, network, shell, and mutation repairs need explicit approval.
- `harn fix --plan` should explain why each repair is or is not safe.
- `harn fix --apply` must respect the declared safety ceiling.
- Do not hide skipped or rejected repairs.
- Keep repairs idempotent.
- Add tests for both plan and apply surfaces when safety behavior changes.

## Cross-surface alignment

- CLI rendering should show the stable code.
- `harn explain` should resolve every registered code.
- LSP diagnostics should expose matching codes and repair metadata.
- JSON surfaces should keep codes machine-readable.
- Conformance `.error` files should lock user-visible failures.
- Docs should describe public diagnostics when users can act on them.
- Generated diagnostic catalogs must stay in sync with the registry.
- Do not hand-edit generated catalog artifacts.
- Use [[harn-testing]] for fixture and JSON report coverage.
- Use [[harn-tracing]] if diagnostics are stored in transcripts or receipts.

## Verify

- Parser/typechecker changes: `cargo test -p harn-parser <filter>`.
- Lint changes: `cargo test -p harn-lint <filter>`.
- CLI explain changes: `cargo test -p harn-cli --test <test-name>`.
- Repair changes: targeted `harn fix` tests and `make lint-test-patterns`.
- Intentional failure fixtures: `cargo run --quiet --bin harn -- test conformance --filter <name>`.
- Rendered lint smoke: `cargo run --quiet --bin harn -- lint <path>`.
- JSON diagnostics smoke: use the relevant `--json` command when available.
- LSP changes: targeted `cargo test -p harn-lsp <filter>`.
- Catalog drift: run the generator/check command documented by the touched catalog.
- Broad gate for shared diagnostic changes: `make test`.
