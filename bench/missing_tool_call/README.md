# Measure missing-tool-call recovery

Use this driver to compare classifier versions on completing agent runs. It
feeds the same assistant text and declared tools to the real classifier,
then completes with the done sentinel. Each invocation refuses to produce a
measurement unless the run completed, the model classifier fired once, and the
consumer published one verdict. The planner is a deterministic caller; this
measures classifier overhead rather than planner quality or tool execution.

1. Configure credentials for the classifier's provider.
2. Write a classifier configuration to `config.json` in a temporary directory.
   For the baseline, use the original provider and model options. For the new
   classifier, use an explicit evaluation policy:

   ```json
   {
     "evaluation": {
       "backend": "native_decision",
       "provider": "openrouter",
       "model": "openrouter/typesafe/jev-1.13",
       "threshold": 0.65,
       "evaluation_cost_limit": 0.01,
       "run_cost_limit": 0.1
     }
   }
   ```

3. Run each corpus index twice with each executable on the same machine. Give
   every invocation its own record directory and profile path:

   ```sh
   HARN_RUN_DIR="$measurement_dir/records" "$harn_bin" run \
     bench/missing_tool_call/completing_run.harn \
     --read-only-root "$config_dir" \
     --profile-json "$measurement_dir/profile.json" -- \
     bench/missing_tool_call/corpus.json 0 "$config_dir/config.json"
   ```

4. Join output rows by corpus ID and trial. Read wall time from the profile,
   baseline usage from `raw.typed_checkpoint.usage`, and new usage from the
   persisted evaluation receipt matching `raw.evaluation.receipt`. Use its
   physical-attempt count to distinguish a zero-cost refusal from a provider
   request. Read cache tokens directly; repeated inputs do not prove reuse.
5. Create calibration rows for intent on every item and tool choice on positive
   items. Keep labels outside the request state. Run
   [`harn eval calibrate`](../../docs/src/eval-calibration-reference.md) for both
   backends and report the row counts, abstentions, confidence error, and
   threshold coverage alongside accuracy. Report cost once per evaluation,
   rather than charging it again for each question.

The driver applies the same diagnostic projection to both versions because
the old classifier passed its checkpoint diagnostic to an event schema that
rejects that field. Reproduce the unmodified baseline failure separately; the
normalized comparison does not prove that the old production run succeeded.
Pass `unprojected` as a fourth script argument to exercise that publication
failure and verify the new event projection on the same path.
This small, authored corpus cannot establish performance on arbitrary tools,
ambiguous intent, long transcripts, or different model revisions.

## Measured result, 2026-10-02

The sixteen texts were run twice per backend on the same Linux build server. Every counted run
completed, reached the real classifier, published its verdict, and made exactly
one physical classifier request. Labels never entered the evaluator's state.
The candidates include the two declared tools and the loop's built-in await tool.
The baseline classifier and prompt in release v0.10.153 match main at
`83f4889983bfc46104c74e9ff34dfeb3580b359f`. Candidate source is
`28d50683b40f46996859709947285df0317b2118`.

| Classifier | Correct intent | Correct recovery and tool | Ambiguous | Median classifier ms | Cost, 32 runs |
| --- | ---: | ---: | ---: | ---: | ---: |
| Baseline structured | 30/32 | 30/32 | 2 | 1,074.5 | $0.02098425 |
| Shared structured | 30/32 | 30/32 | 3 | 2,098 | $0.02738100 |
| Shared native | 32/32 | 32/32 | 1 | 189 | $0.00081707 |

Classifier duration uses the baseline's profiled structured-call builtin and
the candidates' persisted evaluation receipt. Total run medians were 1,423,
4,765.5, and 2,771.5 ms respectively. Those totals compare an optimized baseline
with a debug candidate and include CLI/import work; they do not establish an
end-to-end speed improvement. Native's measured classifier cost was lower;
the structured replacement's cost and measured duration increased.

Both structured routes reported zero cache-read tokens in all 32 calls. The
native endpoint did not report cache categories. Adapter-default zero counters
are not evidence of native cache reuse. Native and structured intent answers
agreed on 30/32 paired trials, and their action labels agreed on 26/32.

The baseline missed the immediate patch commitment in both repeats. The shared
structured classifier incorrectly treated advice to read the README as its own
tool intent in both repeats. Equal aggregate accuracy therefore does not prove
behavioral equivalence or certify the replacement's quality. Native's one
ambiguous verdict was a permission question and caused no false recovery.
The corpus has only sixteen distinct texts; repeats are not independent new
examples. These observations are development evidence, not deployment accuracy.

[`measurement.json`](measurement.json) preserves all 96 measured rows, costs,
attempt counts, confidence, action, timing, and served-model evidence. The
structured receipts report `gpt-5.4-mini-2026-03-17`; native reports
`typesafe/jev-1.13-20260917`. The baseline's served revision is unavailable.
[`calibration-rows.json`](calibration-rows.json) contains 32 intent labels and
16 positive tool-choice labels per candidate backend, charging cost only once
per evaluation. [`calibration-report.json`](calibration-report.json) is the
owning `calibration_report` result over those 96 rows. Intent confidence error
was 0.138126 for structured and 0.092813 for native; tool-choice error was
0.0125 and zero. At the existing 0.65 threshold, intent coverage was 29/32 and
31/32. Neither report can recommend a threshold at 5% target error because its
calibration split is too small. The policy threshold remains unchanged.
