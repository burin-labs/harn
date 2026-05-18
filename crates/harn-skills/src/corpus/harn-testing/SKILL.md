---
name: harn-testing
short: Conformance tests, evals, deterministic fixtures, and test authoring.
description: Use for Harn conformance tests, eval harnesses, deterministic fixtures, and language/runtime test coverage.
when_to_use: Use when adding or reviewing conformance cases, eval fixtures, mock providers, replay fixtures, or deterministic tests.
---

# Harn testing

Use this skill when adding or reviewing tests for Harn language, runtime, providers, orchestration, replay, or eval behavior.

Pair it with [[harn-language]] for syntax contracts and [[harn-tracing]] for replay or transcript assertions.

## Start here

- `conformance/tests/` is the executable spec for user-visible behavior.
- `docs/src/dev/testing.md` lists deterministic test patterns and banned flaky patterns.
- `docs/llm/harn-quickref.md` covers `.harn` syntax used in fixtures.
- Use the narrowest package target that protects the changed contract.
- Expand only when behavior crosses crates or user-facing CLI paths.
- Prefer deterministic mocks over live services.
- Keep fixture names descriptive and stable.
- Put failure cases next to nearby passing cases.

## Conformance fixtures

- Passing `.harn` files normally pair with `.expected`.
- Intentional failures pair with `.error`.
- `@xfail` marks an expected failing case.
- In JSON conformance reports, expected failing `@xfail` cases are `xfail_expected`.
- In JSON conformance reports, passing `@xfail` cases are `xfail_unexpected_pass`.
- Unexpected xfail passes should fail the suite.
- `snapshotKey` identifies the selected fixture set and relevant sidecars.
- Keep fixture sidecars checked in with the case they describe.
- Use mock LLM tapes for provider-sensitive behavior.
- Use process tapes for deterministic host-process behavior.
- Keep `.expected` output minimal and user-visible.
- Keep `.error` output stable around diagnostic codes and messages.

## Determinism rules

- Do not add `std::thread::sleep` to tests.
- Do not add `tokio::time::sleep` to tests.
- Do not add wall-clock polling loops.
- Do not use `SystemTime::now()` in tests.
- Do not use short `recv_timeout` waits.
- Prefer `tokio::time::pause()` and `advance()`.
- Prefer `mock_time` or paused clocks for time-sensitive behavior.
- Prefer `EventLog::subscribe()` over sleep-and-check loops.
- Prefer `OrchestratorHarness` for orchestration state machines.
- Use replay fixtures when output order is part of the contract.
- Keep random behavior seeded or mocked.
- Assert on structured data before rendered prose when possible.

## Layer choice

- Parser syntax belongs in `harn-parser` tests or conformance.
- Formatter behavior belongs in `harn-fmt` tests plus focused CLI smoke when needed.
- Lint rules belong in `harn-lint` tests plus CLI coverage for flags.
- Runtime stdlib behavior belongs in `harn-vm` tests or conformance.
- CLI JSON contracts belong in `crates/harn-cli/tests/`.
- Provider behavior should use mock providers unless live reachability is the point.
- Portal changes need portal lint, tests, and build.
- Tree-sitter changes need grammar tests and fixture updates.
- Docs snippets need `make check-docs-snippets`.
- Keep broad `make test` for higher-risk shared behavior.

## Evals and replay

- Eval fixtures should isolate model variance from runtime determinism.
- Store model-independent expectations whenever possible.
- Use transcripts and receipts as replay contracts.
- Prefer structured assertions over scoring prose.
- Keep Langfuse or OTel exports out of unit tests unless mocked.
- Replay should not require credentials.
- Avoid tests that depend on external network timing.
- Keep failure output short enough for CI logs.
- Add regression tests for every fixed flake.
- Use [[harn-tracing]] for receipt, transcript, and replay details.

## Review checklist

- Does the test fail before the fix?
- Does the test protect the user-visible contract?
- Is the fixture name specific?
- Is the test deterministic on macOS, Linux, and Windows?
- Does it avoid sleeps and wall-clock time?
- Does it avoid live credentials?
- Does it keep generated files out of source control?
- Does it use existing helpers instead of custom harness code?
- Does it keep assertions stable across unrelated formatting changes?
- Does it include both success and failure coverage when the behavior has both?

## Verify

- General workspace tests: `make test`.
- Narrow Rust package: `cargo test -p <package> <filter>`.
- Conformance suite: `cargo run --quiet --bin harn -- test conformance`.
- Conformance JSON: `cargo run --quiet --bin harn -- test conformance --json`.
- Targeted conformance: `cargo run --quiet --bin harn -- test conformance --filter <name>`.
- Flake guard: `make lint-test-patterns`.
- Harn fixture lint: `make lint-harn`.
- Harn fixture format: `make fmt-harn`.
- Docs snippets: `make check-docs-snippets`.
- Portal changes: `npm run portal:lint`, `npm run portal:test`, and `npm run portal:build`.
