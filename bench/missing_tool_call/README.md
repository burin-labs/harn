# Measure missing-tool-call recovery

Use this driver to compare classifier versions on completing agent runs. It
feeds the same assistant text and two candidate tools to the real classifier,
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
