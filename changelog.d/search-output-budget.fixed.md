- **Hostlib search now bounds oversized line payloads.** `hostlib_tools_search`
  clips long matched/context lines at UTF-8 boundaries, keeps matched-line
  snippets centered on the hit, exposes `max_line_bytes` for presets/APIs, and
  marks the response `truncated` when either match count or line content is
  clipped.
