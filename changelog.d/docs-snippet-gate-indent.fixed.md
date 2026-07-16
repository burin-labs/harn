- **The docs snippet gate now checks indented `harn` code blocks.** Fenced
  blocks nested under a list item were silently skipped, so a block that failed
  `harn parse` shipped in the `HARN-OWN-003` "How to fix" guidance. The gate
  reads those blocks now, and the `HARN-OWN-003` remedy shows a form that
  compiles.
