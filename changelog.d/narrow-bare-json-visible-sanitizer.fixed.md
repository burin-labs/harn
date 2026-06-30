- Narrowed the visible-text sanitizer's bare JSON control-message filter so
  legitimate JSON-only assistant answers with keys such as `tasks`, `steps`, or
  `reasoning` remain visible while leaked internal verdict envelopes are still
  hidden.
