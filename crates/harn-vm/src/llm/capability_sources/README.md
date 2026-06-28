# Provider Capability Sources

These TOML fragments are the editable source for Harn's built-in provider/model
capability matrix. The runtime embeds `../capabilities.toml`, which is generated
from this directory by:

```sh
harn provider catalog build-capabilities
```

Use `harn provider catalog build-capabilities --check` to verify that
`../capabilities.toml` matches the fragments. Direct edits to
`../capabilities.toml` are invalid because the next generation pass will
overwrite them.

Fragments use the same `CapabilitiesFile` schema as `[capabilities]` project and
package overlays. File names are sorted lexicographically; keep numeric prefixes
when adding rules because capability matching is first-match-wins.

## Tool mode evidence

Capability rows can prefer `native`, `text`, or `json` tool calling. Choose the
default from end-to-end `agent_loop` evidence, not from advertised provider
schemas alone.

Prefer native tools when a harness-author-style loop can call a tool, receive
the result, and finish cleanly. Prefer text or JSON when that route completes
there but forced native mode emits no native tool call, malformed native-call
fields, provider-specific XML/text markup, or prose claiming a tool already ran.

When recording a non-native default, set `tool_mode_parity` and
`tool_mode_parity_notes` with the observed failure mode and the successful
fallback. Keep notes falsifiable: include the date, provider/model route,
requested reasoning/tool mode, and the transcript-visible failure signature.

Provider parsers stay below policy. They reject only structurally empty tool
turns: billed output with no visible text, no dispatchable tool call, and no
server-hosted tool event. Any non-empty committed text, even a terse token after
a tool result, is returned to `agent_loop`, where loop policy decides whether it
is completion, a nudge, or a failed required-tool stage.
