- **Session recaps retain prompt identities.** Public prompt facts include the caller's message
  identity when the journal recorded one, allowing hosts to reconnect restored prompts to their
  workspace checkpoints.

  Migration: Rust consumers constructing `RecapTextFact` literals must provide `message_id`.
  Use the recorded caller identity for a user prompt, or `None` when it is unavailable or the
  text is from an assistant. Regenerate strict protocol bindings before consuming the new field.
