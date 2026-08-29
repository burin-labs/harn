- Load connector packages that still declare `required_secrets` as bare id
  strings. Typing the secret direction made every already-published connector
  package fail to parse, so upgrading Harn broke consumers that pin those
  packages by git rev. The legacy spelling now resolves to the outbound
  direction the package was published against; the typed
  `{ id, direction }` table stays the authoring form and is still checked key
  by key.
