- **Cheap models no longer loop on JSON-escaped heredoc bodies; parse errors now
  reach the model.** Two fixes for the failure where a model's `edit(...)` turn
  yielded zero tool calls and then re-emitted the identical malformed call until
  the loop exhausted. (1) Parser recovery: a `<<EOF` heredoc whose body uses
  literal `\n` line breaks (the JSON-escaped one-liner form cheap models like
  qwen3.6 emit) is now decoded and dispatched instead of hard-rejected with
  "expected newline after heredoc tag"; real-newline heredocs are parsed exactly
  as before. (2) Feedback fidelity: a turn whose tool calls were all dropped by
  the parser now gets the purpose-built `parse_guidance` feedback (which names
  the exact diagnostic and shows the heredoc syntax) and is excluded from the
  no-progress stall streak, instead of the misleading "emit one well-formed tool
  call" nudge. Both fire purely on the syntactic parse-error condition, so strong
  models that emit clean calls never trigger them.
