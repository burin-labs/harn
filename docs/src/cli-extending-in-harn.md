# Extending the CLI in `.harn`

How to add or port a `harn` subcommand without writing Rust. Most CLI
work is a text/JSON transform, a catalog lookup, a directory walk, or a
process spawn — once a port lands, the Rust handler shrinks to a
~5-line dispatch shim and the actual behavior lives in an embedded
`.harn` script.

This is the cookbook side of the harn-cli self-host epic
([harn#2293]). The reference docs for the primitives the scripts
build on are [`std/cli/argparse`](./cli-argparse-reference.md),
[`std/cli/render`](./cli-render-reference.md),
[`std/cli/paths`](./cli-paths-reference.md), and the
[`harn --json` contract](./cli-json-contract.md).

## Architecture

`harn-cli` keeps top-level clap parsing in Rust — that's how `harn` gets
fast help text, completions, and subcommand dispatch. After parsing,
ported subcommands hand control to an embedded `.harn` script via the
**dispatch wedge** in [`crates/harn-cli/src/dispatch.rs`][dispatch].
The wedge looks the script up in the `STDLIB_CLI_SCRIPTS` table in
[`crates/harn-stdlib/src/lib.rs`][stdlib-table], writes the embedded
source to a temp file, and runs it through the same `harn run` code
path the user-facing CLI uses. Bytecode cache, harness install, skill
loader, store/metadata/checkpoint builtins all come along for free.

The script receives `argv: list<string>` (everything after the
subcommand name, the same global `harn run -- a b c` exposes) and
reads the `HARN_OUTPUT_JSON` env var to decide between human and JSON
output. Build-time constants and other host-provided context travel
through scoped env vars rather than new builtins, so the script's
contract stays small. Every ported handler keeps its legacy Rust path
behind `HARN_CLI_IMPL=rust` — the parity-snapshot harness flips that
switch to assert both implementations produce byte-identical output
([C1 ratchet, #2314]). The flag will go away when the LOC ratchet
flat-lines.

## Walkthrough — port a fictional `harn greet <name>`

End-to-end port of a brand-new subcommand. Total diff is about 80 lines
across six files. The canonical real-world reference is the
**`harn version` port in [#2328][w1-pr]** (W1) — it's the smallest
meaningful port in the tree.

### 1. Clap args struct

`crates/harn-cli/src/cli/greet.rs`:

```rust
use clap::Args;

#[derive(Debug, Args)]
pub(crate) struct GreetArgs {
    /// Who to greet.
    pub name: String,
    /// Emit a `JsonEnvelope` with `{ greeting }` instead of plain text.
    #[arg(long)]
    pub json: bool,
}
```

### 2. Wire into the `Command` enum

`crates/harn-cli/src/cli/mod.rs`:

```rust
mod greet;
// ...
pub(crate) use greet::GreetArgs;
// ...
#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    // ...
    /// Greet someone by name.
    Greet(GreetArgs),
    // ...
}
```

### 3. Dispatch shim

`crates/harn-cli/src/commands/greet.rs`:

```rust
//! `harn greet` dispatch shim. The Rust path stays behind
//! `HARN_CLI_IMPL=rust` until the C1 ratchet (#2314) tightens its
//! budget to zero.

use crate::cli::GreetArgs;
use crate::dispatch;
use crate::env_guard::ScopedEnvVar;

pub(crate) async fn run(args: GreetArgs) -> i32 {
    if std::env::var("HARN_CLI_IMPL").as_deref() == Ok("rust") {
        return run_rust(&args);
    }
    let _name = ScopedEnvVar::set("HARN_GREET_NAME", &args.name);
    let argv = if args.json {
        vec!["--json".to_string()]
    } else {
        Vec::new()
    };
    dispatch::dispatch_to_embedded_script("greet", argv, args.json).await
}

/// Legacy Rust implementation. Kept behind `HARN_CLI_IMPL=rust` for the
/// parity harness; removed when the C1 ratchet flat-lines the budget.
fn run_rust(args: &GreetArgs) -> i32 {
    if args.json {
        println!(r#"{{"schemaVersion":1,"ok":true,"data":{{"greeting":"hello {}"}},"error":null,"warnings":[]}}"#, args.name);
    } else {
        println!("hello {}", args.name);
    }
    0
}
```

Match the subcommand in the top-level dispatch (`lib.rs`):

```rust
Command::Greet(args) => {
    let exit = commands::greet::run(args).await;
    if exit != 0 {
        process::exit(exit);
    }
}
```

### 4. The `.harn` script

`crates/harn-stdlib/src/stdlib/cli/greet.harn`:

```harn
/**
 * `harn greet` ported to .harn. The name comes in via HARN_GREET_NAME
 * (set by the dispatch shim) so we don't have to re-parse it from argv.
 * JSON mode is gated by HARN_OUTPUT_JSON, the dispatch wedge's
 * standard signal.
 */
fn render_envelope(name: string) -> string {
  let env = {
    schemaVersion: 1,
    ok: true,
    data: {greeting: "hello " + name},
    error: nil,
    warnings: [],
  }
  return json_stringify_pretty(env)
}

fn main(harness: Harness) {
  let name = harness.env.get_or("HARN_GREET_NAME", "world")
  let json_mode = harness.env.get_or("HARN_OUTPUT_JSON", "0") == "1"
  if json_mode {
    harness.stdio.println(render_envelope(name))
  } else {
    harness.stdio.println("hello " + name)
  }
}
```

### 5. Register the script

`crates/harn-stdlib/src/lib.rs`, in `STDLIB_CLI_SCRIPTS`:

```rust
StdlibCliScript {
    name: "greet",
    source: include_str!("stdlib/cli/greet.harn"),
},
```

The `name` is the lookup key the dispatch wedge passes in step 3;
nested paths like `"eval/prompt"` are fine too (they're collapsed to
`eval-prompt-` in the temp file prefix so the OS doesn't care).

### 6. Parity test

`crates/harn-cli/tests/greet_dispatch.rs`, following the established
`*_dispatch.rs` pattern that `version_dispatch.rs`,
`scaffold_dispatch.rs`, and `eval_prompt_dispatch.rs` all use — drive
both implementations as subprocesses, flip `HARN_CLI_IMPL=rust` for the
baseline, and structurally compare:

```rust
#[test]
fn greet_matches_rust_baseline() {
    let harn = run_subprocess(&["greet", "kenneth"], &[]);
    let rust = run_subprocess(&["greet", "kenneth"], &[("HARN_CLI_IMPL", "rust")]);
    assert_eq!(harn.stdout, rust.stdout);
    assert_eq!(harn.exit_code, 0);
}

#[test]
fn greet_json_envelope_matches() {
    let harn = run_subprocess(&["greet", "kenneth", "--json"], &[]);
    let rust = run_subprocess(&["greet", "kenneth", "--json"], &[("HARN_CLI_IMPL", "rust")]);
    let h: serde_json::Value = serde_json::from_str(&harn.stdout).unwrap();
    let r: serde_json::Value = serde_json::from_str(&rust.stdout).unwrap();
    assert_eq!(h, r);
}
```

JSON envelopes get compared as parsed `serde_json::Value` rather than
byte-for-byte because Harn's `json_stringify_pretty` sorts dict keys
alphabetically while serde emits struct fields in declaration order.
Both serialize the same envelope.

### 7. Tighten the LOC budget

`scripts/ported_handlers.toml` ([C1 #2314][c1-ratchet]) carries one
`[[handler]]` block per ported subcommand with a `max_loc` budget.
Append a fresh entry for the new shim:

```toml
[[handler]]
path = "crates/harn-cli/src/commands/greet.rs"
max_loc = 45   # current+5 slack
```

The next port to land in this file shrinks the budget to its new
`current+5`. The ratchet itself is a pure-`.harn` script
(`scripts/check_ported_handler_loc.harn`) wired into
`make check-ported-handler-loc` and `.github/workflows/ported-handler-loc.yml`.

## Argparse cookbook

Common patterns using [`std/cli/argparse`](./cli-argparse-reference.md).
Every example assumes `let result = parse(spec, argv)` and a follow-up
error check.

### Positional + required

```harn
import { parser, parse } from "std/cli/argparse"

let spec = parser({
  name: "render",
  args: [
    {name: "template", kind: "positional", required: true,
     help: "Path to the .harn.prompt template."},
  ],
})
let result = parse(spec, argv)
```

Positionals are required by default — flip to `required: false` to opt
out, or set `variadic: true` to greedily collect the rest into a list.

### Repeated flag

```harn
{name: "model", kind: "flag", short: "-m", long: "--model",
 multi: true, value_name: "ID",
 help: "Model id; repeat for multi-model fanout."}
```

With `multi: true`, the parsed value is a `list<string>` (defaulting to
`[]` when unspecified), so `-m claude-opus-4-7 -m gpt-5` yields
`["claude-opus-4-7", "gpt-5"]`.

### `--` separator → `rest`

`argparse` always routes everything after a bare `--` into
`parsed.rest`, no matter what flags or positionals are still pending:

```bash
harn greet -- --not-a-flag "hello world"
```

`parsed.rest` is `["--not-a-flag", "hello world"]`. Use this to forward
verbatim argv to a child process or to a downstream script.

### Error envelope handling

`parse` returns `{ok: dict}` or `{err: ParseError}`. Narrow with the
standard `r.err != nil` pattern and let `render_help` produce the
usage block:

```harn
import { parser, parse, render_help } from "std/cli/argparse"

let result = parse(spec, argv)
if result.err != nil {
  __io_eprintln(render_help(spec))
  __io_eprintln("error: " + result.err.hint)
  exit(2)
}
let parsed = result.ok
```

The error `kind` is one of `missing_required`, `unknown_flag`,
`unknown_arg`, `value_required`, or `bad_value`. Each carries the
offending argv string in `arg` and a one-line `hint`.

See the [`std/cli/argparse` reference](./cli-argparse-reference.md) for
the full surface, error catalog, and `--help` layout contract.

## Output rendering

JSON-mode and human-mode rendering split cleanly through
[`std/cli/render`](./cli-render-reference.md):

```harn
import { envelope, write_envelope, json_mode } from "std/cli/render"
import { ansi_bold } from "std/ansi"
import { render_table } from "std/table"

fn main(harness: Harness) {
  let items = [{provider: "anthropic", model: "claude-opus-4-7"}]
  if json_mode() {
    write_envelope(envelope({
      schema_version: 1,
      api_stability: "stable",
      payload: {items: items},
    }))
    return
  }
  harness.stdio.println(ansi_bold("Models", {}))
  harness.stdio.println(render_table(items, {
    headers: ["Provider", "Model"],
  }))
}
```

- **`std/ansi`** handles color and tty detection; `NO_COLOR` and
  `HARN_COLOR` are honored automatically.
- **`std/table`** renders aligned tables, markdown tables, and
  key/value tables with column auto-width and per-cell truncation.
- **`std/cli/render`** layers the `envelope()` / `write_envelope()`
  helpers on top so JSON output stays snapshot-test friendly with a
  pinned top-level key order (`schemaVersion`, `apiStability`,
  optional `warnings`, then `payload`).

`json_mode()` reads `HARN_OUTPUT_JSON` so the script doesn't need to
re-parse its own `--json` flag — the dispatch wedge already saw the
host's choice.

See the [`std/cli/render` reference](./cli-render-reference.md) for the
full envelope contract and the [`harn --json` contract](./cli-json-contract.md)
for the agent-facing version-bump discipline.

## Config, data, and cache paths

CLI scripts that need app-specific directories should use
[`std/cli/paths`](./cli-paths-reference.md):

```harn
import { xdg_cache_home, xdg_config_home, xdg_data_home } from "std/cli/paths"

let config_dir = xdg_config_home("harn")
let data_dir = xdg_data_home("harn")
let cache_dir = xdg_cache_home("harn")
```

The helpers honor absolute XDG env vars first, ignore relative XDG env
vars, use `~/Library/Application Support` and `~/Library/Caches` on
macOS when XDG is unset, and fall back to the standard `$HOME/.config`,
`$HOME/.local/share`, and `$HOME/.cache` roots elsewhere. They only
resolve paths; call `harness.fs.mkdir(...)` if a script needs to create
the directory.

## Adding a host capability

When a port discovers a gap in the `harness.*` namespace — something
the Rust handler does that no `.harn` script can — the answer is a new
builtin via the G4 pattern ([#2297][g4]). G4 landed a first round of
free builtins (`sha256_hex`, `term_width`,
`term_height`, `mkdtemp`, `glob`, `llm_catalog`,
`llm_provider_status`) that today live as top-level functions so the
ports could move. Directory policy that can be expressed from env vars
belongs in `std/cli/paths` instead of a host capability. `spawn_captured`
has since moved to `harness.process.spawn_captured`. The long-term
direction promotes each remaining free builtin to its
`harness.X.Y` sub-handle through follow-up issues
[#2337][f7]-[#2340][f10] and [#2342][f12]. Add new capabilities the same way:

1. Register the builtin in `crates/harn-vm/src/stdlib/<area>.rs` with
   `register_builtin` and the matching `Harness` accessor.
2. Drop a conformance fixture under `conformance/tests/host_<cap>_*`
   pinning the contract.
3. Document the capability under `docs/src/host-capabilities/` and
   cross-link from the parent index.

Keep the builtin small and orthogonal — single capability, single
return shape, no implicit side-effects. If a port wants something that
feels like an editorial decision (a specific output format, a UI
choice), keep that in the `.harn` script and add the smallest possible
primitive instead.

## Performance budget

Cold-start matters more than steady-state for the CLI. Every ported
subcommand has to meet a wall-clock budget pinned in
`perf/cli/budgets.toml` and gated by `make bench-cli-cold-start`. The
gate runs each subcommand under a cold bytecode cache, samples a
fixed number of invocations, and asserts the median stays under the
budget.

```bash
make bench-cli-cold-start
```

Two ratchets work together to keep the port honest:

- **`make bench-cli-cold-start`** ([G5][g5] / `perf/cli/budgets.toml`)
  fails when a ported script's cold-start regresses. Bump the budget
  only with a rationale comment — re-tuning the budget is the loudest
  possible signal that a port got slower.
- **`make check-ported-handler-loc`** ([C1 #2314][c1-ratchet] /
  `scripts/ported_handlers.toml`) fails when a Rust handler grows back
  past its committed line count. When a port advances and shrinks a
  shim, drop the budget to the new `current+5` in the same PR. When a
  new handler joins the ratcheted set, append a fresh `[[handler]]`
  block with its current+5 budget — never retroactively tighten the
  existing entries.

The bytecode cache (`HARN_BYTECODE_CACHE`) amortizes parse + typecheck
across invocations after the first one; the cold-start gate measures
the worst case. If a port can't meet its budget, the usual fixes are
trimming imports from the script (only pull in what `main` actually
uses), avoiding redundant `json_stringify` round-trips, and pushing
per-call allocation off the hot path.

[harn#2293]: https://github.com/burin-labs/harn/issues/2293
[dispatch]: https://github.com/burin-labs/harn/blob/main/crates/harn-cli/src/dispatch.rs
[stdlib-table]: https://github.com/burin-labs/harn/blob/main/crates/harn-stdlib/src/lib.rs
[w1-pr]: https://github.com/burin-labs/harn/pull/2328
[c1-ratchet]: https://github.com/burin-labs/harn/issues/2314
[g4]: https://github.com/burin-labs/harn/issues/2297
[g5]: https://github.com/burin-labs/harn/issues/2298
[f7]: https://github.com/burin-labs/harn/issues/2337
[f10]: https://github.com/burin-labs/harn/issues/2340
[f12]: https://github.com/burin-labs/harn/issues/2342
