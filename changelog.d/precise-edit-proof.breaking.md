`harn_vm::text_diff::LineChange` now includes the original line `terminator` so
source-image consumers can retain blank lines and exact line endings.

Migration: add `terminator: ""` when constructing a `LineChange` literal without
a line ending, or supply its original `"\n"`, `"\r\n"`, or `"\r"` ending.
