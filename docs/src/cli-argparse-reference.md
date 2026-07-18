# `std/cli/argparse`

Declarative, schema-aware argument parsing for `.harn` CLI subcommand
scripts dispatched via the harn-cli wedge ([harn#2293] epic,
[harn#2295]). Each subcommand declares a parser spec and parses the
global `argv` installed by the dispatch wedge.

The library is pure `.harn` and covers:

- Required and optional positional arguments, including one variadic
  positional.
- Long flags (`--model`, `--model=val`, `--model val`) and short flags
  (`-m val`, `-mval`).
- Boolean switches (`--json`).
- Repeated flags collected with `multi: true`.
- Primitive `string`, `int`, `float`, `bool`, and `list` decoding.
- A bare `--` terminator that routes following tokens to a separate `rest`
  list.
- Optional schema validation and narrowing through `parse_typed<T>`.

Nested subcommand parsers and shell completion generation remain out of
scope. Top-level clap dispatch already owns those concerns.

## API

```harn
pub fn parser(spec: ParserSpec) -> ParserSpec

pub fn parse(
  spec: ParserSpec,
  argv: list<string>,
) -> Result<CliInvocation<dict>, CliParseFailure>

pub fn parse_typed<T>(
  spec: ParserSpec,
  argv: list<string>,
  schema: Schema<T>,
  apply_defaults: bool = false,
) -> Result<CliInvocation<T>, CliParseFailure>

pub fn render_help(spec: ParserSpec) -> string
```

`CliInvocation<T>` has two fields:

```harn
{options: T, rest: list<string>}
```

Declared arguments are under `.options`. When no flag value is pending, a
bare `--` stops argument parsing; later tokens are placed under `.rest` and
are never mixed into the option bag or passed to the schema. Required
argument checks still run.

Use the native `Result` helpers rather than inspecting an object envelope:

```harn
import { parse, parser, render_help } from "std/cli/argparse"

const spec = parser({
  name: "render",
  args: [{name: "input", kind: "positional"}],
})
const result = parse(spec, argv)
if is_err(result) {
  const failure = unwrap_err(result)
  __io_eprintln(render_help(spec))
  __io_eprintln("error: " + failure.message)
  exit(2)
}
const invocation = unwrap(result)
const input = invocation.options.input
const forwarded = invocation.rest
```

## Parser spec

`ParserSpec` is:

```harn
{
  name: string,
  about?: string,
  args: list<ArgSpec>,
  examples?: list<string>,
}
```

Each `ArgSpec` supports:

| Field | Meaning |
| --- | --- |
| `name` | Non-empty, unique key written to `.options`. |
| `kind` | `"positional"`, `"flag"`, or `"switch"`. |
| `short`, `long` | Flag/switch aliases such as `"-m"` and `"--model"`. At least one is required for flags and switches. |
| `required` | Positionals default to `true`; flags and switches default to `false`. |
| `multi` | For flags, collect repeated occurrences into one list. |
| `variadic` | For a positional, greedily collect remaining positional tokens. |
| `value_name` | Placeholder used by help output; defaults to the uppercased `name`. |
| `help` | One-line help text. |
| `parse` | `"string"` (default), `"int"`, `"float"`, `"bool"`, or `"list"`. |
| `separator` | Delimiter for `parse: "list"`; defaults to `","`. |
| `default` | Already-typed, non-`nil` value inserted unchanged when the argument is absent. |

A switch never consumes a value: its presence produces `true`, and its
implicit default is `false`. A `multi` flag implicitly defaults to `[]`.
Other omitted optional values have no implicit entry unless `default` is
declared.

Defaults are not strings waiting to be decoded. For example, an integer
flag uses `default: 4`, not `default: "4"`, and a list flag uses a list
value. Explicit defaults, the switch `false` default, and the `multi`
`[]` default are applied before the required-argument check.

## Primitive decoding

`parse` decodes argv strings at the argument boundary:

| Decoder | Accepted value and output |
| --- | --- |
| `string` | The original argv string. |
| `int` | A base-10 integer with an optional leading sign. |
| `float` | A signed or unsigned decimal, optionally with an exponent. |
| `bool` | Case-insensitive `true`, `yes`, `y`, `1`, `on`, or `false`, `no`, `n`, `0`, `off`, after trimming. |
| `list` | `list<string>` split on `separator`, or on `,` when omitted. |

Invalid `int`, `float`, or `bool` text returns an argv-stage
`invalid_value` failure. Switches may omit `parse`, declare
`parse: "string"`, or declare `parse: "bool"`; because they do not consume
values, all three forms still produce a boolean.

With both `multi: true` and `parse: "list"`, each occurrence is split and
the results are flattened into one list. For example:

```harn
{name: "tag", kind: "flag", long: "--tag", multi: true,
 parse: "list", separator: ":"}
```

`["--tag", "core:cli", "--tag=docs:tests"]` produces
`["core", "cli", "docs", "tests"]`, not a nested list.

## Typed parsing

`parse_typed<T>` first performs the same argv parsing and primitive
decoding as `parse`, then validates only the resulting `.options` against
the supplied `Schema<T>`. On success, `.options` is narrowed to `T` while
`.rest` remains `list<string>`.

```harn
import { parse_typed, parser } from "std/cli/argparse"

type RunOptions = {
  input: string,
  jobs: int,
  tags: list<string>,
  verbose: bool,
}

const spec = parser({
  name: "run",
  args: [
    {name: "input", kind: "positional"},
    {name: "jobs", kind: "flag", long: "--jobs", parse: "int", default: 1},
    {name: "tags", kind: "flag", long: "--tags", parse: "list", multi: true},
    {name: "verbose", kind: "switch", long: "--verbose", parse: "bool"},
  ],
})

const result = parse_typed(spec, argv, schema_of(RunOptions))
if is_err(result) {
  __io_eprintln(unwrap_err(result).message)
  exit(2)
}
const invocation = unwrap(result)
const jobs: int = invocation.options.jobs
```

The fourth argument controls schema-owned defaults. It defaults to
`false`, which checks without applying schema defaults. Pass `true` to
apply defaults declared by the schema. `ArgSpec.default` values are always
applied by argv parsing first and are already typed; this flag does not
control them.

## Failures

`parse` and `parse_typed` return `CliParseFailure` in `Result.Err`:

```harn
{
  kind: "cli_parse_failure",
  stage: "argv" | "schema",
  code: string,
  message: string,
  issues: list<{
    stage: "argv" | "schema",
    code: string,
    message: string,
    arg?: string,
    path?: string,
  }>,
}
```

The failure and every issue contain only JSON-safe data.

### Argv stage

An argv-stage failure has the same code at the top level and in its one
issue. The issue's `arg` identifies the argument name, flag, or token
associated with the failure.

| `code` | When |
| --- | --- |
| `missing_required` | A required positional or flag is absent after defaults are applied. |
| `unknown_flag` | A long or short flag is not registered in the spec. |
| `unknown_arg` | A positional appears after all declared positionals are filled. |
| `value_required` | A value-taking flag is the last argv entry. |
| `bad_value` | A switch is supplied with an inline value. |
| `invalid_value` | Primitive `int`, `float`, or `bool` decoding fails. |

### Schema stage

When argv parsing succeeds but schema validation fails, the top-level
failure has `stage: "schema"` and `code: "schema_mismatch"`. Each schema
report issue is projected into `issues` with `stage: "schema"`, its schema
validation code and message, and its path (for example, `jobs`). If the
schema report omits issues or paths, argparse supplies one issue and uses
`root` as the fallback path.

This stage distinction lets command code render one stable failure shape
without re-checking or re-validating `.options` at the call site.

## Static parser errors

`parser(spec)` checks static programmer-owned declarations and throws
immediately when a spec is invalid. These exceptions are not
`CliParseFailure` values. It rejects:

- Empty argument names, duplicate names, invalid kinds, and duplicate flag
  aliases.
- Positionals with `short`, `long`, or `multi`, and any positional after a
  variadic positional.
- Flags or switches without a `short` or `long` alias, or with
  `variadic: true`.
- Switches with `multi: true`, or with a decoder other than the default
  `string` or `bool`.
- Unknown primitive decoders.
- `separator` without `parse: "list"`, or an empty list separator.

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

The layout is clap-flavored without trying to match it byte-for-byte.
Snapshot tests under `conformance/tests/cli/` pin its structure and column
alignment.

[harn#2293]: https://github.com/burin-labs/harn/issues/2293
[harn#2295]: https://github.com/burin-labs/harn/issues/2295
