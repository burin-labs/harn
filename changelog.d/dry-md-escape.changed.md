- **Internal: unified the Markdown table-cell pipe escaping used by the CLI
  report commands.** The eval summary, provider-matrix, provider-support, and
  diagnostics-catalog commands each carried a private copy of the same
  `|`-escaping helper; they now share `crate::format::escape_md`. No behavior
  change to any rendered report.
