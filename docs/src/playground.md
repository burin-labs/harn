# Playground

`harn playground` runs a pipeline against a Harn-native host module in the same
process. It is intended for fast pipeline iteration without wiring a JSON-RPC
host or booting a larger app shell.

## Quick start

The repo ships with a minimal example:

```bash
harn playground \
  --host examples/playground/host.harn \
  --script examples/playground/echo.harn \
  --task "Explain this repository in plain English"
```

`--task` is exposed to the script through the `HARN_TASK` environment variable,
so the example reads it with `env_or("HARN_TASK", "")`.

If you want an offline smoke test, force the mock provider:

```bash
harn playground \
  --host examples/playground/host.harn \
  --script examples/playground/echo.harn \
  --task "Say hello" \
  --llm mock:mock
```

For deterministic end-to-end iteration, `harn playground` also accepts the
same JSONL fixture flags as `harn run`:

```bash
harn playground \
  --host examples/playground/host.harn \
  --script examples/playground/echo.harn \
  --task "Explain this repository" \
  --llm-mock fixtures/playground.jsonl
```

Use `--llm-mock-record <path>` once to capture a replayable fixture, then
switch back to `--llm-mock <path>` while you iterate on control flow.

## Host modules

A playground host is just a `.harn` file that exports the functions your
pipeline expects:

```harn
pub fn build_prompt(task_text) {
  return "Task: " + task_text + "\nWorkspace: " + cwd()
}

pub fn request_permission(tool_name, request_args) -> bool {
  return true
}
```

The playground command loads those exported functions and makes them available
to the entry script during execution. If the script calls a host function that
the module does not export, the command fails with a pointed error naming the
missing function and the caller location.

## Watch mode

Use `--watch` to re-run when either the host module or the script changes:

```bash
harn playground --watch --task "Refine the prompt"
```

The watcher tracks the host and script parent directories recursively and
debounces save bursts before re-running.

## Starter project

Use the built-in scaffold when you want a dedicated scratchpad:

```bash
harn new pipeline-lab-demo --template pipeline-lab
cd pipeline-lab-demo
harn playground --task "Summarize this project"
```
