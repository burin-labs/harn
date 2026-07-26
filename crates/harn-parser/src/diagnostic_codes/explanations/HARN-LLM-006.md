# HARN-LLM-006 — provider, model, and tool format form a known-unsafe composition

## How to fix

- Omit `tool_format` to use the catalog default, or select the catalog-recommended format.
- Choose a provider/model route whose declared tool-calling channel supports the requested format.
- For a deliberate agent-loop probe, add a non-empty `tool_format_override_reason`; Harn records the override in the transcript.

Dynamic and custom routes remain open-world. This diagnostic only rejects literal compositions that Harn's capability
registry already knows it must correct or cannot serve.
