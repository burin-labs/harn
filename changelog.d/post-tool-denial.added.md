- **PostToolUse hooks can return typed denials.** A hook may return
  `{result, denial: {kind, message}}`; the denial survives later result
  rewrites and reaches the agent tool envelope without relying on message text.
