- `harn-hostlib`: `sanitize_component` no longer lets an all-dots session id (`.`, `..`) pass through
  verbatim, which let staged-filesystem state escape one directory level via `push("..")`. Such ids now
  fall back to the hashed, traversal-free form like any other unsafe component.
