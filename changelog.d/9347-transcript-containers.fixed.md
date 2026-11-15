- Transcript normalization decodes legacy message, tool call and usage containers
  before reading their fields and preserves raw payloads and extension fields.
  Argument diagnostics now distinguish encoded JSON null from absent arguments
  and accept valid argument objects containing their own `_raw` key.
