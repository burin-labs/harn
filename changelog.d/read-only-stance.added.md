- **Read-only stance (experimental, default-off).** `agent_loop` gains a
  `read_only_stance` option: tasks classified as read-only get a
  least-privilege tool window (read-only-annotated tools only; unannotated
  tools count as mutating) plus an auto-registered `request_write_access`
  escape hatch whose consent check verifies — agentically, against the
  session's recent user messages — that the user expressed or implied consent
  before mutating tools return. Transitions emit typed `stance_transition`
  events (armed / write_access_granted / write_access_denied / disarmed) on
  the agent event stream and the ACP session-update channel.
