# HARN-LLM-006 — provider, model, and requested options form a known-unsafe composition

## How to fix

- Remove a portable generation option that the selected route does not support, or choose a compatible route.
- For `cache` or `prompt_cache_ttl`, declare prompt-cache support and
  selectable TTL values for the custom route, or remove the request.
- Put a provider-native control below `provider_options.<provider>` instead of spelling it as a portable top-level option.
- Omit `tool_format` to use the catalog default, or select the catalog-recommended format.
- Choose a provider/model route whose declared tool-calling channel supports the requested format.
- For a deliberate probe, add a non-empty `tool_format_override_reason`;
  agent loops record the override event, and provider-call records expose the
  effective format and native tool count.

Dynamic values remain under the runtime guard. Custom generation routes remain
open-world, but custom cache controls require authored capability facts because
Harn must select a provider-specific lowering. This diagnostic only rejects a
literal composition the registry can already prove unsafe.
