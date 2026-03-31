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
harn fmt --check main.harn    # check mode (no changes, exit 1 if unformatted)
```

## harn lint

Lint a `.harn` file for common issues (unused variables, unreachable code,
empty blocks, etc.).

```bash
harn lint main.harn
```

## harn check

Type-check a `.harn` file without executing it.

```bash
harn check main.harn
```

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
