# `std/cli/envelope`

Fail-closed decoders for public harn-cli JSON envelopes published through
`harn --json-schemas`. Emission helpers stay in [`std/cli/render`](./cli-render-reference.md);
this module owns consume/decode.

## Lint envelope (schema-v1)

```harn
import { decode_lint_json, decode_lint_envelope } from "std/cli/envelope"

fn main(harness: Harness) {
  const text = harness.stdio.read()
  const decoded = decode_lint_json(text, {exit_status: 0})
  if is_err(decoded) {
    harness.stdio.eprintln(unwrap_err(decoded).message)
    return
  }
  const envelope = unwrap(decoded)
  for file in envelope.data.files {
    harness.stdio.println(file.path + ": " + file.status)
  }
}
```

| Helper | Role |
| --- | --- |
| `lint_envelope_schema()` | Native `std/schema` contract for the envelope |
| `decode_lint_json(text, options?)` | Parse JSON text, then fail closed |
| `decode_lint_envelope(value, options?)` | Decode an already-parsed value |

`options.exit_status`, when set, requires `(exit_status == 0) == envelope.ok`.
`options.expected_schema_version` defaults to `1`.

### Failure kinds

| `kind` | Meaning |
| --- | --- |
| `json_parse` | Malformed JSON text |
| `schema` | Structural mismatch against the envelope schema |
| `unsupported_schema_version` | `schemaVersion` is not the expected lint version |
| `invalid_severity` | Diagnostic severity outside `info` / `warning` / `error` |
| `invalid_span` | Byte span with `start > end`, or invalid `added_lines` |
| `inconsistent_aggregate` | `summary` counters disagree with per-file totals |
| `inconsistent_status` | File `status` disagrees with its diagnostics |
| `exit_status_mismatch` | Process exit status disagrees with `ok` |
| `envelope_invariant` | `ok` / `error` / `data` combination is impossible |

### Byte spans

Lint diagnostic `span` values are UTF-8 half-open byte offsets
`[start, end)` into the source file. See
[CLI `--json` contract](./cli-json-contract.md#harn-lint---json).

Discover the live schema with:

```bash
harn --json-schemas --command lint
```
