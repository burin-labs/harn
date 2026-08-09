---
name: harn-testing
short: Deterministic, claim-driven Harn verification.
description: Test owning interfaces, canonical paths, liveness, replay, and stochastic quality at the right altitude.
when_to_use: Use for conformance, runtime, workflow, integration, product, replay, or evaluation tests.
---

# Harn testing

Use this skill to choose evidence that can falsify the claim being made.

Pair it with [[harn-probe]] for uncertain runtime facts,
[[harn-orchestration]] for workflow ownership, and [[harn-product-quality]] for
launch behavior.

## Start from the claim

- Write the claim in observable terms.
- Name a plausible falsifier.
- Identify the owning module and public interface.
- Choose the lowest test altitude that can falsify the claim.
- Add higher-altitude evidence when the claim crosses adapters or surfaces.
- Do not count tests as proof; map evidence to claims.
- Record what remains outside the evidence.

## Test altitude

- Pure language semantics: conformance fixture.
- Parser, type, lint, or format behavior: focused crate test plus conformance.
- Runtime module behavior: test through the public runtime interface.
- Workflow behavior: deterministic harness test.
- Adapter contract: production and in-memory adapters through the same seam.
- Product behavior: canonical path end to end.
- Liveness: progress, interruption, recovery, terminal state.
- Model quality: multiple trials with a calibrated evaluator.

## Determinism

- Use `mock_time`, paused Tokio time, and explicit advancement.
- Subscribe to events rather than polling.
- Use `OrchestratorHarness` for workflow state.
- Use deterministic provider and tool adapters.
- Seed randomness and record the seed.
- Avoid `sleep`, wall-clock deadlines, and short `recv_timeout`.
- Avoid network dependencies in deterministic suites.
- Assert on typed events and outcomes rather than incidental log order.

## Interface tests

- Tests and callers should cross the same interface.
- Assert observable outcomes, errors, receipts, and state transitions.
- Do not test through private seams merely because they are convenient.
- Replace shallow-module tests when a deeper owning interface supersedes them.
- Keep test setup smaller than the behavior under test.
- Use production-shaped configuration defaults.
- Verify closed error variants and diagnostic codes.
- Keep fixtures minimal and readable.

## Run Harn test suites

- Put module behavior behind a public Harn function and call that function
  from tests in the same process. Use a subprocess only when the CLI adapter,
  installed artifact, process isolation, or operating-system exit status is
  the behavior under test.
- Select related files in one invocation with repeatable `--test-path`
  arguments. Harn compiles the selected import graph once and schedules the
  combined cases from one queue.
- Use `--parallel` without `--jobs` by default. The runner sizes the worker
  pool from available CPU and memory. Reserve `--jobs` or `HARN_TEST_JOBS` for
  an operator-imposed limit on a shared host.
- Mark tests that start compilers, test runners, or multi-threaded child
  processes with `@heavy(threads: N)`. Mark tests that share mutable external
  state with `@serial(group: "name")`. Keep ordinary filesystem fixtures in
  per-test workspace temporary directories instead of serializing the suite.
- Use `--timeout` as an execution-only wall-clock safety rail. It starts after
  discovery, import-graph compilation, fixture setup, and resource queueing;
  it still detects blocked I/O and deadlocks, which a CPU-time limit would
  miss.
- Assert performance separately with `--max-test-ms` or `--max-execute-ms` in
  a host-isolated lane. Harn serializes those measurement budgets so scheduler
  contention cannot decide the result.

The repository reference `docs/src/testing.md` defines the exact timing
phases, cache contract, defaults, and machine-readable receipts.

## Conformance

- Put language/runtime specification behavior under `conformance/tests/`.
- Include positive and negative cases.
- Preserve diagnostic codes and useful spans.
- Update formatter, linter, tree-sitter, and editor fixtures for syntax changes.
- Run the narrow filtered case first.
- Pin a capability-policy root to `harness.fs.workspace_temp_dir()`, never to
  the system temporary directory. A checkout under `/tmp` sits inside the
  latter. The fixture then becomes in-scope, and a case asserting that an
  out-of-scope read is refused fails on that machine alone.
- Run `make conformance`, `make lint-harn`, and `make fmt-harn` for syntax work.
- Keep generated spec and grammar artifacts synchronized.
- Treat conformance as executable specification, not broad integration coverage.

## Workflow and lifecycle

- Assert queued, running, waiting, blocked, stopped, failed, and complete states.
- Verify progress follows observable work.
- Send stop, wait, stand-down, and pivot during active execution.
- Verify no stale work occurs after accepted control.
- Restart and resume from durable checkpoints.
- Exercise duplicate delivery and idempotency.
- Verify parent/child lineage and graceful handoff.
- Bound retries, concurrency, iterations, time, tokens, and cost.

## Canonical product path

- Install or package the production-shaped artifact.
- Start from documented defaults.
- Exercise the default workflow without hidden setup.
- Verify native approvals and concrete mutations through host adapters.
- Compare semantic state across CLI, TUI, IDE, headless, and cloud projections.
- Inject a representative failure and complete the documented recovery.
- Verify accessibility for interactive surfaces.
- Preserve receipts and support-ready diagnostics.

## Portable kernel claims

- Compile the same source through the canonical frontend; never test an
  adapter-owned parser or evaluator.
- Compare exact native and Wasm terminal values and exact invalid-program
  diagnostics over the same corpus.
- Exercise the browser adapter in a real dedicated worker. Node-only execution
  does not prove the browser path.
- Prove identical input produces identical artifact bytes, then reject version
  mismatch, corruption, trailing data, excessive size, excessive nesting, and
  unsupported semantic contracts.
- Trigger capability suspension through a registered, exactly granted method.
  Also prove denial, wrong request id, wrong snapshot key, changed grants,
  stale snapshots, and incompatible results fail deterministically.
- Compare snapshot/resume with the equivalent uninterrupted terminal result.
- Audit the built Wasm imports for ambient file, process, network, clock,
  randomness, model, thread, and shared-memory authority.
- For native parallelism, share only the immutable artifact across threads and
  assert isolated executions retain terminal parity. For browsers, use
  independent workers rather than assuming Wasm threads.
- Measure compile, decode, first dispatch, steady-state dispatch,
  initialization, and bundle size separately. Preserve machine, browser, build
  mode, sample count, receipt, and revision with the observation.
- Do not use timing thresholds as semantic assertions. Functional worker tests
  should signal completion structurally, without sleeps or wall-clock polling.

## Stochastic quality

- Define the population, metric, threshold, and maximum spend before running.
- Use multiple independent trials.
- Calibrate the grader on known accepted and rejected examples.
- Report a distribution and representative failures.
- Separate model, prompt, harness, and tool changes where possible.
- Store traces needed to reproduce or review failures.
- Re-evaluate after model or provider updates.
- Never generalize reliability from one successful transcript.

## Replay

- Record enough inputs and decisions to reproduce the run.
- Verify replay reaches the same deterministic decisions.
- Separate live provider output from replayed output.
- Preserve tool call ids, lifecycle events, and receipts.
- Test compaction and resume continuity.
- Detect stale snapshots and double resume.
- Verify redaction does not remove required audit structure.
- Compare at the owning interface, not presentation details.

## Failure injection

- Provider timeout and malformed response.
- Tool rejection, timeout, and partial success.
- Approval denial and expiration.
- Network disconnect and process restart.
- Duplicate trigger delivery.
- Stale checkpoint or incompatible version.
- Resource-budget exhaustion.
- User stop or pivot during consequential work.

## Verify the tests

- Run the narrowest test first.
- Prove the regression fails without the fix when practical.
- Run adjacent package or conformance coverage.
- Run repository drift checks against current source.
- Rebuild before binary-semantics checks.
- Run the broad gate before merge.
- Inspect the rebased final diff and status.
- Report evidence by claim, plus residual risk.
