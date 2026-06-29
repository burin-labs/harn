- **Strings now support `.slice(start, end?)` as an alias for `.substring`.**
  `slice` was a list-only method, so calling it on a string raised a runtime
  `string has no method \`slice\`` that aborted the whole agent loop. Harness
  authors (and JS/Python muscle memory) reach for `.slice` on strings
  constantly; it is now char-based and negative-index aware, mirroring
  `list.slice` exactly, which structurally removes the crash class rather than
  chasing individual call sites. This was crashing `agent/stall`'s failure-
  snippet path whenever a failing tool result carried a >240-char error body
  (e.g. a long compiler error), terminating the loop mid-fix.
