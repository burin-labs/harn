- **Runtime, ACP, DAP, OAuth, and stdlib edge cases.** Fixed integer
  division overflow panics, channel-select hangs on closed empty channels,
  duplicate in-flight ACP request IDs, stale DAP output flushing, OAuth token
  expiry validation, OAuth error-body secret leakage, and inconsistent
  duration option coercion across stdlib modules.
