# Run the decision study tools

Use these tools to prepare a comparison, resume its recorded trials, or analyze
an existing result directory. The dated reports in this directory describe the
original studies. A new run does not replace their evidence.

From the repository root, set `BINARY` to the absolute path of the Harn executable
that will run the trials. The outer driver also needs Harn with `std/config.parse_env`.
Prepare the default corpus without reading credentials or making model calls:

```bash
harn run --no-sandbox experiments/missing-tool-call-decision/run-cutover.harn -- \
  "$BINARY" "$PWD" "$PWD/.harn-runs/decision-study" 1 --prepare-only
```

The default baseline comes from `cutover-v6-manifest.json`. To select a different
baseline or corpus, append `BASELINE_REF CORPUS_FILENAME` after the repetition count.
Corpus files must live in this study directory.

To execute, repeat the command without `--prepare-only`. The driver reads only
the OpenAI and OpenRouter keys from `~/gate-clone/.env`, as literal values.
It passes them to confined trial processes without printing or storing them.
The trusted outer driver uses `--no-sandbox` to read the checkout and credentials;
each trial keeps the normal Harn sandbox.

For a modern runtime, add `--migrate-baseline`. This runs the selected executable's
`harn fix` on a scratch copy of the old classifier, limited to behavior-preserving
repairs for removed methods. The manifest records the migrated source hash.
The old prompt and original source hashes stay in the manifest. Omit this flag
when reproducing an original study with its matching checkout and executable.

Resume with the same inputs and flags. The driver refuses changed study inputs,
invalid recorded cells, and trial directories without a completed result.
Inspect an interrupted trial before retrying it, since a missing result can hide
a paid request. Unknown costs reserve $0.05 each against the $1 study allowance.

Analyze a complete result directory:

```bash
harn run experiments/missing-tool-call-decision/analyze.harn -- \
  .harn-runs/decision-study
```

The analyzer requires one result for every row, arm, and repetition. It writes
`analysis.json`, `calibration-rows.json`, and `observations.jsonl`. Bootstrap
sampling keeps the original seed, row grouping, and interval calculation.
Missing cost and cache readings stay unavailable instead of becoming zero.

Run the agreement, cost, cache, and varied-row controls:

```bash
harn test scripts/tests/decision_study_tools_test.harn
```
