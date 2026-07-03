A `<tool_call>` block whose body is a JSON array of calls (`[{ "name": …, "arguments": … }]`) is no
longer silently swallowed as prose. A single-element array now dispatches its one call identically
to the bare and object-envelope forms; a multi-element array surfaces the actionable "one call per
`<tool_call>` block" error instead of vanishing with no feedback. The array body was being intercepted
by the narration-recovery path, which guarded `<`- and `{`-leading bodies from the prose fallback but
omitted `[`.
