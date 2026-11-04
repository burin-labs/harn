- **A grant declares its own credential.** Output redaction no longer reads a variable's name. A `secret://` grant,
  a grant bound to one command, and the new `secret-env:VAR` source (`GrantSourceSpec::SecretEnv`) are filtered from
  the current session's output. Filtering matches exact values and is best-effort cleanup, not a security boundary;
  encoded forms and output outside a session are no longer filtered.

  Migration: declare a session-wide `env:` grant that holds a credential as `secret-env:`.
