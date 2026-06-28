Added `agent_fanout(requests, options)` to `std/agent/workers` — a general parallel sub-agent fan-out primitive. It
maps a list of independent units onto concurrent background `sub_agent_run` children in bounded waves (`max_parallel`,
default 8), joins them, and returns one normalized `{label, index, status, ok, result, error}` per request in input
order. Composes the existing worker primitives (no new host surface); the caller owns each child's tool surface,
capability policy, model, and prompt via per-request `options`. Two integration tests lock the contract:
`worker_overlap` proves the children's LLM turns overlap in wall-clock time (A/B serial-vs-concurrent), and
`agent_fanout` proves order/label preservation, per-child isolation, ok/error normalization, and wave chunking. See
`docs/src/agent-pools.md` (“Parallel sub-agent fan-out”).
