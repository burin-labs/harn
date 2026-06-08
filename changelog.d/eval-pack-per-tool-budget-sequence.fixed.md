- Fixed named per-tool `tool_budgets` (e.g. `{edit: 1}`) being silently
  unenforced for live coding-agent eval packs. The in-process executor reports
  tool usage only as a per-call `sequence` array (no `by_tool` map), so the
  budget checker could never resolve a named tool's count and skipped the limit
  entirely. The checker now falls back to counting occurrences in `sequence`, so
  a configured per-tool budget is enforced regardless of executor summary shape.
