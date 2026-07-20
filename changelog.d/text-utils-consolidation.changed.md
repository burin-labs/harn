- **One owner for shared text utilities.** Truncation, ANSI stripping, and identifier
  case conversion moved to `harn_vm::text`, replacing eight ad-hoc truncators, three
  hand-written Levenshtein implementations (now `strsim`), and four private case
  converters. Behavioral consequences: case conversion outside the `strings` builtins
  now follows the same word-splitting rules those builtins use, so acronyms convert as
  one word (`HTTPServer` → `http_server`, not `h_t_t_p_server`) — this affects linter
  rename suggestions, connector event discriminators, and import-path guessing. The
  three "did you mean" ranking policies (typechecker snake-segment reordering,
  length-tiered VM name suggestions, host-operation matching) are unchanged.
