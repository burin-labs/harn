# CLI Reference

All commands available in the `harn` CLI.

## harn run

Execute a `.harn` file.

```bash
harn run <file.harn>
harn run --trace <file.harn>
harn run -e 'println("hello")'
harn run --deny shell,exec <file.harn>
harn run --allow read_file,write_file <file.harn>
```

| Flag | Description |
|---|---|
| `--trace` | Print LLM trace summary after execution |
| `-e <code>` | Evaluate inline code instead of a file |
| `--deny <builtins>` | Deny specific builtins (comma-separated) |
| `--allow <builtins>` | Allow only specific builtins (comma-separated) |

You can also run a file directly without the `run` subcommand:

```bash
harn main.harn
```

## harn test

Run tests.

```bash
harn test conformance                  # run conformance test suite
harn test tests/                       # run user tests in directory
harn test tests/ --filter "auth*"      # filter by pattern
harn test tests/ --parallel            # run tests concurrently
harn test tests/ --watch               # re-run on file changes
harn test conformance --verbose        # show per-test timing
harn test tests/ --record              # record LLM fixtures
harn test tests/ --replay              # replay LLM fixtures
```

| Flag | Description |
|---|---|
| `--filter <pattern>` | Only run tests matching pattern |
| `--parallel` | Run tests concurrently |
| `--watch` | Re-run tests on file changes |
| `--verbose` / `-v` | Show per-test timing and detailed failures |
| `--junit <path>` | Write JUnit XML report |
| `--timeout <ms>` | Per-test timeout in milliseconds (default: 30000) |
| `--record` | Record LLM responses to `.harn-fixtures/` |
| `--replay` | Replay recorded LLM responses |

When no path is given, `harn test` auto-discovers a `tests/` directory
in the current folder.

## harn repl

Start an interactive REPL with syntax highlighting.

```bash
harn repl
```

## harn fmt

Format `.harn` source files. Accepts files or directories.

```bash
harn fmt main.harn
harn fmt src/
harn fmt --check main.harn            # check mode (no changes, exit 1 if unformatted)
harn fmt --line-width 80 main.harn    # custom line width
```

| Flag | Description |
|---|---|
| `--check` | Check mode: exit 1 if any file would be reformatted, make no changes |
| `--line-width <N>` | Maximum line width before wrapping (default: 100) |

The formatter enforces a **100-character line width** by default (overridable with `--line-width`). When a line exceeds
this limit the formatter wraps it automatically:

- **Comma-separated forms** — function call arguments, function declaration
  parameters, list literals, dict literals, struct construction fields,
  enum constructor payloads, selective import names, interface method
  parameters, and enum variant fields all wrap with one item per line and
  trailing commas (except selective imports, which omit the trailing comma).
- **Binary operator chains** — long expressions like `a + b + c + d` break
  before the operator. Operators that the parser cannot resume across a bare
  newline (`-`, `==`, `!=`, `<`, `>`, `<=`, `>=`, `in`, `not in`, `??`)
  get an automatic backslash continuation (`\`); other operators (`+`, `*`,
  `/`, `%`, `||`, `&&`, `|>`) break without one.
- **Operator precedence parentheses** — the formatter inserts parentheses
  to preserve semantics when the AST drops them (e.g. `a * (b + c)` stays
  parenthesised) and for clarity when mixing `&&` / `||`
  (e.g. `a && b || c` becomes `(a && b) || c`).

## harn lint

Lint one or more `.harn` files or directories for common issues (unused
variables, unreachable code, empty blocks, missing `///` HarnDoc on public
functions, etc.).

```bash
harn lint main.harn
harn lint src/ tests/
```

## harn check

Type-check one or more `.harn` files or directories and run preflight
validation without executing them. The preflight pass resolves imports, checks
literal `render(...)` targets, detects import symbol collisions across
modules, validates `host_invoke(...)` capability/operation pairs, and flags
missing template resources, execution directories, and worker repos that would
otherwise fail only at runtime. Source-aware lint rules run as part of
`check`, including the `missing-harndoc` warning for undocumented `pub fn`
APIs.

```bash
harn check main.harn
harn check src/ tests/
harn check --host-capabilities host-capabilities.json main.harn
harn check --bundle-root .bundle main.harn
```

| Flag | Description |
|---|---|
| `--host-capabilities <file>` | Load a host capability manifest for preflight validation. Supports plain `{capability: [ops...]}` objects, nested `{capabilities: ...}` wrappers, and per-op metadata dictionaries. |
| `--bundle-root <dir>` | Validate `render(...)` and template paths against an alternate bundled layout root |

## harn init

Scaffold a new project with `harn.toml` and `main.harn`.

```bash
harn init              # create in current directory
harn init my-project   # create in a new directory
```

## harn watch

Watch a file for changes and re-run it automatically.

```bash
harn watch main.harn
harn watch --deny shell main.harn
```

## harn acp

Start an ACP (Agent Client Protocol) server on stdio.

```bash
harn acp                    # bridge mode, no pipeline
harn acp pipeline.harn      # execute a pipeline per prompt
```

See [MCP and ACP Integration](./mcp-and-acp.md) for protocol details.

## harn runs

Inspect persisted workflow run records.

```bash
harn runs inspect .harn-runs/<run>.json
harn runs inspect .harn-runs/<run>.json --compare baseline.json
```

## harn replay

Replay a persisted workflow run record from saved output.

```bash
harn replay .harn-runs/<run>.json
```

## harn eval

Evaluate a persisted workflow run record as a regression fixture.

```bash
harn eval .harn-runs/<run>.json
harn eval .harn-runs/<run>.json --compare baseline.json
harn eval .harn-runs/
harn eval evals/regression.json
```

`harn eval` accepts three inputs:

- a single run record JSON file
- a directory of run record JSON files
- an eval suite manifest JSON file with grouped cases and optional baseline comparisons

## harn serve

Start an A2A (Agent-to-Agent) HTTP server.

```bash
harn serve agent.harn               # default port 8080
harn serve --port 3000 agent.harn   # custom port
```

See [MCP and ACP Integration](./mcp-and-acp.md) for protocol details.

## harn mcp-serve

Serve a Harn pipeline as an MCP server over stdio.

```bash
harn mcp-serve agent.harn
```

See [MCP and ACP Integration](./mcp-and-acp.md) for details on defining
tools, resources, and prompts.

## harn mcp

Manage standalone OAuth state for remote HTTP MCP servers.

```bash
harn mcp redirect-uri
harn mcp login notion
harn mcp login https://mcp.notion.com/mcp
harn mcp login my-server --url https://example.com/mcp --client-id <id> --client-secret <secret>
harn mcp status notion
harn mcp logout notion
```

`harn mcp login` resolves the server from the nearest `harn.toml` when you pass
an MCP server name, or uses the explicit URL when you pass `--url` or a raw
`https://...` target. The CLI:

- discovers OAuth protected resource and authorization server metadata
- prefers pre-registered `client_id` / `client_secret` values when supplied
- falls back to dynamic client registration when supported by the server
- stores tokens in the local OS keychain and refreshes them automatically

Relevant flags:

| Flag | Description |
|---|---|
| `--url <url>` | Explicit MCP server URL when logging in/out by a custom name |
| `--client-id <id>` | Use a pre-registered client ID instead of dynamic registration |
| `--client-secret <secret>` | Optional client secret for `client_secret_post` / `client_secret_basic` servers |
| `--scope <scopes>` | Override or provide requested OAuth scopes |
| `--redirect-uri <uri>` | Override the default loopback redirect URI (default shown by `harn mcp redirect-uri`) |

Security guidance:

- prefer the narrowest scopes the remote MCP server supports
- treat configured `client_secret` values as secrets
- review remote MCP capabilities before using them in autonomous workflows

## Release gate

For repo maintainers, the comprehensive docs audit, verification gate, version
bump flow, and publish sequence are orchestrated by:

```bash
./scripts/release_gate.sh audit
./scripts/release_gate.sh full --bump patch --dry-run
```

## harn add

Add a dependency to `harn.toml`.

```bash
harn add my-lib --git https://github.com/user/my-lib
```

## harn install

Install dependencies declared in `harn.toml`.

```bash
harn install
```

## harn version

Show version information.

```bash
harn version
```
