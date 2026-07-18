# Persona prompt compiler eval

This eval pack measures the complete prompt-to-persona path:

1. one bounded `persona_compile_prompt` checkpoint;
2. closed blueprint and fixed `suggest` / required-receipt lowering;
3. canonical strict package materialization; and
4. synthetic dispatch of the generated `persona://` binding through the
   orchestrator.

The twelve frozen cases are split evenly between `meter-tune` and
`meter-holdout`. Each trial writes a durable JSON receipt under `receipts/`,
and the standard Harn eval ledger records pass/fail, cost, wall time,
reliability, fingerprints, and the first typed failure bucket. Generated
packages and receipts are ignored runtime output, not source fixtures.

## Run

The Harn entrypoint defaults to a deterministic mock blueprint for a zero-spend
compile, materialize, and dispatch smoke. Mock results prove harness and product
plumbing only; they are never prompt-quality evidence. A live provider requires
an explicit spend confirmation and a cell ceiling:

```bash
HARN_PERSONA_EVAL_PROVIDER=openrouter \
HARN_PERSONA_EVAL_MODEL=<model> \
HARN_PERSONA_EVAL_SPLIT=meter-tune \
HARN_PERSONA_EVAL_TRIALS=5 \
HARN_PERSONA_EVAL_CONFIRM_SPEND=1 \
HARN_PERSONA_EVAL_MAX_CELLS=30 \
HARN_EVENT_LOG_BACKEND=sqlite \
HARN_EVENT_LOG_SQLITE_PATH="$HOME/.harn/persona-prompt-evals.sqlite" \
harn run run.harn
```

The runner is serial. It never repairs or retries model output, and the
compiler caps every response at 512 tokens unless its owning product contract
changes. `HARN_PERSONA_EVAL_MAX_CELLS` must cover the selected cases times the
requested trials, which prevents an accidental full-suite live run.
Paid runs also require an absolute shared SQLite event-log path. Use the same
path for both source revisions so their durable rows are actually pairable;
worktree-local default ledgers are deliberately rejected for live evidence.

## Compare

Grounding ablations use two source revisions of the compiler with this exact
eval directory unchanged. This keeps case and harness fingerprints compatible
without exposing an eval-only grounding option in the production API. Run the
baseline and treatment with distinct commits, then render the paired gate:

```bash
HARN_PERSONA_EVAL_BASELINE_COMMIT=<baseline-sha> \
HARN_PERSONA_EVAL_TREATMENT_COMMIT=<treatment-sha> \
HARN_PERSONA_EVAL_PROVIDER=<provider> \
HARN_PERSONA_EVAL_MODEL=<model> \
HARN_PERSONA_EVAL_SPLIT=meter-holdout \
HARN_EVENT_LOG_BACKEND=sqlite \
HARN_EVENT_LOG_SQLITE_PATH="$HOME/.harn/persona-prompt-evals.sqlite" \
harn run report.harn
```

The report requires at least five decided trials (PASS or FAIL) for every case,
exact case and harness fingerprints from the current frozen manifest, and
complete paired coverage. Skips remain reported but never buy statistical
power. Mixed harness generations and stale or foreign cohort identities under
one commit/model/split key fail the gate instead of being pooled. It reports
macro pass@1,
all-pass/flaky/all-fail reliability, skip and timeout rates, worst source
group, mean wall time, total cost, cost per solved case, valid packages per
dollar, failure buckets, paired 95% bootstrap CI, realized CI half-width, and
the baseline-minus-one-sigma regression gate.

Exit status is zero only for a confirmed improvement whose paired 95% CI lower
bound is greater than zero and which passes the regression gate. An
inconclusive full-quickref treatment therefore does not replace the compact
grounding prompt.
