# Record and replay a decision evaluation

Use an evaluation tape to reproduce an evaluation without credentials or a
provider connection. An existing tape is always replayed. A missing record in
an existing tape fails; it never triggers a live request.

Record a request with the normal provider credentials available:

```sh
harn llm evaluate --request request.json --tape probe.tape --json
```

Run the same command again to replay the recorded result. Keep the adjacent
`probe.tape.cas` directory with the tape: large request and response records
are stored there. Tapes contain the supplied state and model responses; choose
a storage location suitable for that data.

For a script, use `harn run script.harn --evaluation-tape probe.tape`.
The repository's `examples/decision-probes/probe.harn` exercises six probe
families in 53 lines. Run it with `--evaluation-tape
examples/decision-probes/probe.tape` for a credential-free, explicitly synthetic
protocol example.
The scope covers evaluations in child VMs as well as the entry script.
Every record must be consumed exactly once, in recorded order. A different
state, question rubric, policy, site, or evaluator contract fails verification.
Extra records also fail the run, including when the script makes no evaluations.
Catching a replay error inside a script does not turn the run into success.

Read the returned receipt's `source` before interpreting accounting.
`tape` and `cache` receipts report zero current provider requests and charges.
Their `reused_from` field retains the complete original receipt, including
unknown usage and the provider's served model identity. Original spend is
provenance, not spend incurred by replay.

`evaluation_id` identifies the stable normalized request. `invocation_id`
identifies this occurrence within its execution. New outcome receipt handles
use the invocation ID, so repeated requests remain distinguishable. Historical
receipts without that field keep their original stable handle.

To reuse complete identical answers within one execution, pass
`harn run script.harn --evaluation-cache`. This explicit cache stores at most
1024 complete answers and does not persist between runs. Cache and tape modes
are mutually exclusive. A cache miss uses the normal evaluator admission and
transport path; a tape miss never does.

Request binding does not authenticate a model's answer, establish calibrated
confidence, or turn recorded data into a live observation. Keep those claims
separate from reproducibility.
