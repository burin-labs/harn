# LLM handler helpers

`std/llm/handlers` provides small middleware helpers for call handlers that
accept a call dict such as `{prompt, system, opts}`.

```harn
import { with_circuit_breaker } from "std/llm/handlers"

pipeline default() {
  let raw_handler = fn(call) {
    return llm_call(call.prompt, call?.system, call.opts)
  }
  let pooled = with_circuit_breaker(raw_handler)
  let shared = with_circuit_breaker(raw_handler, {name: "primary-llm", threshold: 3})
}
```

`with_circuit_breaker(handler, options?)` uses a circuit named from each call's
`opts.provider` and `opts.model`, falling back to `"<unset>"` for missing
fields. A failing upstream therefore opens only its own circuit even when many
models share the same wrapper. Pass `name` when a caller deliberately wants one
shared circuit.

Options:

| Option | Default | Description |
|---|---:|---|
| `name` | derived | Manual circuit name for shared state |
| `threshold` | `5` | Consecutive failures before the circuit opens |
| `reset_ms` | `30000` | Time before `circuit_check` reports `half_open` |
