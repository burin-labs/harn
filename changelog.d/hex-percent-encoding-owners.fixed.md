- Replace hand-rolled hex and percent-encoding helpers across the workspace with
  the `hex` and `percent-encoding` crates. This fixes the playground fake
  server mangling multi-byte UTF-8 and malformed escapes in decoded query
  strings, and file-URI decoding accepting `%+1`-style escapes. A workspace
  test now fails when a new hand-rolled copy appears.
