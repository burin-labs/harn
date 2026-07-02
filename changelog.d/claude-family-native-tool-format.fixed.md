- Claude-family models now default to `native` tool calling at the family level on OpenRouter and on
  prefixed direct-Anthropic ids: catalog catch-all rules cover ids the versioned capability rows miss
  (new family names, unparseable version segments, dated slugs, pre-4.x models), which previously fell
  through to the global text-channel `json` default. Hosts no longer need per-alias `tool_format`
  pins for Claude routes; explicit pins still win.
