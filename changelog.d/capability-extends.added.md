- **Capability rules support `extends = true` field-wise fall-through.** A
  matching `[[provider.<name>]]` capability rule that sets `extends = true`
  now contributes ONLY the fields it explicitly sets and lets resolution
  continue to later matching rules (user rules before built-in rules, then
  the `provider_family` chain) and ultimately to provider / built-in defaults
  to fill the rest. A rule without `extends` (or with `extends = false`)
  terminates resolution exactly as before, so every existing catalog and
  overlay is unchanged. This lets an overlay tweak one field of a shipped row
  without copying the whole row verbatim (which silently freezes the rest of
  the row against catalog updates). The capability matrix (`harn` audit /
  matrix surfaces) reports an `extends` row's own fields and, for a matched
  model, the full precedence chain of absorbed rule patterns.
