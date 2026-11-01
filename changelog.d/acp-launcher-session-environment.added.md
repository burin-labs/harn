- **A standard ACP editor can open a session on `harn serve acp`.** The new
  `--session-environment inherited|isolated` flag states, at launch, the
  environment policy for a client whose `session/new` names none. Without the
  flag such a request is still refused with `environment_policy.missing`. A
  policy the client sends always wins, and the `session/new` result now reports
  the policy that applied as `environmentPolicy: {kind, source}`.
