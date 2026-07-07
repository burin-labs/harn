# `std/cli/render`

Output helpers for `.harn` CLI subcommand scripts dispatched via the
harn-cli wedge ([harn#2293] epic, [harn#2296]). This module is
intentionally a thin layer — most port-facing rendering primitives
already exist in the stdlib:

| Need | Use |
| --- | --- |
| Color + tty detection | `std/ansi`: `ansi_color`, `ansi_bold`, `ansi_strip`, `ansi_enabled`. Honors `NO_COLOR` and `HARN_COLOR`. |
| tty test | `std/io`: `is_tty(fd)`. |
| Tables | `std/table`: `render_table`, `render_markdown_table`, `render_kv_table`. Column auto-width + alignment + per-cell truncation. |
| Diff rendering | `std/diff` (Myers). |
| Read stdin / passwords | `std/io`: `read_line`, `read_password`. |

What `std/cli/render` adds on top:

- `envelope(spec) -> dict` — JSON envelope wrapper with pinned
  top-level key ordering (`schemaVersion`, `apiStability`, optional
  `warnings`, then `payload`). Snapshot-test friendly.
- `write_envelope(env)` — serialize and write to stdout, pretty when
  stdout is a tty, compact otherwise.
- `mode()` / `json_mode()` — read the dispatch wedge's
  `HARN_OUTPUT_JSON` env to decide between human and JSON output
  without re-parsing `--json`.

## Surface

```harn
import { envelope, write_envelope, json_mode, mode } from "std/cli/render"
import { ansi_bold, ansi_color } from "std/ansi"
import { render_table } from "std/table"

fn main(harness: Harness) {
  const result = {ok: true, items: [{provider: "anthropic", model: "claude-opus-4-7"}]}
  if json_mode() {
    write_envelope(envelope({
      schema_version: 1,
      api_stability: "stable",
      payload: result,
    }))
    return
  }
  // Human mode.
  __io_println(ansi_bold("Result", {}))
  __io_println(render_table(result.items, {
    headers: ["Provider", "Model"],
  }))
}
```

## Envelope contract

The `envelope` helper enforces a stable top-level key order so JSON
snapshot tests stay robust as new fields are added at the bottom:

1. `schemaVersion` — int, set by the caller
2. `apiStability` — `"stable"` | `"experimental"` | `"internal"`
3. `warnings` — list of strings; omitted when empty
4. `payload` — the actual result body

Consumers can switch on `schemaVersion` + `apiStability` without
parsing prose. Future additions go between `apiStability` and
`payload` so they never displace existing keys.

[harn#2293]: https://github.com/burin-labs/harn/issues/2293
[harn#2296]: https://github.com/burin-labs/harn/issues/2296
