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

## Recover from a rejected tool call

```sh
harn run tool-recovery.harn --llm-mock tool-recovery.jsonl
```

```text
tool: 2 + 3 = 5
done
rejected calls: 1
successful calls: 1
2 + 3 = 5.
```

The first fixture entry omits the required `b` argument. Harn rejects the call
before entering the handler and sends the error back to the model. The second
entry matches that error and supplies both arguments; the handler runs once.
The final entry completes the loop within its three-iteration bound. The
program checks that one call was rejected and one succeeded.

This synthetic fixture demonstrates error feedback and recovery through the
existing loop. It doesn't measure whether a real model will correct its call.
