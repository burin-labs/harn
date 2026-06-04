Reserved-token tool-call delimiter remap (`[[CALL]]` → `<tool_call>`) now runs in the shared LLM transport
funnel instead of only the registered OpenAI-compat path. An unregistered `llamacpp` provider (configured via
`providers.toml` but never `provider_register`-ed) previously returned raw wire-form completions, so the tagged
tool-call parser saw zero `<tool_call>` blocks and silently dropped every call — a local `qwen3.6` eval dispatched
0 of its tool calls across 23 turns and hallucinated completion. The remap now fires identically on every route
(registered and unregistered, streaming and non-streaming), with a streaming/non-streaming parity test and debug
logging of the wire→canonical marker counts.
