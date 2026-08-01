Session titles can now be pinned to a person's choice, so generated titles no
longer overwrite one the user set. `UpdateSession` writes that omit
`title_pinned` are treated as derived and yield to a pinned title, making
existing auto-titling callers safe without change.
