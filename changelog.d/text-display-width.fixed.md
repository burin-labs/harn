- The formatter now measures line width in terminal display columns rather than
  character counts, so a line holding CJK or emoji is wrapped and its overflow
  reported against the width it actually occupies. ASCII source formats
  identically to before.
- `harn demo` no longer wraps its scenario descriptions by byte length, which
  broke lines far too early for any non-ASCII text; the listing now wraps on
  display columns.
