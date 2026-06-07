- **Text tool-call parser no longer wastes a turn on narration wrapped in
  `<tool_call>` tags.** Weak value models (DeepSeek) wrap their thinking in
  `<assistant_prose>` *inside* a `<tool_call>` block. The parser previously
  treated this as a malformed call, dropped it, and emitted a "could not be
  parsed" diagnostic — costing the model its whole turn for merely narrating.
  Such a block is now reclassified as assistant narration: the inner text is
  preserved as prose, no tool call and no parse error are emitted, and a
  prose-only turn surfaces to the loop as "said X but took no action" so the
  normal no-tool-call nudge applies. If the same wrapper also carries a real
  `name({ ... })` / nested-XML call, that call is still recovered and
  dispatched. The allowance is scoped to a small narration allowlist
  (`assistant_prose`, `thinking`, `reasoning`); unknown wrapped tags that look
  like attempted calls (e.g. `<frobnicate>{...}`) are still rejected.
