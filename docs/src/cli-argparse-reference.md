# `std/cli/argparse`

Declarative argument parser for `.harn` CLI subcommand scripts dispatched
via the harn-cli wedge ([harn#2293] epic, [harn#2295]). Each ported
subcommand declares a parser spec and calls `parse(spec, argv)` against
the global `argv` the dispatch wedge installs.

The library is pure `.harn` — no new Rust builtins — and intentionally
covers only the subset of flag parsing that today's ported commands
need:

- Positional arguments, required by default.
- Long flags (`--model`, `--model=val`, `--model val`) and short flags
  (`-m val`, `-mval`).
- Boolean switches (`--json`).
- Repeated flags collected into a list (`multi: true`).
- A `--` separator routing everything after it into `parsed.rest`.
- Structured error envelopes for the four kinds of parse failures
  callers can usefully react to.

Out of scope by design:

- Nested subcommand parsers — each subcommand is its own embedded
  script and top-level dispatch has already picked it.
- Shell completion generation — clap covers that for the top-level
  CLI; the per-subcommand scripts don't ship completions.
- Validators beyond "value required" — the caller decides whether a
  parsed value is a valid integer / path / model id / etc.

## Surface

```harn
import { parser, parse, render_help } from "std/cli/argparse"

let spec = parser({
  name:  "eval-prompt",
  about: "Render and run a .harn.prompt across a fleet of models.",
  args: [
    { name: "prompt",  kind: "positional", required: true,
      help: "Path to the .harn.prompt file." },
    { name: "model",   kind: "flag", short: "-m", long: "--model",
      multi: true, value_name: "ID",
      help: "Model id; repeat for multi-model fanout." },
    { name: "fixture", kind: "flag", long: "--fixture", value_name: "PATH",
      help: "Replay LLM responses from this JSONL fixture." },
    { name: "json",    kind: "switch", long: "--json",
      help: "Emit a JSON envelope instead of human output." },
  ],
  examples: [
    "eval-prompt prompts/summarize.harn.prompt",
    "eval-prompt -m claude-opus-4-7 -m claude-haiku-4-5 prompts/summarize.harn.prompt",
  ],
})

let result = parse(spec, argv)
let err = result.err
if err != nil {
  __io_eprintln(render_help(spec))
  __io_eprintln("error: " + (err.hint ?? ""))
  exit(2)
}
let parsed = result.ok
// parsed.prompt: string, parsed.model: list<string>, parsed.fixture: string?,
// parsed.json: bool, parsed.rest: list<string>
```

## Argument kinds

| Kind | Required by default | Notes |
| --- | --- | --- |
| `positional` | Yes (override with `required: false`) | Set `variadic: true` to greedily collect the rest. |
| `flag` | No | Takes a value. `multi: true` collects repeats into a list; default value becomes `[]` when unspecified. |
| `switch` | No | Boolean; presence sets `true`. Default value becomes `false` when unspecified. |

## Error envelopes

`parse` returns either `{ ok: dict }` or `{ err: ParseError }`. Error
`kind` is one of:

| `kind` | When |
| --- | --- |
| `missing_required` | A required positional or flag was absent. |
| `unknown_flag` | A `--foo` or `-f` not registered in the spec. |
| `unknown_arg` | An extra positional beyond what's registered. |
| `value_required` | A flag was the last argv entry with no value. |
| `bad_value` | A switch received a value (`--json=true`). |

Each error carries the offending `arg` (the original argv string) and a
short `hint` suitable for one-line stderr output.

## `--help` rendering

`render_help(spec)` returns a stable, snapshot-tested layout:

```text
{about, if present}

USAGE:
  {name} [OPTIONS] <positional>...

ARGS:
  <positional>                  help text

OPTIONS:
  -s, --long <VALUE>            help text
      --switch                  help text
  -h, --help                    Print help

EXAMPLES:
  {example 1}
  {example 2}
```

The layout is intentionally clap-flavored without trying to match
byte-for-byte. Snapshot tests under `conformance/tests/cli/` pin both
the structure and the column alignment.

[harn#2293]: https://github.com/burin-labs/harn/issues/2293
[harn#2295]: https://github.com/burin-labs/harn/issues/2295
