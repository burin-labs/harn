- **Module exports now require explicit `pub`.** A module's import surface is exactly the functions it marks
  `pub` (plus `pub import` re-exports); a module with no `pub` functions exports nothing. The previous
  "a module with no `pub` exports everything" fallback is removed — it made adding the first `pub` a silent
  breaking change for a module's importers. Both `harn check` (`HARN-IMP-002`) and the runtime loader enforce
  the rule. To migrate, mark intended exports `pub`; the diagnostic points at any selective import that needs it.
