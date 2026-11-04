Output redaction now covers only credentials a grant hands to a child. A session-wide `env:` grant
that carries a path or a mode, and a literal grant, stay readable in command output, events and
transcripts.
