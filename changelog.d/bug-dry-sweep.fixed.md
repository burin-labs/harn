- **Hostlib deterministic tools now reject malformed scalar payloads and report
  process/file-watch edge cases correctly.** Shared VmValue parsing now rejects
  non-finite or out-of-range numeric inputs, `run_command` no longer ignores
  malformed `cwd`/`stdin` fields or wait failures, directory listing reports
  symlinks without following their targets, inline command output preserves
  invalid UTF-8 lossily, and file watches can subscribe to `access`/`other`
  events.
