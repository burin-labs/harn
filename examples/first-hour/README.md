# Three offline examples

Run these examples with an installed Harn release. They use synthetic model
responses, need no credentials, and make no model requests.

## Run an agent loop

```sh
harn run agent.harn
```

```text
Running: Say hello.
done
Mock response to 2-word prompt: Say hello.
```

`agent.harn` bounds the loop to two iterations. The `mock` provider supplies a
deterministic answer. Replace the provider and model options after following
the [provider guide](../../docs/src/provider-setup.md).

## Let the agent call a tool

```sh
harn run tools.harn --llm-mock tools.jsonl
```

```text
tool: 2 + 3 = 5
done
2 + 3 = 5.
```

The fixture first requests `add`, then supplies the final answer. The first
output line comes from the tool itself, so it demonstrates that Harn invoked
the handler. The tool accepts two integers and returns an integer; it has no
file or process effects. The fixture illustrates execution, not model quality.

## Record and replay a model response

```sh
harn run record-replay.harn --llm-mock-record responses.jsonl
harn run record-replay.harn --llm-mock responses.jsonl
```

Both commands print:

```text
Mock response to 2-word prompt: Say hello.
```

The first command records the synthetic response in `responses.jsonl`. The
second reads it through the same model-call interface. This fixture replays
model responses; it doesn't snapshot the filesystem or replay tool effects.
Recorded responses can include supplied prompts and tool arguments. Inspect
them before sharing. Delete `responses.jsonl` to discard this local recording.
