- `harn-vm` tool-call parser feedback is now precise about what went wrong and
  how to fix it, so cheap coding models stop re-emitting the same broken turn:
  - Source/test code emitted where a tool call was expected (`it(...)`,
    `expect(...)`, `describe(...)`, `assertServiceCount(...)`, …) no longer
    reports a misleading `Unknown tool 'it'`. The feedback now names the real
    cause — code outside a heredoc/`content` envelope — and tells the model to
    wrap it.
  - The "Unknown tool" available-tools list is no longer capped at 20 names
    (which could hide the very tool the model needed). It lists every tool, and
    appends an explicit `…and N more` only for a pathologically large registry —
    never silently truncating. The highest-frequency misses (`read`, `write`,
    `list`, `search`, …) now carry a canonical alias hint, e.g. `read` →
    `look({ intent: "read" })`. Genuine close-miss typos still get the
    `Did you mean '<tool>'?` suggestion. Applies to both the bare-TS and
    native-JSON tool-call parsers.
  - A denied/permission-gated tool result now carries an actionable `next_step`
    ("do not retry the same call; make progress with allowed tools, or ask for
    permission") instead of a bare `{"error":"permission_denied"}`.
  - Object-literal tool-call parse errors now include a short `Raw:` preview of
    the offending span (mirroring the native-JSON parser), so the model can tell
    which of several on-screen calls failed.
