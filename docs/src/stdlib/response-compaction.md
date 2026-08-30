# Response compaction

`std/agent/response_compaction` retains a response exactly before optionally
projecting it to a smaller typed value. It is a generic mechanism. Callers own
the threshold, summary instructions, critical facts, and fallback value.

```harn
import {
  response_compact,
  response_compaction_read_exact,
} from "std/agent/response_compaction"

type Exact = {rows: list<dict>, total: int}
type Summary = {overview: string, critical_facts: list<string>}

const compacted = response_compact(
  {fs: harness.fs, llm: harness.llm},
  exact_response,
  schema_of(Exact),
  schema_of(Summary),
  {
    summarize_above_bytes: 32000,
    instructions: "Preserve identifiers, errors, and incomplete work.",
    fallback: {overview: "Exact response retained.", critical_facts: []},
    ladder: "agent_cheap",
    ttl_seconds: 86400,
    max_entries: 256,
  },
)

const exact = response_compaction_read_exact(
  {fs: harness.fs, llm: harness.llm},
  compacted.receipt.exact_ref,
  schema_of(Exact),
)
```

## Contract

- The exact typed value is written to an atomic filesystem cache below
  `harness.fs.workspace_temp_dir()`, then schema-validated and checked against
  its canonical JSON digest on readback before a model call can begin.
- `output.kind` is exactly `exact`, `summarized`, or `fallback`.
- Summaries use a named catalog ladder. The cache identity includes the exact
  digest, summary schema, instructions, ladder, and token bound.
- Only schema-valid summaries enter the cache. A malformed or failed summary
  returns the caller's typed fallback while `exact_ref` remains readable.
- `exact_ref` contains no caller-controlled path. Readback derives the owned
  workspace-temporary root. Malformed references and schema-valid content
  tampering both return `broken`.
- TTL and LRU limits bound both exact and summary storage. Expired or evicted
  exact values read as `missing`, never as a successful empty value.
- Receipts contain digests, byte counts, cache and route facts, and optional
  token usage, not response content. A measured token count of zero is kept;
  an unavailable measurement remains absent.

`response_compaction_read_exact` returns `{state: "found", value}`,
`{state: "missing"}`, or `{state: "broken", detail}`. Hosts that expose exact
responses should wrap this function in their existing tool or MCP surface;
they should not read the filesystem-cache layout directly.
