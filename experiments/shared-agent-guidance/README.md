# Shared operating guidance experiment

This experiment compares the completion and native-tool prompts before and
after inspection, authorization, and delegation guidance moved into one system
fragment. Both arms run the canonical `agent_loop` on the same frozen candidate
binary. The baseline overrides the two old templates and empties the new
fragment; the candidate uses its embedded defaults.

The repair cells cover transactional balances and idempotency, incremental UTF-8
JSON-line parsing with atomic recovery, and bounded asynchronous queues with
cancellation and close semantics. The Python behavior oracle measures 6, 8, and
8 cases respectively. Each oracle rejects both its broken starter and an empty
implementation, and accepts its reference implementation. Python 3.13 or newer
is required for the queue reference's standard-library shutdown operation.

`report.json` records five paired trials per cell with `openai:gpt-5.4-mini`.
The first two rounds calibrated the output budget and are retained separately.
The measured rounds are 2 through 6, selected before their outcomes were read.
Ledger and streaming cells use medium reasoning and a 4,500-token output ceiling;
the queue uses low reasoning and 9,000 tokens. Arm order alternates by round.

The candidate passed 15 of 15 repairs; the baseline passed 13 of 15. One baseline
streaming run exhausted its output budget, and another produced an incorrect
repair. System instructions fell from 3,048 to 2,497 characters, or 18.1%.
Mean first-call provider input fell from 955.7 to 850.2 tokens, or 11.0%.
These are measurements of this workload and route; fifteen pairs do not establish
a general quality improvement. Every candidate run reached file inspection,
file writing, and behavioral verification.

`suite.harn` accepts one JSON configuration path. The configuration has `binary`,
`python`, and `output` absolute paths; `cells` names the fixture directories;
`trials` lists the round numbers; and `live` selects live inference after
calibration. `live: false` runs only the positive, broken, and empty controls.
Live runs require `OPENAI_API_KEY` in the parent environment. Grader subprocesses
receive a replacement environment without provider credentials. The output
directory must be new.

The checked-in runner was also exercised with a separate paired ledger repair:
both arms reached and passed all six oracle cases. Earlier runner packaging
checks exposed working-directory and filesystem-grant errors; those checks are
retained locally and excluded from the measured quality cohort.

```sh
harn run --standalone --no-sandbox experiments/shared-agent-guidance/suite.harn -- /absolute/path/config.json
```

The checked-in report records accepted-turn costs. Across the measured runs and
calibration, those recorded costs totaled $1.93; failed provider attempts are
not fully represented in that number. Raw local receipts retain their available
usage and errors separately.
