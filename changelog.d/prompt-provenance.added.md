- Run tapes now trace an agent's system prompt to its prompt sources. Plain text spans record their real
  template line, column and revision. Each call reports template-text and filled-value bytes separately, in
  place of one attributed count. A call record also states its reasoning effort, the offered tools with
  their titles (MCP titles included), and each tool result as the model saw it.
- The project-profile guidance, active-skills block, MCP advisory context and terminal output contract now
  render from stdlib `.harn.prompt` assets. These and the native tool protocol section are attributed on the
  tape even after prompt assembly trims them.
- The Rust prompt-prose ratchet now also flags literals assigned to prompt-bound fields such as `body:` in
  `harn-builtins` and `harn-vm`.
