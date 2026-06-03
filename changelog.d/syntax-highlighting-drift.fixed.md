- Tree-sitter highlighting now covers `break`, `continue`, `require`, and the
  HITL keywords (`ask_user`, `dual_control`, `escalate_to`,
  `request_approval`). They are valid grammar keywords but were silently
  rendered as plain identifiers in Neovim and other tree-sitter editors.
- The VS Code TextMate grammar now nests block comments, so a `*/` inside a
  `/* ... */` no longer ends the comment early, and recognizes raw string
  literals (`r"..."`), which previously orphaned the `r` prefix and
  mis-highlighted backslashes as escape sequences.
