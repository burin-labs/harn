# HARN-SUS-012 — replay drain decision prompt hash diverges from journaled receipt

The replay runtime tried to memoize a settlement-agent drain decision
against a [`DrainDecisionReceipt`] whose `prompt_hash` does not match the
candidate prompt the second run computed. Drain decisions are journaled
so that replay can skip the settlement agent's LLM call — but if the
prompt drifts, the recorded `action` no longer reflects what the agent
would actually decide.

Common causes:

- The pipeline's settlement-agent prompt template or its inputs (the
  unsettled-state snapshot, the budget summary, the worker list) changed
  between record and replay. Either re-record the trace or roll the
  template back to its recorded form.
- The drain item set itself changed shape (eg. a new suspended subagent
  appeared on replay). Fix the upstream determinism gap first, then the
  drain receipt will line up.

The recorded receipt is still safe to inspect via
`lifecycle_receipts_snapshot()`, and its signed timestamp tells you
when the original decision was minted. Treat the mismatch as a signal
that the replay is no longer a determinism check.
