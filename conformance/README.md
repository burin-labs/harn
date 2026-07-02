# Conformance tests

This directory is Harn's primary behavioral test suite. Each test is a
`.harn` program with a sibling baseline file the runner diffs against;
together they define the contract the Harn language, stdlib, runtime, and
orchestration substrate are expected to honor.

## Why "conformance"?

The label is load-bearing. These tests are written as if a second Harn
implementation might one day need to pass them — they encode the language
contract, not internal implementation details. Two consequences flow from
that framing:

1. **Tests treat the Harn CLI as a black box.** Inputs are `.harn` source
   plus optional sidecar files (`*.llm-mock.jsonl`, `*.process-tape.json`,
   `*.fs-overlay/`); outputs are `stdout`, exit code, and diagnostic text.
   Internals like VM bytecode shape, parser tree layout, or scheduler
   tick ordering are deliberately out of scope.
2. **Adding or changing a test is a spec edit.** Bug-fix changes that
   alter conformance baselines need a written justification in the PR
   description (what behavior changed, what spec section is affected if
   any). Pure refactors should not touch `.expected` / `.error` files.

This is the same shape as ECMAScript Test262, the WebAssembly spec
testsuite, and TypeScript's `tests/cases/conformance/` directory — even
though Harn has a single implementation today, the suite is structured so
that constraint can relax later without rewriting the tests.

For fast, implementation-coupled checks (parser AST shape, VM dispatch
internals, type checker invariants), see Rust unit tests under
`crates/*/src/**/tests.rs` and Rust integration tests under
`crates/*/tests/**/*.rs`. The three layers cover different surfaces and
intentionally overlap only at the public API boundary.

## Layout

```text
conformance/
├── tests/               — behavioral tests by feature
├── errors/              — behavioral error tests by error class
├── fixtures/            — shared test data (connectors, graphql, triggers)
├── protocols/           — protocol contract schemas + fixtures
├── helpers/             — Python utilities used by the runner
├── replay-oracle/       — determinism replay harness
└── tool-call-eval/      — LLM tool-call evaluation datasets
```

### `tests/` — behavioral tests by feature

The runner walks `tests/` recursively. Top-level subdirectories group
tests by the feature surface they exercise:

| Category              | What it covers                                        |
|-----------------------|-------------------------------------------------------|
| `language/`           | Pattern matching, control flow, `const_eval`, syntax  |
| `types/`              | Type narrowing, destructuring, unions, generics       |
| `collections/`        | Array / dict / set operations                         |
| `concurrency/`        | Parallel pipelines, channels, supervisors, combinators|
| `control_flow/`       | Loops, returns, deferred execution                    |
| `functions/`          | Closures, higher-order calls, arity                   |
| `modules/`            | `import`, module resolution, package boundaries       |
| `runtime/`            | VM evaluation, scoping, recursion                     |
| `optimizer/`          | Optimizer pass invariants                             |
| `strings/`            | String formatting + builtins                          |
| `agents/`             | Agent loop, autonomy, resumption, pipeline lifecycle  |
| `autonomy/`           | Autonomy-budget surface specifically                  |
| `hooks_session/`      | Session lifecycle hooks, reminder hooks               |
| `llm_routing/`        | Provider routing, retry, circuit breakers             |
| `personas/`           | Persona resolution + composition                      |
| `skills/`             | Skill loading, signing, fs substitution               |
| `reminders/`          | Reminder injection, lifecycle, propagation, render    |
| `stdlib/`             | Built-in stdlib API surface                           |
| `templates/`          | Prompt template engine                                |
| `triggers/`           | Webhook + channel triggers, dispatch                  |
| `pool/`               | Pipeline pool primitives                              |
| `pool_durability/`    | Pool durability across restarts                       |
| `pool_otel/`          | OTEL emission from pools                              |
| `observability/`      | Tracing, metrics, log fields                          |
| `cli/`                | CLI flag + envelope contracts                         |
| `fmt/`                | `harn fmt` formatter idempotence                      |
| `harn_pack/`          | `harn pack` signing, manifests, asset bundling        |
| `harness/`            | Test harness self-checks                              |
| `compression/`        | Context compression policies                          |
| `net_policy/`         | Network capability sandbox                            |
| `sandbox_hardened/`   | Tightened sandbox profile                             |
| `trust_graph/`        | Trust graph evaluation                                |
| `testbench/`          | Testbench (process tape, fs overlay) self-tests       |
| `scenarios/`          | Cross-feature compositions (agent_loop × providers ×  |
|                       | mcp × skills × tool middleware × …)                   |
| `errors_by_feature/`  | Error tests grouped by the feature that produces them |

High-volume categories may have a second level. `stdlib/` currently splits
into `stdlib/oauth/`, `stdlib/json/`, `stdlib/hitl/`,
`stdlib/preset_hooks/`, `stdlib/tool_hooks/`, and `stdlib/project/`.
Other categories may grow similar subdirectories when their flat file
count crosses ~50 tests.

### `errors/` — behavioral error tests by error class

A parallel test home for negative-path tests grouped by **where** in the
compilation pipeline the error fires:

- `errors/syntax/` — parse failures
- `errors/types/` — type checker rejections
- `errors/semantic/` — name resolution, capability, attribute, and other
  pre-execution semantic errors
- `errors/runtime/` — exceptions raised during execution

Use `errors/` when the error is best understood as a class (e.g. "any
type mismatch in a `let` binding"). Use `tests/errors_by_feature/` when
the test demonstrates how a specific feature signals failure (e.g.
`agent_loop_done_sentinel_empty_string`).

Both homes share the same `.harn` + `.error` (or `.expected`) sibling-file
convention and are walked by the same runner.

## Sibling files

A single test can be made up of several files in the same directory,
sharing a stem:

| Suffix                 | Role                                                |
|------------------------|-----------------------------------------------------|
| `.harn`                | The test source (required)                          |
| `.expected`            | Exact stdout match (test exits 0)                   |
| `.error`               | Substring match against the error / non-zero exit   |
| `.lint`                | Substring match against `harn lint` diagnostics     |
| `.llm-mock.jsonl`      | Recorded LLM responses for replay                   |
| `.process-tape.json`   | Subprocess output tape for the testbench shim       |
| `.fs-overlay/`         | Files materialized into a tempdir before the test   |
| `.harness.json`        | Per-test harness sidecar config                     |

A test must have either `.expected` or `.error` (not both).

## Common helpers

`tests/_common.harn` exposes helpers shared across categories (CLI binary
resolution, log-wait predicates, etc.). Import it from a direct
subdirectory with `import "../_common"`, or from a second-level
subdirectory with `import "../../_common"`.

The `CONFORMANCE_HELPER_ALLOWLIST` in `scripts/lint_test_patterns.harn`
keeps copy-pasted variants of these helpers out of the suite — adding
your own `wait_for_listener_url` is a lint failure.

## Running

```bash
# Full suite
make conformance

# Or, equivalently:
HARN_LLM_CALLS_DISABLED=1 cargo run --bin harn -- test conformance

# A single category (positional selection — file or directory)
cargo run --bin harn -- test conformance conformance/tests/stdlib/oauth

# By substring filter
cargo run --bin harn -- test conformance --filter oauth_device_flow

# By regex
cargo run --bin harn -- test conformance --filter 're:^scenarios/.*hot_reload'

# Structured JSON output
cargo run --bin harn -- test conformance --json
```

The runner is fully directory-walk-based — no category name is hard-coded
anywhere in Rust source. New top-level categories or second-level subdirs
work as soon as you `git mv` files into them.

## Expected-output discipline

- **`make conformance` must be green on every commit.** No `#[ignore]`,
  no deletion-as-suppression.
- Deferred-but-required failures use an inline marker:

  ```harn
  // @xfail: short reason — tracked in #NNNN
  ```

  The runner expects failure; an unexpected pass is itself a CI failure
  (treat it as "ratchet me down"). The marker is closer to pytest's
  `@pytest.mark.xfail(strict=True)` and lit's `XFAIL:` than to Rust's
  `#[ignore]` — it stays visible, surfaces when the underlying issue is
  fixed, and is bounded by a CI ratchet.
- `scripts/check_xfail_count.harn` enforces a hard cap (currently `0` in
  `scripts/xfail_threshold.txt`). Adding an `@xfail` requires either
  raising the threshold (with reviewer justification) or fixing an
  existing one in the same PR.

## Demo gate

The conformance suite is paired with — but separate from — the
`harn demo` scenario gate (the `Demo gate` job in the `PR gates`
workflow). The gate is diff-driven: it auto-detects whether a PR adds a
public primitive (stdlib builtin, host capability, orchestrator surface,
language construct) and passes on its own otherwise. When it fires, the
PR must either register a `crates/harn-cli/assets/demo/<id>/` scenario or
carry the `no-demo-needed` label — don't add the label preemptively.
Conformance proves the primitive behaves; the demo proves the primitive
is reachable from a realistic user workflow.
