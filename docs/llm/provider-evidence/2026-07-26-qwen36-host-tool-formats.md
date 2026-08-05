# 2026-07-26 Qwen 3.6 host tool-format probe

This probe followed up #3730 with the built-in `provider tool-calibrate`
runner. Each measured cell used the `single_tool_call` case and five live
attempts. OpenRouter and Hugging Face used non-streaming transport; Together's
streaming-only route used streaming transport. The three forced formats were
provider native tools, fenced JSON text tools, and tagged/heredoc text tools.

| Host | Model | Native | JSON | Tagged text | Decision |
| --- | --- | ---: | ---: | ---: | --- |
| OpenRouter | `qwen/qwen3.6-35b-a3b` | 5/5 | 5/5 | 5/5 | Keep native |
| Together | `Qwen/Qwen3.6-Plus` | 5/5 | 5/5 | 1/5 | Keep native |
| Hugging Face | `Qwen/Qwen3.6-35B-A3B` | 3/5 | 5/5 | 4/5 | Prefer tagged text; native unreliable |

The two Hugging Face native failures were billed empty completions. The tagged
text failure exposed a raw model tool tag rather than a dispatchable call, so
the route enables the Qwen reserved-`<tool_call>` delimiter remap. A second
N=5 tagged-text sweep with that remap dispatched 5/5.

A second N=5 `large_string_argument` sweep kept OpenRouter clean in all three
formats. Hugging Face exhausted the probe's 256-token output cap in every JSON
attempt and most native/text attempts, so that sweep is recorded as a probe
limit rather than format evidence; it did not override the complete
single-tool result.

## Hosts without a live verdict

- DashScope could not be called because no `DASHSCOPE_API_KEY` was available.
- Fireworks returned no Qwen 3.6 model from its authenticated `/models`
  endpoint.
- Together's listed 35B-A3B FP8 route requires a dedicated endpoint and was not
  accessible through the serverless credential. `Qwen/Qwen3.6-Plus` requires
  streaming, so its result used streaming transport after fixing the live probe
  to send `stream: true`.

These gaps are not evidence for changing their native pins. OpenRouter's clean
result and Hugging Face's host-specific failure also rule out a family-wide
Qwen 3.6 native-unreliable gate from this data.
