# Completion claim exit eval

This pack compares the existing tool-free exit with the optional typed
`task_complete` exit on matched tasks. Each frozen split contains two task
pairs, and every pair runs a baseline cell and a treatment cell for at least
five trials.

The independent verifier requires the agent to call the inspection tool,
report its exact evidence token, finish successfully, and avoid the completion
judge cap. A treatment cell must finish through a valid explicit claim without
falling back to the tool-free exit; a baseline cell must not report an explicit
exit. Trial receipts additionally record explicit-versus-fallback exit,
provider-call count, known cost, unpriced calls, unknown-usage calls, and the
output-token ceiling. A zero cost is therefore never presented without the
accounting-status fields that say whether it was actually measured.

For a live tune run:

```bash
HARN_EXT_COMPLETION_CLAIM_EVAL_PROVIDER=openrouter \
HARN_EXT_COMPLETION_CLAIM_EVAL_MODEL=<model> \
HARN_EXT_COMPLETION_CLAIM_EVAL_SPLIT=meter-tune \
HARN_EXT_COMPLETION_CLAIM_EVAL_TRIALS=5 \
HARN_EXT_COMPLETION_CLAIM_EVAL_CONFIRM_SPEND=1 \
HARN_EXT_COMPLETION_CLAIM_EVAL_MAX_CELLS=20 \
HARN_EVENT_LOG_BACKEND=sqlite \
HARN_EVENT_LOG_SQLITE_PATH="$HOME/.harn/completion-claim-evals.sqlite" \
harn run run.harn
```

Run `meter-holdout` only after the tune result and implementation are frozen.
Do not pool the two splits or treat mock-provider results as model evidence.
