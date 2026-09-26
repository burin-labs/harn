# Run the decision probes offline

From the repository root:

```sh
harn run examples/decision-probes/probe.harn --evaluation-tape examples/decision-probes/probe.tape
```

No credentials are needed. The 53-line script batches tool safety, message
compaction, skill selection, title selection, and completion questions over one
shared state. A second evaluation demonstrates the state ceiling refusal.

The checked-in tape was recorded through the native Vercel adapter against a
local deterministic HTTP fixture. Its served model is explicitly
`local-fixture-not-a-model-observation`. The answers and original token counts
are synthetic protocol fixtures, not evidence of model quality or actual spend.
Replay receipts retain those original facts under `reused_from` and report zero
new provider requests and charges.

Keep `probe.tape.cas` with the tape. Removing a record makes the command fail;
replay never fills a missing record with a provider request. The CLI end-to-end
test checks both the complete tape and that deletion control without API keys.

To try a real provider, choose a different, nonexistent tape path and configure
the requested provider credential. An existing tape always selects replay.
State, question, policy, route, and evaluator contract changes require a new
recording; old records are not silently reinterpreted.
