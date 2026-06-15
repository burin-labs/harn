- Fixed token-pressure reminders to use the current prompt/context token count
  when available instead of cumulative session token totals, preventing false
  "compact or summarize" warnings in long agent loops.
