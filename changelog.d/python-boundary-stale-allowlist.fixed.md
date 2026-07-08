- **Python boundary checks now reject stale MCP helper shims.** The boundary allowlist no longer
  permits MCP helper filenames that were already cut over to Harn, and the remaining proxy fixture
  documents its real TCP-listener constraint instead of pointing at closed cutover work.
