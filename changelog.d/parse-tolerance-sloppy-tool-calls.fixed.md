- **Text tool-call parser now recovers more sloppy-but-unambiguous shapes from
  weak value models.** Building on the nested-XML wrapper acceptance, the
  `<tool_call>` parser now also recovers: a nested XML tool tag whose inner
  close is mismatched (`</edit_call>`) or absent and whose outer `</tool_call>`
  is missing entirely (terminating the body at the JSON object's closing
  brace); a missing inner close paired with a duplicate/trailing `</tool_call>`
  (the orphan close tag is swallowed silently); and leading-dot decimal literals
  (`.100` → `0.100`) inside an otherwise-valid `name({ ... })` argument object.
  Recovery stays constrained to registered/implicit tool names with JSON-object
  arguments and canonicalizes back to `<tool_call>name({ ... })</tool_call>` on
  replay; unknown inner tags are still rejected.
